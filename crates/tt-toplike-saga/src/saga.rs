//! The front door: [`Saga`] takes bus records in order and keeps the whole
//! story (spec sections 3, 4 and 5).
//!
//! A host feeds every record to [`Saga::feed`], reads the state with
//! [`Saga::state`], and publishes what [`Saga::drain_events`] returns. The
//! saga wires the earlier modules together:
//!
//! ```text
//! Record ─► Feed ─► Observation ─┬─► SeedPool (the world seed's jitter)
//!                                ├─► TrailBook (chip statistics for vitality)
//!                                ├─► chip readings ─► FloorSpecs
//!                                └─► Lifecycle ─► world events
//!                                                   │
//!                    party, quests and chronicle ◄──┘ ─► SagaEvents
//! ```
//!
//! # One observation, in order
//!
//! 1. The observation goes into the [`SeedPool`].
//! 2. A sample on a chip (or on a chip's tile, DRAM channel or Ethernet port)
//!    records the chip as seen, while fewer than [`MAX_CHIPS`] chips are kept
//!    (a chip beyond the cap stops here). A topology reading
//!    (`toplike.harvesting_state`, `toplike.enabled_tensix_col`,
//!    `toplike.eth.live_mask`) updates that chip's floor spec, and the live
//!    mask also goes into the link table ([`Links`]). A sample on
//!    the chip itself goes into the [`TrailBook`]: temperatures through
//!    [`push_temperature`], so the baseline series exists (ledger ruling R15),
//!    and every other number as it is (the vitality inputs through
//!    [`TrailBook::push_reserved`], see "Bounds"). A `toplike.heartbeat` value that
//!    differs from the last one marks the chip's heartbeat as advanced at
//!    this tick.
//! 3. An event updates the list of ready models (a `ModelReady` adds one
//!    while fewer than [`MAX_READY_MODELS`] are listed, a `ModelGone` removes
//!    it) and the link table (a `LinkChanged` on a kept chip, a `LinkPeer`;
//!    see "Links"). A tick counts one more tick.
//! 4. On a tick, every chip that has ever reported a heartbeat gets one push
//!    of the [`HEARTBEAT_ALIVE`] series: 1.0 if its heartbeat advanced within
//!    the last [`HEARTBEAT_GRACE_TICKS`] ticks (the tick of the change and
//!    the 19 after it), else 0.0 (ruling R15).
//! 5. The [`Lifecycle`] steps. Each world it ends or births becomes events
//!    and chronicle beats (below).
//! 6. On a tick in a world that was not born by this observation, each
//!    member of the party acts, in index order (below).
//!
//! # Floors
//!
//! The front door builds one [`FloorSpec`] for each chip entity it has seen,
//! in chip key order, from that chip's latest topology readings. The
//! architecture is a guess, since the bus carries no architecture signal:
//! a chip is Blackhole when its key contains `blackhole` (any case) or when it
//! has reported `toplike.enabled_tensix_col`, a register that only Blackhole
//! telemetry carries (`src/models/telemetry.rs` in the root crate says so).
//! Every other chip is [`Arch::Generic`]. A world takes its floors from the
//! specs at its birth and keeps them for its life. Each portal of a floor
//! belongs to one Ethernet port, a live port of the chip's mask at the
//! world's birth ([`FloorSpec::portal_ports`], [`maze::Floor::portals`]).
//!
//! # Links
//!
//! The saga keeps a link table ([`Saga::links`], [`crate::links`]): each
//! kept chip's latest live mask, from its `toplike.eth.live_mask` samples
//! and its `LinkChanged` events, and every Ethernet peer a `LinkPeer` names.
//! A `LinkChanged` carries the mask's low 16 bits, so it replaces those bits
//! and keeps the rest. A `LinkPeer` goes into the table even when its chips
//! have not been sampled yet, within the table's own cap of [`MAX_CHIPS`]
//! chips: the bus source writes a chip's peers right after that chip's own
//! readings, before the next chip's first one. For a world, a portal is
//! bound when its port's peer has a portal on another floor of that world,
//! and open while both ends are live ([`Links::open`]). No rule of the story
//! reads the table yet (the crossing quest will), so it moves no event and
//! is not part of the digest.
//!
//! # Worlds, the party and quests
//!
//! A world has one floor per chip (at most [`crate::world::MAX_FLOORS`]),
//! and its party has one member per floor: member `i` is the hero born on
//! floor `i` (the party addendum, section 3.1). The member's index is its
//! identity for the world's life. A one-chip world has a party of one, and
//! everything below is then the saga of 0.16, as long as every `ModelReady`
//! names that chip or names none. In 0.16 a model that named a chip never
//! sampled got a dungeon on that chip's generic floor. Now its dungeon is
//! on the sampled chip, and its sage stands on no floor.
//!
//! When a world is born:
//!
//! * the chronicle's generator is seeded from the world seed on
//!   [`CHRONICLE_STREAM`]. The chronicle alone draws from it (ruling R16):
//!   generated names draw more from it than real names do, and that must
//!   not change a maze or a walk. All members share it, because the
//!   chronicle is one log and its draws come in beat order;
//! * each member gets its own plan generator, seeded from the world seed on
//!   [`plan_stream`] of its index: [`PLAN_STREAM`] for member 0, as before
//!   the party, and [`PARTY_STREAM_BASE`] plus `i` for member `i` (the
//!   addendum, section 4.2). It names the member and plans its quests, so
//!   one member's extra draw never changes another member's walk;
//! * each member gets its own hooks: one visit-the-sage hook per ready model
//!   whose sage stands on the member's floor (the model named the floor's
//!   chip, or named no chips), in model key order, then one explore hook and
//!   one rest hook whose cause is the world's birth;
//! * member `i` is born at floor `i`'s entrance, named by
//!   [`Pack::hero_name_unique`] from its generator against the names of the
//!   members before it, and plans its first quest.
//!
//! The events of a birth come in this order: `WorldBorn` and its `Beat`;
//! in a party of two or more, one `PartyBorn` `Beat` for the whole party (a
//! world's line, with no floor, whose `{count}` is the party's size); then
//! for each member in index order its `HeroBorn` (with a beat only in a
//! party of one); then for each member its `QuestStarted` and its `Beat`,
//! when the pace writes that line (see "Pace"). An end comes before the
//! birth that follows it: `WorldEnded`, its `Beat`.
//!
//! On each tick in a world, each member in index order reads its vitality
//! from the trails of its floor's chip and takes one step. A member's step
//! reads only its own floor, quest, hooks and generator. Then:
//!
//! * **Arrived.** `QuestDone` and, when the pace writes it, a beat. A sage
//!   hook is dropped, for this
//!   member only, once it has visited the sage. The member then waits
//!   [`DWELL_TICKS`] ticks and plans its next quest, so the chronicle does
//!   not fill with arrivals.
//! * **A model goes away.** Its sage hooks leave every member of the current
//!   world, whichever signal made that world, and every member on its way
//!   to that sage plans again at once, in index order.
//! * **No path.** The hook behind the quest is dropped for this member, so
//!   it does not retry one unreachable goal forever, and it plans again at
//!   once (ruling R15). The explore hook is never dropped: each plan draws a
//!   new room, so the next one may be reachable.
//!
//! # Pace
//!
//! A party of 32 heroes, each starting and finishing quests, would give the
//! chronicle one or two lines a tick, too fast to read. So a member's
//! routine lines share one budget (the party addendum, section 3.5). The
//! routine kinds are `QuestStarted`, `QuestDone`, `Rest` and `Ambient`; a
//! world's line (born, ended, the party's birth) and a hero's birth line are
//! notable and always written.
//!
//! * Ticks fall into windows of [`ROUTINE_WINDOW_TICKS`] (10, one second at
//!   10 Hz): window `k` is ticks `10k` to `10k + 9`.
//! * The first floor to write a routine line in a window owns it. Its
//!   routine lines in that window are written; another floor's are not.
//!   A world's birth opens the window again, so the old world's owner does
//!   not carry over into the new party.
//! * One addition to the addendum's rule keeps every floor heard. A member
//!   is overdue when its last written routine line, or its birth, is at
//!   least [`OVERDUE_TICKS`] (300) ticks old. While any member is overdue, a
//!   window with no owner goes only to an overdue member, and other lines in
//!   it are not written until one takes it. Over the 32-chip party log every
//!   floor is then heard at least once a minute.
//! * A line that is not written still raises its event, so tensixinet and
//!   the digest see every quest. The member's [`Member::unwritten`] counts
//!   one more. In generated mode a `QuestStarted` with no line takes its
//!   reason from [`Chronicle::clean_cause`] (see "Names").
//!
//! The pace reads only ticks and the order of lines, so it keeps the
//! determinism contract. A party of one takes every window, since either
//! its member is overdue or no member is, so a one-chip saga writes every
//! line, as 0.16 did.
//!
//! # Tone
//!
//! Each beat's tone is chosen from the machine's state (spec 5). A member's
//! beat (its birth and its quests) uses that member's vitality. A world's
//! beat (born or ended) uses the lowest vitality in the party. With one
//! member both are the hero's vitality.
//!
//! * **Tense** when that vitality is below [`REST_BELOW`], or when the
//!   beat belongs to an observation that ended a world by a reset or by its
//!   model going down (the end's beat and the beats of the world born from
//!   it).
//! * Otherwise **Triumphant** for a quest done, and for the beats of a world
//!   born from a model becoming ready.
//! * Otherwise **Calm**.
//!
//! # Causes
//!
//! A beat's cause is the observation that led to it: a sample's entity and
//! signal; an event's main entity and its kind (`ModelReady`, `Reset`, ...);
//! for a tick, the pacemaker sweep's entity and its first signal. The
//! ordinal is the record's position in the stream ([`Feed::ordinal`]). Quest
//! beats take the quest's reason as their cause.
//!
//! # Names
//!
//! In [`NameMode::Generated`] the chronicle aliases every beat's cause (ruling
//! R17). The saga does the same for `QuestStarted`: its reason is the cause
//! its beat stored, and a target that is the reason's entity takes the same
//! alias. A target that is neither a hardware key nor the reason's entity is
//! left out of the event in generated mode (phase 1 quests have none). Each
//! member's `quest` keeps the real reason in both modes, so the two modes
//! plan and walk alike.
//!
//! A `QuestStarted` whose line the pace leaves unwritten has no stored
//! cause. Its reason then goes through [`Chronicle::clean_cause`], which
//! aliases it the same way, from the same table and the chronicle's own
//! generator.
//!
//! A quest's [`Quest::target_label`] is the name a view prints for its
//! target. The saga fills it when the quest starts, after its line is
//! written or left unwritten: a hardware key's id, or for a sage the name
//! the line writes, through [`Chronicle::label_for`] (the model's real name
//! in real mode, its alias in generated mode). An alias the table already
//! holds costs no draw, so a label taken after a written line leaves the
//! chronicle's generator as it was. Of a member's fields, only the label
//! differs between the name modes.
//!
//! # Labels
//!
//! A mock saga appends "(mock)" to the first chronicle line, and a replay
//! appends "(replay)" (spec 3.9). [`SagaState::label`] carries the label for
//! the view's title.
//!
//! # Bounds
//!
//! Any local process can write to the bus, so a noisy or hostile writer can
//! send any number of chips, signal names and models, of any length up to
//! a bus line's 16 MB (ledger rulings R20, R21 and R22).
//!
//! **Item size.** The [`Feed`] drops, whole, any record that carries an
//! entity key, signal name or event string longer than
//! [`crate::observe::MAX_ITEM_BYTES`] (256 bytes), and counts it in
//! [`Saga::oversized`]. So every key and name the saga stores is at most
//! 256 bytes, and the counts below bound memory as well as items.
//!
//! **Spoofed series.** The feed also drops any sample or sweep from another
//! source that carries a `saga.*` signal, the prefix of the saga's derived
//! vitality series, and counts it in [`Saga::rejected`].
//!
//! A dropped record takes no ordinal, so it leaves the saga and its digest
//! as they were.
//!
//! Every collection the saga keeps has a fixed bound:
//!
//! | Collection | Bound | Where the cap lives |
//! |---|---|---|
//! | chips seen (and their floor specs, heartbeats and topology) | [`MAX_CHIPS`] (64); a further chip feeds the seed pool and nothing else | [`crate::world`] |
//! | a world's chips and floors | [`crate::world::MAX_FLOORS`] (32), the first in key order | [`crate::world`] |
//! | entities with trails | [`MAX_CHIPS`] (64): only kept chips get trails | here |
//! | trails per chip | [`crate::stats::MAX_TRAILS_PER_ENTITY`] (128) names, plus [`crate::stats::RESERVED_TRAILS`] (4) for the vitality inputs | [`crate::stats`] |
//! | ready models | [`MAX_READY_MODELS`] (64) | here |
//! | chips a `ModelReady` names | the first [`crate::world::MAX_NAMED_CHIPS`] (64) entries of its list are read | [`crate::world::named_chips`] |
//! | named chip keys that are not kept chips | [`MAX_CHIPS`] (64), in one set shared by every ready model; first seen wins | here |
//! | party members | one per floor of the world, so at most [`crate::world::MAX_FLOORS`] (32) | here |
//! | the link table | [`MAX_CHIPS`] (64) chips, 64 ports each: at most 4,096 peers | [`crate::links`] |
//! | a member's hooks | [`MAX_READY_MODELS`] sage hooks (they come from the ready list), plus explore and rest | here |
//! | undrained events | [`MAX_PENDING_EVENTS`] (4096); the oldest is dropped and counted in [`Saga::dropped_events`] | here |
//! | chronicle lines | [`CAPACITY`] (500) | [`crate::chronicle`] |
//! | generated names | [`crate::chronicle::ALIAS_LIMIT`] (256) | [`crate::chronicle`] |
//! | seed pool | [`crate::seed::RING_LEN`] (64) values, 512 bytes | [`crate::seed`] |
//! | one floor | [`crate::topology::MAX_COLS`] x [`crate::topology::MAX_ROWS`] (120 x 60) cells | [`crate::topology`] |
//! | a member's path | one floor's cells | [`crate::hero`] |
//! | copies of a world's floors | one: [`World::floors`] is an `Arc<[Floor]>`, so a clone of the state shares it | [`crate::world`] |
//!
//! The scratch buffers (`obs`, `enc`, `world_events`) keep the capacity of
//! the largest record or event seen so far.
//!
//! **Made-up chips.** The caps keep the first chips seen and, in a world,
//! the first in key order, whatever their source. A local writer that sends
//! samples for keys that sort before the real chips (`chip:0` sorts before
//! `chip:0000:01:00.0`) fills the floors before them, and the dungeon then
//! shows chips that do not exist. Memory and cost stay bounded; keeping the
//! backend's chips first is left for a later phase (final review finding
//! M4).
//!
//! The entities the saga holds at once are at most 64 kept chips and 64
//! ready models: 128, under the spec's 500 (spec 9). A world's chips are
//! kept chips (or the `chip:none` placeholder when no chip is kept), so
//! they add none. The chips a ready model named are kept with it, so
//! that a later world can place its sage. A named key that is a kept chip
//! adds no entity. Every other named key goes into one set shared by all
//! ready models, which holds at most [`MAX_CHIPS`] keys (ledger ruling
//! R31). First seen wins: a key is held if the set already holds it or has
//! room. A later key is read but not kept, so a model that names it has no
//! sage on that chip's floor, even if the chip is kept later. A key leaves
//! the set when no ready model names it any more. The link table holds at
//! most [`MAX_CHIPS`] chip keys of its own, which can be chips the saga
//! does not keep (a `LinkPeer` names both ends), and no port keys. So the
//! saga holds at most 128 + 64 + 64 = 256 entity keys, and 288 entities
//! with the 32 heroes.
//!
//! **Cost of one record.** At most one world is born per record: an event
//! births at most once, and a sweep's samples can start one training run,
//! after which its tick finds the run fresh and the world newly active. A
//! birth generates one maze per world chip, so it costs at most 32 mazes of
//! at most 120 x 60 cells. Each member searches at most once per tick (at
//! most 32 searches), and A* expands each cell at most once. Today's floors
//! are 53 x 38 (Blackhole) and 50 x 32 (generic), so the widest birth
//! generates about 64,000 cells. The
//! test-only work counter (`src/cost.rs`) checks this against a budget of a
//! million work units per record. A unit (a maze cell, an A* expansion, a
//! trail push or an event) is not an elementary operation: a maze cell costs
//! roughly 30 of them. Trail pushes grow with the record's own size: one per
//! sweep value, plus one per kept chip on a tick.
//!
//! **Cost with 32 chips.** A box with more than 32 kept chips still gets 32
//! floors and 32 members: the first 32 chips in key order (the party
//! addendum, section 3.7). Every member steps on every tick, and no floor is
//! lazy. A step along a path moves one cell, so it is not counted as a work
//! unit; a member that ends a quest replans with one A* search, which is.
//! On the 32-chip party log (1,300 ticks, about 50,000 records), the worst
//! record is a 32-floor birth at 64,520 units, 6.5% of the budget. That log
//! has the generic and Blackhole floor sizes, about 2,000 cells each. A
//! birth of 32 floors at the grid limit (120 x 60) would cost about 230,000
//! cells, still under the budget. The worst record that is not a birth is
//! about 10,900 units (members replanning at once), and the mean of those
//! records is about 6 units, or about 400 units per tick. The test
//! `a_record_costs_under_a_million_units_with_32_chips` in
//! `src/fuzz_tests.rs` measures these numbers. It checks the budget, and it
//! also holds the log to about 3x its measured steady cost, so a regression
//! in replanning fails it long before the budget would.
//!
//! **Cost of a snapshot.** A world's floors never change during its life,
//! so [`World::floors`] is an `Arc<[Floor]>`. Cloning the state, which the
//! root crate's driver does for each snapshot, copies a pointer to the
//! floors and no cell.
//!
//! # The digest
//!
//! [`Saga::digest`] is FNV-1a 64 over one byte stream, written by hand so
//! that it is the same on every platform and compiler (ledger ruling R19).
//! No `Debug` output is hashed. The stream is a sequence of items, each
//! starting with a one-byte tag. This is version 3 of the encoding
//! ([`DIGEST_VERSION`]; the party addendum, sections 3.6 and 8.6):
//!
//! | Tag | Item | Fields after the tag, in order |
//! |---|---|---|
//! | `0x00` | the encoding version, once, first (hashed by [`Saga::new`]) | version (3) |
//! | `0x01` | `WorldBorn` event | tick, world, biome |
//! | `0x02` | `WorldEnded` event | tick, world, why |
//! | `0x03` | `HeroBorn` event | tick, world, name, floor |
//! | `0x04` | `QuestStarted` event | tick, world, floor, verb, target, reason |
//! | `0x05` | `QuestDone` event | tick, world, floor, verb |
//! | `0x06` | `Beat` event | tick, world, floor option, tone, text, cause |
//! | `0x10` | a world's seed, at its birth (before its `WorldBorn`) | seed |
//! | `0x20` | one chronicle line, oldest first (only in [`Saga::digest`]) | floor option, text |
//! | `0x30` | the end (only in [`Saga::digest`]) | current world seed, member count, then floor, x and y for each member in index order |
//!
//! The end item's count is the party's size: 1 for a one-chip world and
//! before the first world. Version 3 has the items of version 2. Its
//! version byte moved when portals were bound to Ethernet ports, so the
//! digests that task moved are kept apart from the digests the crossing
//! quest will move when it adds its events. Version 1 (0.16.x) had no version
//! item, no floors on quest events, beats or lines, and the hero's floor, x
//! and y in place of the count and triples.
//!
//! Field encodings:
//!
//! * `tick`, `world`, `seed` and ordinals: `u64`, 8 bytes little-endian.
//! * the version, `floor`, the member count, and a member's x and y: one
//!   byte.
//! * strings and entity keys: the UTF-8 length as a `u32`, 4 bytes
//!   little-endian, then the UTF-8 bytes.
//! * an `Option`: one presence byte (0 absent, 1 present), then the value
//!   when present. A floor option is two bytes when present (1, then the
//!   floor) and one byte when absent (0). A world's beat (born or ended)
//!   has no floor; a member's beat (its birth or a quest) has the floor of
//!   that member's hero.
//! * a cause: entity key, signal string, ordinal.
//! * an `f32` (none yet; phase 2 intensities): `to_bits()`, 4 bytes
//!   little-endian.
//! * enums, one byte: biome Dungeon 0, Forge 1, Dream 2; why ModelDown 0,
//!   Reset 1, Season 2, HeroDied 3, Superseded 4, RunEnded 5; verb Explore 0,
//!   Rest 1, VisitSage 2, Slay 3, Escort 4, Cleanse 5, Rebuild 6; tone Calm 0,
//!   Tense 1, Triumphant 2.
//!
//! The tags and codes are part of the golden digests: change one only on
//! purpose, and re-pin. The tick is in every event because the events
//! themselves carry none: without it, a hero that walked one tick slower
//! would give the same digest. Draining events does not change the digest.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use tt_toplike_bus::{EntityKey, Event, Payload, Record};

use crate::chronicle::{
    Beat, BeatCtx, BeatKind, Chronicle, NameMode, NameRole, RealName, Tone, CAPACITY,
};
use crate::event::{Biome, EndWhy, SagaEvent};
use crate::hero::{
    push_temperature, vitality, Cause, Hero, ASIC_TEMPERATURE, HEARTBEAT_ALIVE,
    HEARTBEAT_GRACE_TICKS, VITALITY_SIGNALS,
};
use crate::links::Links;
use crate::maze;
use crate::observe::{Feed, Observation};
use crate::pack::Pack;
use crate::quest::{plan, Hook, Quest, Verb, REST_BELOW};
use crate::rng::Pcg32;
use crate::seed::{fnv1a64, fnv1a64_extend, SeedPool};
use crate::stats::TrailBook;
use crate::topology::{Arch, FloorSpec};
use crate::world::{named_chips, Lifecycle, World, MAX_CHIPS, NO_CHIP};

/// Stream selector of member 0's plan generator, which names it and plans
/// its quests. It is the hero's stream from before the party, so a one-chip
/// world draws as it did.
pub const PLAN_STREAM: u64 = 1;

/// Member `i >= 1` draws from stream `PARTY_STREAM_BASE + i` (the party
/// addendum, section 4.2). 16 leaves room below it for other streams.
pub const PARTY_STREAM_BASE: u64 = 16;

/// The stream selector of member `i`'s plan generator: [`PLAN_STREAM`] for
/// member 0, [`PARTY_STREAM_BASE`] plus `i` for every other member.
pub fn plan_stream(i: usize) -> u64 {
    if i == 0 {
        PLAN_STREAM
    } else {
        PARTY_STREAM_BASE + i as u64
    }
}

/// Stream selector of the chronicle's own generator (ruling R16).
pub const CHRONICLE_STREAM: u64 = 2;

/// Ticks a member waits after finishing a quest before it plans the next
/// one (3 s at 10 Hz).
pub const DWELL_TICKS: u64 = 30;

/// Ticks in one window of the chronicle's pace: 10, one second at 10 Hz
/// (the party addendum, section 3.5). Window `k` is ticks `10k` to
/// `10k + 9`. In each window the routine lines of one floor are written (see
/// "Pace" in the module comment).
pub const ROUTINE_WINDOW_TICKS: u64 = 10;

/// A member whose last written routine line (or its birth) is this many
/// ticks old or older is overdue: 300 ticks, 30 seconds at 10 Hz. While any
/// member is overdue, an open window of the pace goes only to an overdue
/// member (see "Pace" in the module comment). This rule is an addition to
/// the party addendum's first-come rule, which on its own left some floors
/// of a 32-chip party unheard for over a thousand ticks.
pub const OVERDUE_TICKS: u64 = 300;

/// Ready models the saga keeps (ledger ruling R20). A `ModelReady` for a new
/// model while this many are ready is not added to the list; its world is
/// still born. Each member's sage hooks come from this list, so a member has
/// at most this many sage hooks, plus one explore and one rest hook.
pub const MAX_READY_MODELS: usize = 64;

/// Events kept until a host drains them (ledger ruling R20). When the buffer
/// is full the oldest event is dropped and counted in
/// [`Saga::dropped_events`]. A host that drains after each batch never
/// reaches it.
pub const MAX_PENDING_EVENTS: usize = 4096;

// The bus signal names below are copies of the root crate's
// `crate::bus::signals` constants: the saga crate depends only on the bus
// crate, which does not hold them. A root-crate test checks every copy
// (`crate::saga::tests::the_saga_signal_names_are_the_bus_names`), so a
// rename in the bus cannot leave the saga reading a signal that no longer
// exists.

/// The ARC heartbeat counter on a chip.
pub const HEARTBEAT: &str = "toplike.heartbeat";
/// The harvesting state register on a chip.
pub const HARVESTING_STATE: &str = "toplike.harvesting_state";
/// The enabled Tensix column mask on a chip (Blackhole only).
pub const ENABLED_TENSIX_COL: &str = "toplike.enabled_tensix_col";
/// The Ethernet live-link mask on a chip.
pub const ETH_LIVE_MASK: &str = "toplike.eth.live_mask";

/// Where the saga's records come from. It decides the mark on the title and
/// the first chronicle line (spec 3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// Live readings: no mark.
    Live,
    /// `--mock`: every reading is estimated. Marked "(mock)".
    Mock,
    /// A recorded log played back. Marked "(replay)".
    Replay,
}

impl Label {
    /// The mark a view shows next to the title, if any.
    pub fn mark(self) -> Option<&'static str> {
        match self {
            Label::Live => None,
            Label::Mock => Some("(mock)"),
            Label::Replay => Some("(replay)"),
        }
    }
}

/// How a saga is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SagaConfig {
    /// Quiet ticks that end a world (spec 3.5; 12,000 is 20 minutes at
    /// 10 Hz). 0 counts as 1.
    pub season_ticks: u64,
    /// Whether real names appear in the chronicle.
    pub names: NameMode,
    /// Where the records come from.
    pub label: Label,
}

/// One member of the party: the hero born on one floor, and its quest.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub hero: Hero,
    /// The hero's current quest. After an arrival it stays the finished
    /// quest until the hero plans the next one.
    pub quest: Quest,
    /// The member's routine chronicle lines that the pace left unwritten in
    /// the current world (see "Pace" in the module comment). It counts up
    /// from 0 at the member's birth and stops at `u32::MAX`. A member alone
    /// in its party never has a line left unwritten.
    pub unwritten: u32,
}

/// Everything a view draws.
///
/// Before the first world is born (at most five ticks, ledger ruling R11)
/// `world` is a placeholder with id 0: one generic floor for the chip
/// [`NO_CHIP`], seed 0, and a party of one nameless hero resting at its
/// entrance. Every real world has an id of 1 or more.
#[derive(Debug, Clone)]
pub struct SagaState {
    pub label: Label,
    pub world: World,
    /// One member per floor of the world, in floor order: `party[i]` is the
    /// hero born on floor `i`. Never empty.
    pub party: Vec<Member>,
    pub chronicle: Chronicle,
    /// Ticks seen so far (pacemaker sweeps, see [`crate::observe`]).
    pub tick: u64,
    /// The chips the stream has shown the saga so far, at most
    /// [`crate::world::MAX_CHIPS`]. A world keeps at most
    /// [`crate::world::MAX_FLOORS`] of them as floors, so a view can say how
    /// many chips have no floor. It counts the chips of the stream the story
    /// reads, which under a replay or an attached bus is another box's.
    /// Views read it; no rule of the story does, and it is not part of
    /// [`Saga::digest`].
    pub chips_seen: usize,
}

impl SagaState {
    /// Whether a real world has been born. Before the first world (at most
    /// five ticks) the state holds the placeholder world with id 0, and a
    /// view shows a waiting state.
    pub fn has_world(&self) -> bool {
        self.world.id > 0
    }

    /// The lead member: member 0, the hero of floor 0. With one chip it is
    /// the only hero.
    pub fn lead(&self) -> &Member {
        &self.party[0]
    }

    /// The members whose hero stands on `floor`, with their indices, in
    /// index order. In the party stage that is member `floor` alone.
    pub fn on_floor(&self, floor: u8) -> impl Iterator<Item = (usize, &Member)> {
        self.party
            .iter()
            .enumerate()
            .filter(move |(_, m)| m.hero.floor == floor)
    }
}

/// What the core keeps for one member besides its public [`Member`]: index
/// aligned with [`SagaState::party`].
#[derive(Debug, Clone)]
struct MemberCore {
    /// The member's hooks in the current world.
    hooks: Vec<Hook>,
    /// Names the member and plans its quests. Reseeded at each birth on
    /// [`plan_stream`] of the member's index.
    plan_rng: Pcg32,
    /// The tick at which the member plans its next quest after an arrival.
    dwell_until: Option<u64>,
    /// The tick of the member's last written routine line, or of its birth
    /// before its first (see "Pace" in the module comment).
    heard: u64,
}

/// A ready model: the cause of its `ModelReady` and the chips it named.
#[derive(Debug, Clone)]
struct Ready {
    cause: Cause,
    /// The model's `ModelReady` named no chips, so its sage stands on every
    /// floor.
    anywhere: bool,
    /// The chips the model named that the saga holds, sorted, without
    /// repeats (see "Bounds" in the module comment): each is a kept chip or
    /// a key in `Saga::named_unseen`.
    chips: Vec<EntityKey>,
}

impl Ready {
    /// Whether this model's sage stands on the floor of `chip`.
    fn stands_on(&self, chip: &EntityKey) -> bool {
        self.anywhere || self.chips.binary_search(chip).is_ok()
    }
}

/// What the saga has seen of one chip.
#[derive(Debug, Clone, Default)]
struct ChipSeen {
    harvesting: Option<u64>,
    tensix_cols: Option<u64>,
    eth_live_mask: Option<u64>,
    /// The last heartbeat value and the tick at which it last changed.
    heartbeat: Option<(f64, u64)>,
}

/// The saga core. See the module comment.
#[derive(Debug, Clone)]
pub struct Saga {
    cfg: SagaConfig,
    feed: Feed,
    seeds: SeedPool,
    life: Lifecycle,
    trails: TrailBook,
    /// Every chip seen, in key order.
    chips: BTreeMap<EntityKey, ChipSeen>,
    /// One spec per chip in `chips`, rebuilt when `topo_dirty` is set.
    topo: Vec<FloorSpec>,
    topo_dirty: bool,
    /// Ready models, the cause of their `ModelReady` and their chips.
    ready: BTreeMap<EntityKey, Ready>,
    /// Live masks and Ethernet peers (see "Links" in the module comment).
    links: Links,
    /// The chip keys ready models named that are not kept chips, shared by
    /// every ready model: at most [`MAX_CHIPS`] (see "Bounds").
    named_unseen: BTreeSet<EntityKey>,
    /// One per member of the party, index aligned with `state.party`.
    members: Vec<MemberCore>,
    /// The chronicle's own stream, shared by every member. Reseeded at each
    /// birth.
    story_rng: Pcg32,
    /// The window of the pace that last had an owner, and the floor that
    /// owns it: `(k, floor)` for ticks `10k` to `10k + 9` (see "Pace" in
    /// the module comment). A window other than the current tick's has no
    /// owner yet. A world's birth clears it.
    window_owner: Option<(u64, u8)>,
    /// Whether the first chronicle line has been written (and marked).
    wrote_first: bool,
    /// Events raised and not yet drained, oldest first, at most
    /// [`MAX_PENDING_EVENTS`].
    events: VecDeque<SagaEvent>,
    /// Events dropped from a full buffer before they were drained.
    dropped_events: u64,
    /// The FNV-1a 64 state after the digest stream so far: the version
    /// item, world seeds and events (see "The digest" in the module
    /// comment).
    trace: u64,
    /// Scratch buffer for the digest's encoding.
    enc: Vec<u8>,
    /// Scratch buffers, kept to avoid an allocation per record.
    obs: Vec<Observation>,
    world_events: Vec<SagaEvent>,
    state: SagaState,
    /// Test only: the member whose sage is walled in at every birth (see
    /// `Saga::with_walled_sage`).
    #[cfg(test)]
    walled_sage: Option<usize>,
}

/// What one observation's beats share: where they came from and how they
/// feel.
struct Moment {
    cause: Cause,
    /// The observation ended a world by a reset or its model going down.
    collapse: bool,
    /// The model whose `ModelReady` this observation is, if it is one.
    ready_model: Option<EntityKey>,
}

impl Saga {
    /// A saga that has seen nothing. Its digest stream starts with the
    /// version item ([`DIGEST_VERSION`]).
    pub fn new(cfg: SagaConfig) -> Saga {
        let chip = EntityKey::chip(NO_CHIP);
        let floor = maze::generate(&FloorSpec::generic(&chip), 0, 0.0);
        let pos = floor.entrance;
        let placeholder = World {
            id: 0,
            seed: 0,
            born_tick: 0,
            chips: vec![chip.clone()],
            floors: Arc::from([floor]),
            biome: Biome::Dungeon,
            sage_chips: Vec::new(),
        };
        Saga {
            cfg,
            feed: Feed::new(),
            seeds: SeedPool::new(),
            life: Lifecycle::new(cfg.season_ticks),
            trails: TrailBook::new(),
            chips: BTreeMap::new(),
            topo: Vec::new(),
            topo_dirty: false,
            ready: BTreeMap::new(),
            links: Links::new(),
            named_unseen: BTreeSet::new(),
            members: vec![MemberCore {
                hooks: Vec::new(),
                plan_rng: Pcg32::new(0, PLAN_STREAM),
                dwell_until: None,
                heard: 0,
            }],
            story_rng: Pcg32::new(0, CHRONICLE_STREAM),
            window_owner: None,
            wrote_first: false,
            events: VecDeque::new(),
            dropped_events: 0,
            // The version item goes in first, so a saga that has seen
            // nothing already has a version 3 digest.
            trace: fnv1a64(&[TAG_VERSION, DIGEST_VERSION]),
            enc: Vec::new(),
            obs: Vec::new(),
            world_events: Vec::new(),
            state: SagaState {
                label: cfg.label,
                world: placeholder,
                party: vec![Member {
                    hero: Hero::new(String::new(), 0, pos, 0),
                    quest: rest_at(pos, chip),
                    unwritten: 0,
                }],
                chronicle: Chronicle::new(),
                tick: 0,
                chips_seen: 0,
            },
            #[cfg(test)]
            walled_sage: None,
        }
    }

    /// Takes in one record. Records from source `saga` are skipped (see
    /// [`Feed`]). Only the record's source and payload are read.
    pub fn feed(&mut self, rec: &Record) {
        let mut obs = std::mem::take(&mut self.obs);
        obs.clear();
        self.feed.push(rec, &mut obs);
        let ordinal = self.feed.ordinal();
        for o in &obs {
            self.observe(o, rec, ordinal);
        }
        self.obs = obs;
    }

    /// The state for a view to draw.
    pub fn state(&self) -> &SagaState {
        &self.state
    }

    /// The link table: every kept chip's live mask and every Ethernet peer
    /// the bus has named (see "Links" in the module comment).
    pub fn links(&self) -> &Links {
        &self.links
    }

    /// The events raised since the last call, oldest first. The saga keeps
    /// at most [`MAX_PENDING_EVENTS`] undrained events, so a host drains
    /// after each batch of records it feeds.
    pub fn drain_events(&mut self) -> Vec<SagaEvent> {
        self.events.drain(..).collect()
    }

    /// Records dropped because they carry an entity key, signal name or event
    /// string longer than [`crate::observe::MAX_ITEM_BYTES`] (ledger ruling
    /// R21). A dropped record changes nothing else, including the digest.
    pub fn oversized(&self) -> u64 {
        self.feed.oversized()
    }

    /// Records dropped because a source other than "saga" sent a signal
    /// named `saga.*`, the prefix of the saga's own derived series. A
    /// dropped record changes nothing else, including the digest.
    pub fn rejected(&self) -> u64 {
        self.feed.rejected()
    }

    /// Events dropped, oldest first, because the buffer already held
    /// [`MAX_PENDING_EVENTS`] undrained events. It counts up for the saga's
    /// life. Dropped events still count in the digest.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// A hash of everything the saga has told: every world seed, every event
    /// raised (drained or not), and the chronicle's text. See the module
    /// comment.
    pub fn digest(&self) -> u64 {
        let mut h = self.trace;
        let mut buf = Vec::new();
        for beat in self.state.chronicle.lines(CAPACITY) {
            buf.clear();
            encode_line(&mut buf, beat);
            h = fnv1a64_extend(h, &buf);
        }
        buf.clear();
        let members: Vec<(u8, u8, u8)> = self
            .state
            .party
            .iter()
            .map(|m| (m.hero.floor, m.hero.pos.0, m.hero.pos.1))
            .collect();
        encode_end(&mut buf, self.state.world.seed, &members);
        fnv1a64_extend(h, &buf)
    }

    /// One observation, in the order the module comment gives.
    fn observe(&mut self, o: &Observation, rec: &Record, ordinal: u64) {
        self.seeds.push(o);
        match o {
            Observation::Sample {
                entity,
                signal,
                value,
            } => self.on_sample(entity, signal, *value, ordinal),
            Observation::Event(e) => self.on_event(e, ordinal),
            Observation::Tick => {
                self.state.tick += 1;
                self.push_heartbeats();
            }
        }
        if self.topo_dirty {
            self.rebuild_topology();
        }

        let mut wev = std::mem::take(&mut self.world_events);
        wev.clear();
        self.life.step(
            o,
            self.state.tick,
            &self.seeds,
            ordinal,
            &self.topo,
            &mut wev,
        );

        let mut born = false;
        if !wev.is_empty() {
            let mut moment = Moment {
                cause: self.cause_of(o, rec, ordinal),
                collapse: false,
                ready_model: match o {
                    Observation::Event(Event::ModelReady { model, .. }) => Some(model.clone()),
                    _ => None,
                },
            };
            for ev in wev.drain(..) {
                match ev {
                    SagaEvent::WorldEnded { world, why } => {
                        moment.collapse |= matches!(why, EndWhy::Reset | EndWhy::ModelDown);
                        self.on_world_ended(world, why, &moment);
                    }
                    SagaEvent::WorldBorn { .. } => {
                        self.on_world_born(&moment);
                        born = true;
                    }
                    // The lifecycle raises only world events.
                    other => self.emit(other),
                }
            }
        }
        self.world_events = wev;

        // Every member walking to a sage that just went away plans again at
        // once, in index order. When the model's own dungeon ended, the new
        // world already planned.
        if let Observation::Event(Event::ModelGone { model }) = o {
            if !born && self.state.world.id > 0 {
                for i in 0..self.state.party.len() {
                    if self.state.party[i].quest.target.as_ref() == Some(model) {
                        self.members[i].dwell_until = None;
                        self.start_quest(i, false);
                    }
                }
            }
        }

        if matches!(o, Observation::Tick) && !born && self.state.world.id > 0 {
            for i in 0..self.state.party.len() {
                self.member_tick(i);
            }
        }
    }

    /// A sample: chip readings, the link table's masks, trails and
    /// heartbeats.
    fn on_sample(&mut self, entity: &EntityKey, signal: &str, value: f64, ordinal: u64) {
        let Some(chip) = entity.chip_owner() else {
            return;
        };
        let on_chip = &chip == entity;
        let tick = self.state.tick;
        // A new chip changes the topology. The key is cloned only then. A
        // chip beyond MAX_CHIPS has fed the seed pool already and gets
        // nothing more: no floor, no trails, no heartbeat.
        if !self.chips.contains_key(&chip) {
            if self.chips.len() >= MAX_CHIPS {
                return;
            }
            self.topo_dirty = true;
            self.chips.insert(chip.clone(), ChipSeen::default());
            // For views only: the digest never reads it.
            self.state.chips_seen = self.chips.len();
        }
        let Some(seen) = self.chips.get_mut(&chip) else {
            return;
        };
        if !on_chip {
            return;
        }
        // Registers arrive as whole numbers; the cast is exact up to 2^53
        // and saturates beyond, which only matters for bits the floor
        // ignores.
        let bits = value as u64;
        let changed = |slot: &mut Option<u64>| {
            let new = Some(bits);
            let differs = *slot != new;
            *slot = new;
            differs
        };
        match signal {
            HARVESTING_STATE => self.topo_dirty |= changed(&mut seen.harvesting),
            ENABLED_TENSIX_COL => self.topo_dirty |= changed(&mut seen.tensix_cols),
            ETH_LIVE_MASK => {
                self.topo_dirty |= changed(&mut seen.eth_live_mask);
                let cause = Cause {
                    entity: chip.clone(),
                    signal: ETH_LIVE_MASK.to_string(),
                    ordinal,
                };
                self.links.observe_mask(&chip, bits, cause);
            }
            HEARTBEAT => {
                let advanced = seen.heartbeat.is_none_or(|(last, _)| last != value);
                if advanced {
                    seen.heartbeat = Some((value, tick));
                }
            }
            _ => {}
        }
        if signal == ASIC_TEMPERATURE {
            push_temperature(&mut self.trails, entity, value);
        } else if VITALITY_SIGNALS.contains(&signal) {
            self.trails.push_reserved(entity, signal, value);
        } else {
            self.trails.push(entity, signal, value);
        }
    }

    /// An event: the list of ready models, and the link table.
    fn on_event(&mut self, e: &Event, ordinal: u64) {
        match e {
            Event::LinkChanged { chip, mask } => {
                // Only a kept chip's mask is kept, as with the sample. The
                // event carries the low 16 bits: they replace the same
                // bits of the mask, and the rest stays.
                if self.chips.contains_key(chip) {
                    let old = self.links.mask(chip).unwrap_or(0);
                    let merged = (old & !0xffff) | u64::from(*mask);
                    let cause = Cause {
                        entity: chip.clone(),
                        signal: "LinkChanged".to_string(),
                        ordinal,
                    };
                    self.links.observe_mask(chip, merged, cause);
                }
            }
            Event::LinkPeer { port, peer } => {
                let cause = Cause {
                    entity: port.clone(),
                    signal: "LinkPeer".to_string(),
                    ordinal,
                };
                self.links.observe_peer(port, peer, cause);
            }
            Event::ModelReady { model, chips } => {
                // A model already listed takes the new cause and chips; a
                // new one is listed only below MAX_READY_MODELS.
                if !self.ready.contains_key(model) && self.ready.len() >= MAX_READY_MODELS {
                    return;
                }
                let cause = Cause {
                    entity: model.clone(),
                    signal: "ModelReady".to_string(),
                    ordinal,
                };
                // The model's earlier chips leave the shared set first, so
                // a model that is ready again is placed by its new list.
                if self.ready.remove(model).is_some() {
                    self.rebuild_named_unseen();
                }
                // A kept chip is always held. Another key is held while the
                // shared set has room or already holds it; first seen wins,
                // and a later key is read but not kept (see "Bounds").
                let mut held = Vec::new();
                for chip in named_chips(chips) {
                    if self.chips.contains_key(&chip) || self.named_unseen.contains(&chip) {
                        held.push(chip);
                    } else if self.named_unseen.len() < MAX_CHIPS {
                        self.named_unseen.insert(chip.clone());
                        held.push(chip);
                    }
                }
                let ready = Ready {
                    cause,
                    anywhere: chips.is_empty(),
                    chips: held,
                };
                self.ready.insert(model.clone(), ready);
            }
            Event::ModelGone { model } => {
                // The model's sage leaves every member of every world,
                // including a world the model did not make (ruling R14
                // keeps such a world alive).
                if self.ready.remove(model).is_some() {
                    self.rebuild_named_unseen();
                }
                for core in &mut self.members {
                    core.hooks.retain(|h| h.target.as_ref() != Some(model));
                }
            }
            _ => {}
        }
    }

    /// Rebuilds the shared set of named keys that are not kept chips from
    /// the ready models' lists. It holds only keys the set held before (a
    /// key that became a kept chip leaves it), so it stays within
    /// [`MAX_CHIPS`].
    fn rebuild_named_unseen(&mut self) {
        let chips = &self.chips;
        self.named_unseen = self
            .ready
            .values()
            .flat_map(|r| &r.chips)
            .filter(|c| !chips.contains_key(*c))
            .cloned()
            .collect();
    }

    /// One [`HEARTBEAT_ALIVE`] push per chip that has reported a heartbeat.
    fn push_heartbeats(&mut self) {
        let tick = self.state.tick;
        for (chip, seen) in &self.chips {
            if let Some((_, changed)) = seen.heartbeat {
                let alive = tick.saturating_sub(changed) < HEARTBEAT_GRACE_TICKS;
                self.trails
                    .push_reserved(chip, HEARTBEAT_ALIVE, if alive { 1.0 } else { 0.0 });
            }
        }
    }

    /// One spec per seen chip, in key order. See "Floors" in the module
    /// comment for the architecture guess.
    fn rebuild_topology(&mut self) {
        self.topo = self
            .chips
            .iter()
            .map(|(chip, seen)| {
                let blackhole = seen.tensix_cols.is_some()
                    || chip.as_str().to_lowercase().contains("blackhole");
                let arch = if blackhole {
                    Arch::Blackhole
                } else {
                    Arch::Generic
                };
                FloorSpec::for_chip(
                    chip,
                    seen.harvesting,
                    seen.tensix_cols,
                    seen.eth_live_mask,
                    arch,
                )
            })
            .collect();
        self.topo_dirty = false;
    }

    /// The cause of an observation. See "Causes" in the module comment.
    fn cause_of(&self, o: &Observation, rec: &Record, ordinal: u64) -> Cause {
        let fallback = || {
            self.state
                .world
                .chips
                .first()
                .cloned()
                .unwrap_or_else(|| EntityKey::chip(NO_CHIP))
        };
        let (entity, signal) = match o {
            Observation::Sample { entity, signal, .. } => (entity.clone(), signal.clone()),
            Observation::Event(e) => {
                let (entity, kind) = event_cause(e);
                (entity.unwrap_or_else(fallback), kind.to_string())
            }
            Observation::Tick => match &rec.payload {
                Payload::Sweep(s) => (
                    s.entity.clone(),
                    s.signals.first().cloned().unwrap_or_default(),
                ),
                _ => (fallback(), String::new()),
            },
        };
        Cause {
            entity,
            signal,
            ordinal,
        }
    }

    /// The current world ended: its event and beat.
    fn on_world_ended(&mut self, world: u64, why: EndWhy, moment: &Moment) {
        self.emit(SagaEvent::WorldEnded { world, why });
        let tone = self.tone(moment.collapse, false, self.lowest_vitality());
        // A world's beat: no floor.
        let ctx = self.ctx(
            None,
            tone,
            None,
            NameRole::Service,
            None,
            Some(why),
            &moment.cause,
        );
        self.beat(BeatKind::WorldEnded, &ctx);
    }

    /// A world was born: take it from the lifecycle, reseed, make the party
    /// and each member's first quest. See the module comment for the event
    /// order.
    fn on_world_born(&mut self, moment: &Moment) {
        let Some(world) = self.life.world().cloned() else {
            return;
        };
        let mut buf = std::mem::take(&mut self.enc);
        buf.clear();
        buf.push(TAG_SEED);
        put_u64(&mut buf, world.seed);
        self.trace = fnv1a64_extend(self.trace, &buf);
        self.enc = buf;
        self.story_rng = Pcg32::new(world.seed, CHRONICLE_STREAM);

        // The party: one member per floor, in floor order. A world has at
        // least one floor and at most MAX_FLOORS, so the party is never
        // empty and every index fits a byte.
        self.state.party.clear();
        self.members.clear();
        // A new party starts with the window open: a floor of the old
        // world that owned the current window does not silence the new
        // party for the rest of it.
        self.window_owner = None;
        let mut names: Vec<String> = Vec::with_capacity(world.floors.len());
        for (i, floor) in world.floors.iter().enumerate() {
            let chip = &world.chips[i];
            let mut plan_rng = Pcg32::new(world.seed, plan_stream(i));
            // Hooks: the sages standing on this floor first, then explore
            // and rest.
            let mut hooks: Vec<Hook> = self
                .ready
                .iter()
                .filter(|(_, r)| r.stands_on(chip))
                .map(|(model, r)| Hook {
                    verb: Verb::VisitSage,
                    target: Some(model.clone()),
                    reason: r.cause.clone(),
                })
                .collect();
            for verb in [Verb::Explore, Verb::Rest] {
                hooks.push(Hook {
                    verb,
                    target: None,
                    reason: moment.cause.clone(),
                });
            }
            let name = Pack::builtin().hero_name_unique(&mut plan_rng, &names);
            names.push(name.clone());
            let hero = Hero::new(name, i as u8, floor.entrance, world.born_tick);
            // A placeholder until the member plans its first quest below.
            let quest = rest_at(floor.entrance, chip.clone());
            self.state.party.push(Member {
                hero,
                quest,
                unwritten: 0,
            });
            self.members.push(MemberCore {
                hooks,
                plan_rng,
                dwell_until: None,
                heard: self.state.tick,
            });
        }
        let (id, biome) = (world.id, world.biome);
        self.state.world = world;
        // The sage's floors are the floors whose member got the model's
        // hook: the lifecycle's list, read again through the saga's own
        // list of the model's chips, which can hold fewer (see "Bounds").
        // A model that is not on the ready list (it was full) has no sage.
        let world = &mut self.state.world;
        world.sage_chips = match self.life.model().and_then(|m| self.ready.get(m)) {
            Some(r) => world
                .chips
                .iter()
                .filter(|c| r.stands_on(c))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        #[cfg(test)]
        self.wall_test_sage();

        let triumph = moment.ready_model.is_some();
        let tone = self.tone(moment.collapse, triumph, self.lowest_vitality());
        let model_name = moment.ready_model.as_ref().and_then(real_name);
        let role = if model_name.is_some() {
            NameRole::Model
        } else {
            NameRole::Service
        };

        self.emit(SagaEvent::WorldBorn { world: id, biome });
        // A world's beat: no floor.
        let ctx = self.ctx(None, tone, model_name, role, None, None, &moment.cause);
        self.beat(BeatKind::WorldBorn, &ctx);

        // A party of two or more: one line for the whole party, a world's
        // line with no floor, in the world's tone. It names no model, so it
        // draws no alias.
        let alone = self.state.party.len() == 1;
        if !alone {
            let ctx = self.ctx(
                None,
                tone,
                None,
                NameRole::Service,
                None,
                None,
                &moment.cause,
            );
            self.beat(BeatKind::PartyBorn, &ctx);
        }

        for i in 0..self.state.party.len() {
            let hero = &self.state.party[i].hero;
            let (name, floor, vitality) = (hero.name.clone(), hero.floor, hero.vitality);
            self.emit(SagaEvent::HeroBorn {
                world: id,
                name,
                floor,
            });
            // A hero's beat, on its floor, only in a party of one. A larger
            // party's birth has its single party line above.
            if alone {
                let tone = self.tone(moment.collapse, triumph, vitality);
                let ctx = self.ctx(
                    Some(i),
                    tone,
                    None,
                    NameRole::Service,
                    None,
                    None,
                    &moment.cause,
                );
                self.beat(BeatKind::HeroBorn, &ctx);
            }
        }

        for i in 0..self.state.party.len() {
            self.start_quest(i, moment.collapse);
        }
    }

    /// Plans member `i`'s next quest and raises `QuestStarted` and its beat.
    fn start_quest(&mut self, i: usize, collapse: bool) {
        let core = &mut self.members[i];
        let quest = plan(
            &self.state.world,
            &self.state.party[i].hero,
            &core.hooks,
            &mut core.plan_rng,
        );
        let name = match quest.verb {
            Verb::VisitSage => quest.target.as_ref().and_then(real_name),
            _ => None,
        };
        let role = if name.is_some() {
            NameRole::Model
        } else {
            NameRole::Service
        };
        let tone = self.tone(collapse, false, self.state.party[i].hero.vitality);
        let ctx = self.ctx(
            Some(i),
            tone,
            name,
            role,
            Some(quest.verb),
            None,
            &quest.reason,
        );
        let stored = self.routine_beat(i, BeatKind::QuestStarted, &ctx);

        // In generated mode the event takes the beat's cleaned cause, or for
        // a line the pace left unwritten the cause cleaned the same way (see
        // "Names" in the module comment).
        let (reason, target) = match self.cfg.names {
            NameMode::Real => (quest.reason.clone(), quest.target.clone()),
            NameMode::Generated => {
                let cause = match &stored {
                    Some(SagaEvent::Beat { cause, .. }) => cause.clone(),
                    _ => self.state.chronicle.clean_cause(
                        &quest.reason,
                        self.cfg.names,
                        &mut self.story_rng,
                    ),
                };
                let target = quest.target.as_ref().and_then(|t| {
                    if t == &quest.reason.entity {
                        Some(cause.entity.clone())
                    } else if t.chip_owner().is_some() {
                        Some(t.clone())
                    } else {
                        None
                    }
                });
                (cause, target)
            }
        };
        self.emit(SagaEvent::QuestStarted {
            world: self.state.world.id,
            floor: self.state.party[i].hero.floor,
            verb: quest.verb,
            target,
            reason,
        });
        if let Some(stored) = stored {
            self.emit(stored);
        }
        let mut quest = quest;
        quest.target_label = self.target_label(&quest);
        self.state.party[i].quest = quest;
    }

    /// The name a view prints for `quest`'s target, taken after the
    /// quest's line was written or left unwritten (see "Names" in the module
    /// comment):
    ///
    /// * no target: `None`;
    /// * a hardware key (a chip, tile, DRAM channel or Ethernet port): its
    ///   id, which the chronicle never aliases;
    /// * the reason's entity (a sage's model): the name its line writes,
    ///   through [`Chronicle::label_for`], so the alias in generated mode;
    /// * anything else, or an id that is not a single token: `None`.
    fn target_label(&mut self, quest: &Quest) -> Option<String> {
        let target = quest.target.as_ref()?;
        if target.chip_owner().is_some() {
            let id = target.as_str().split_once(':').map_or("", |(_, id)| id);
            return Some(id.to_string());
        }
        if *target != quest.reason.entity {
            return None;
        }
        let name = real_name(target)?;
        Some(self.state.chronicle.label_for(
            &name,
            NameRole::Model,
            self.cfg.names,
            &mut self.story_rng,
        ))
    }

    /// Member `i`'s tick: vitality, one step, and what follows it.
    fn member_tick(&mut self, i: usize) {
        // Vitality first, so it is fresh while the member dwells and when
        // it plans after dwelling.
        let floor_at = self.state.party[i].hero.floor as usize;
        if let Some(chip) = self.state.world.chips.get(floor_at) {
            self.state.party[i].hero.vitality = vitality(&self.trails, chip);
        }
        if let Some(until) = self.members[i].dwell_until {
            if self.state.tick < until {
                return;
            }
            self.members[i].dwell_until = None;
            self.start_quest(i, false);
            return;
        }
        let state = &mut self.state;
        let Some(floor) = state.world.floors.get(floor_at) else {
            return;
        };
        let member = &mut state.party[i];
        let r = member.hero.step(floor, &member.quest);
        if r.arrived {
            let quest = member.quest.clone();
            let (vitality, on) = (member.hero.vitality, member.hero.floor);
            if quest.verb == Verb::VisitSage {
                self.drop_hook(i, &quest);
            }
            self.emit(SagaEvent::QuestDone {
                world: self.state.world.id,
                floor: on,
                verb: quest.verb,
            });
            let tone = self.tone(false, true, vitality);
            let ctx = self.ctx(
                Some(i),
                tone,
                None,
                NameRole::Service,
                Some(quest.verb),
                None,
                &quest.reason,
            );
            if let Some(ev) = self.routine_beat(i, BeatKind::QuestDone, &ctx) {
                self.emit(ev);
            }
            self.members[i].dwell_until = Some(self.state.tick + DWELL_TICKS);
        } else if r.no_path {
            let quest = member.quest.clone();
            if quest.verb != Verb::Explore {
                self.drop_hook(i, &quest);
            }
            self.start_quest(i, false);
        }
    }

    /// Removes the hook member `i`'s quest came from, for that member only.
    fn drop_hook(&mut self, i: usize, quest: &Quest) {
        self.members[i]
            .hooks
            .retain(|h| !(h.verb == quest.verb && h.target == quest.target));
    }

    /// The lowest vitality in the party: the vitality a world's beat uses.
    /// A vitality that is not a number is skipped, so it never makes a
    /// world's beat tense, the same as it never makes a member's beat tense
    /// in [`Saga::tone`].
    fn lowest_vitality(&self) -> f32 {
        self.state
            .party
            .iter()
            .map(|m| m.hero.vitality)
            .fold(f32::INFINITY, f32::min)
    }

    /// The tone rule of the module comment, for a beat whose vitality is
    /// `vitality` (a member's own, or the party's lowest for a world's
    /// beat).
    fn tone(&self, collapse: bool, triumph: bool, vitality: f32) -> Tone {
        if collapse || vitality < REST_BELOW {
            Tone::Tense
        } else if triumph {
            Tone::Triumphant
        } else {
            Tone::Calm
        }
    }

    /// A beat context in the current world. `who` is the member the beat is
    /// about: the beat takes that member's floor and hero name. A world's
    /// beat (born or ended) passes `None`: it belongs to no floor, and its
    /// hero name is the lead member's.
    #[allow(clippy::too_many_arguments)]
    fn ctx(
        &self,
        who: Option<usize>,
        tone: Tone,
        name: Option<RealName>,
        role: NameRole,
        verb: Option<Verb>,
        why: Option<EndWhy>,
        cause: &Cause,
    ) -> BeatCtx {
        let hero = &self.state.party[who.unwrap_or(0)].hero;
        BeatCtx {
            world: self.state.world.id,
            biome: self.state.world.biome,
            tone,
            name,
            role,
            hero: hero.name.clone(),
            verb,
            why,
            tick: self.state.tick,
            cause: cause.clone(),
            floor: who.map(|_| hero.floor),
            // At most MAX_FLOORS (32) members, so the count fits a byte.
            count: u8::try_from(self.state.party.len()).unwrap_or(u8::MAX),
        }
    }

    /// Member `i`'s routine line under the chronicle's pace (see "Pace" in
    /// the module comment). When the line's floor owns the current window,
    /// or the window has no owner yet and the floor takes it, the line is
    /// written, and its event is returned without being raised. Otherwise nothing is
    /// written, member `i`'s `unwritten` counts one more, and `None` is
    /// returned. A line with no floor is always written.
    fn routine_beat(&mut self, i: usize, kind: BeatKind, ctx: &BeatCtx) -> Option<SagaEvent> {
        if let Some(floor) = ctx.floor {
            let tick = self.state.tick;
            let window = tick / ROUTINE_WINDOW_TICKS;
            let overdue = |heard: u64| tick.saturating_sub(heard) >= OVERDUE_TICKS;
            let writes = match self.window_owner {
                // A window with an owner: only the owner's floor writes.
                Some((k, owner)) if k == window => owner == floor,
                // An open window: an overdue member takes it; another member
                // takes it only while no member is overdue. With one member
                // either holds, so a lone member takes every window.
                _ => {
                    let takes = overdue(self.members[i].heard)
                        || !self.members.iter().any(|m| overdue(m.heard));
                    if takes {
                        self.window_owner = Some((window, floor));
                    }
                    takes
                }
            };
            if !writes {
                if let Some(m) = self.state.party.get_mut(i) {
                    m.unwritten = m.unwritten.saturating_add(1);
                }
                return None;
            }
            self.members[i].heard = tick;
        }
        Some(self.write_beat(kind, ctx))
    }

    /// Writes a beat and raises its event.
    fn beat(&mut self, kind: BeatKind, ctx: &BeatCtx) {
        let ev = self.write_beat(kind, ctx);
        self.emit(ev);
    }

    /// Writes a beat to the chronicle, marking the first line of a mock or
    /// replay saga, and returns its event without raising it.
    fn write_beat(&mut self, kind: BeatKind, ctx: &BeatCtx) -> SagaEvent {
        let chronicle = &mut self.state.chronicle;
        chronicle.push_beat(
            kind,
            ctx,
            Pack::builtin(),
            self.cfg.names,
            &mut self.story_rng,
        );
        if !self.wrote_first {
            self.wrote_first = true;
            if let Some(mark) = self.cfg.label.mark() {
                chronicle.append_to_last(mark);
            }
        }
        chronicle
            .lines(1)
            .next()
            .expect("a beat was just written")
            .event(ctx.world)
    }

    /// Raises an event and folds it into the digest.
    fn emit(&mut self, ev: SagaEvent) {
        #[cfg(test)]
        crate::cost::event();
        let mut buf = std::mem::take(&mut self.enc);
        buf.clear();
        encode_event(&mut buf, self.state.tick, &ev);
        self.trace = fnv1a64_extend(self.trace, &buf);
        self.enc = buf;
        if self.events.len() >= MAX_PENDING_EVENTS {
            self.events.pop_front();
            self.dropped_events += 1;
        }
        self.events.push_back(ev);
    }
}

/// A rest where the hero stands, caused by `chip` with an empty signal: the
/// placeholder quest of the placeholder world, and of each member between
/// its birth and its first plan.
fn rest_at(pos: (u8, u8), chip: EntityKey) -> Quest {
    Quest {
        verb: Verb::Rest,
        target: None,
        reason: Cause {
            entity: chip,
            signal: String::new(),
            ordinal: 0,
        },
        goal: pos,
        target_label: None,
    }
}

#[cfg(test)]
impl Saga {
    /// Test only: a saga in which member `member`, at every birth, gets one
    /// more sage hook first (for the made-up model `model:demo-walled`), and
    /// the exit of its floor, where every sage stands, becomes a wall. Its
    /// first quest then has no path, so it replans at its first step. No
    /// other member and no other floor changes.
    pub(crate) fn with_walled_sage(cfg: SagaConfig, member: usize) -> Saga {
        let mut saga = Saga::new(cfg);
        saga.walled_sage = Some(member);
        saga
    }

    /// Applies `with_walled_sage` to the world just born.
    fn wall_test_sage(&mut self) {
        let Some(i) = self.walled_sage else { return };
        // The floors are shared behind an `Arc` and never change in a
        // world's life. This test hook runs at the birth, before anything
        // else holds them, so `make_mut` adds the wall in place.
        let floors = Arc::make_mut(&mut self.state.world.floors);
        let (Some(core), Some(floor)) = (self.members.get_mut(i), floors.get_mut(i)) else {
            return;
        };
        core.hooks.insert(
            0,
            Hook {
                verb: Verb::VisitSage,
                target: Some(EntityKey::model("demo-walled")),
                reason: Cause {
                    entity: EntityKey::model("demo-walled"),
                    signal: "ModelReady".to_string(),
                    ordinal: 0,
                },
            },
        );
        let (x, y) = floor.exit;
        let w = floor.w as usize;
        floor.cells[y as usize * w + x as usize] = maze::Cell::Wall;
    }
}

// ---------------------------------------------------------------------------
// The digest's byte encoding. See "The digest" in the module comment for the
// table; every match below lists every variant, so a new variant does not
// compile until it has a code.
// ---------------------------------------------------------------------------

/// The version of the digest's byte encoding, hashed first by every saga.
/// Version 3 (task B.3 of the party plan) has the items of version 2; it
/// marks the saga whose portals know their Ethernet ports. Version 2 added
/// the version item itself and a floor on every quest event, beat and
/// chronicle line, and gave the end item one triple per member (the
/// addendum, section 3.6). Version 1, the encoding of 0.16.x, had no
/// version item.
pub const DIGEST_VERSION: u8 = 3;
/// Stream tag of the version item.
const TAG_VERSION: u8 = 0x00;
/// Stream tag of a world seed.
const TAG_SEED: u8 = 0x10;
/// Stream tag of a chronicle line.
const TAG_LINE: u8 = 0x20;
/// Stream tag of the digest's last item.
const TAG_END: u8 = 0x30;

/// The one-byte tag of an event.
fn event_tag(ev: &SagaEvent) -> u8 {
    match ev {
        SagaEvent::WorldBorn { .. } => 0x01,
        SagaEvent::WorldEnded { .. } => 0x02,
        SagaEvent::HeroBorn { .. } => 0x03,
        SagaEvent::QuestStarted { .. } => 0x04,
        SagaEvent::QuestDone { .. } => 0x05,
        SagaEvent::Beat { .. } => 0x06,
    }
}

/// Appends one event item: its tag, the tick, then its fields in the
/// table's order.
fn encode_event(out: &mut Vec<u8>, tick: u64, ev: &SagaEvent) {
    out.push(event_tag(ev));
    put_u64(out, tick);
    match ev {
        SagaEvent::WorldBorn { world, biome } => {
            put_u64(out, *world);
            out.push(biome_code(*biome));
        }
        SagaEvent::WorldEnded { world, why } => {
            put_u64(out, *world);
            out.push(why_code(*why));
        }
        SagaEvent::HeroBorn { world, name, floor } => {
            put_u64(out, *world);
            put_str(out, name);
            out.push(*floor);
        }
        SagaEvent::QuestStarted {
            world,
            floor,
            verb,
            target,
            reason,
        } => {
            put_u64(out, *world);
            out.push(*floor);
            out.push(verb_code(*verb));
            match target {
                Some(key) => {
                    out.push(1);
                    put_str(out, key.as_str());
                }
                None => out.push(0),
            }
            put_cause(out, reason);
        }
        SagaEvent::QuestDone { world, floor, verb } => {
            put_u64(out, *world);
            out.push(*floor);
            out.push(verb_code(*verb));
        }
        SagaEvent::Beat {
            world,
            tone,
            floor,
            text,
            cause,
        } => {
            put_u64(out, *world);
            put_opt_u8(out, *floor);
            out.push(tone_code(*tone));
            put_str(out, text);
            put_cause(out, cause);
        }
    }
}

/// Appends one chronicle line item: its tag, the line's floor option, then
/// its text.
fn encode_line(out: &mut Vec<u8>, beat: &Beat) {
    out.push(TAG_LINE);
    put_opt_u8(out, beat.floor);
    put_str(out, &beat.text);
}

/// Appends the end item: its tag, the current world's seed, the number of
/// members as one byte, then each member's floor, x and y in index order.
/// The party has at most [`crate::world::MAX_FLOORS`] (32) members, so the
/// count fits a byte; a longer list would record 255, and a debug build
/// stops on it.
fn encode_end(out: &mut Vec<u8>, seed: u64, members: &[(u8, u8, u8)]) {
    debug_assert!(
        members.len() <= usize::from(u8::MAX),
        "the end item counts {} members in one byte",
        members.len()
    );
    out.push(TAG_END);
    put_u64(out, seed);
    out.push(u8::try_from(members.len()).unwrap_or(u8::MAX));
    for &(floor, x, y) in members {
        out.extend_from_slice(&[floor, x, y]);
    }
}

/// One presence byte (0 absent, 1 present), then the byte when present.
fn put_opt_u8(out: &mut Vec<u8>, v: Option<u8>) {
    match v {
        Some(b) => out.extend_from_slice(&[1, b]),
        None => out.push(0),
    }
}

/// 8 bytes, little-endian.
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A `u32` little-endian byte length, then the UTF-8 bytes. Saga text is
/// far shorter than 4 GB; a longer string would record `u32::MAX`.
fn put_str(out: &mut Vec<u8>, s: &str) {
    let len = u32::try_from(s.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Entity key, signal, ordinal.
fn put_cause(out: &mut Vec<u8>, c: &Cause) {
    put_str(out, c.entity.as_str());
    put_str(out, &c.signal);
    put_u64(out, c.ordinal);
}

fn biome_code(b: Biome) -> u8 {
    match b {
        Biome::Dungeon => 0,
        Biome::Forge => 1,
        Biome::Dream => 2,
    }
}

fn why_code(w: EndWhy) -> u8 {
    match w {
        EndWhy::ModelDown => 0,
        EndWhy::Reset => 1,
        EndWhy::Season => 2,
        EndWhy::HeroDied => 3,
        EndWhy::Superseded => 4,
        EndWhy::RunEnded => 5,
    }
}

fn verb_code(v: Verb) -> u8 {
    match v {
        Verb::Explore => 0,
        Verb::Rest => 1,
        Verb::VisitSage => 2,
        Verb::Slay => 3,
        Verb::Escort => 4,
        Verb::Cleanse => 5,
        Verb::Rebuild => 6,
    }
}

fn tone_code(t: Tone) -> u8 {
    match t {
        Tone::Calm => 0,
        Tone::Tense => 1,
        Tone::Triumphant => 2,
    }
}

/// A model key's id as a [`RealName`]: `model:demo-sage` gives `demo-sage`.
fn real_name(model: &EntityKey) -> Option<RealName> {
    let id = model
        .as_str()
        .split_once(':')
        .map_or(model.as_str(), |(_, id)| id);
    RealName::new(id)
}

/// An event's main entity, if it has one, and its kind as the log writes it.
fn event_cause(e: &Event) -> (Option<EntityKey>, &'static str) {
    match e {
        Event::ModelLoading { model, .. } => (Some(model.clone()), "ModelLoading"),
        Event::ModelReady { model, .. } => (Some(model.clone()), "ModelReady"),
        Event::ModelGone { model } => (Some(model.clone()), "ModelGone"),
        Event::RequestsCompleted { model, .. } => (Some(model.clone()), "RequestsCompleted"),
        Event::KernelCompiled { chip, .. } => (chip.clone(), "KernelCompiled"),
        Event::Reset { chips } => (chips.first().cloned(), "Reset"),
        Event::LinkChanged { chip, .. } => (Some(chip.clone()), "LinkChanged"),
        Event::LeaseGranted { lease, .. } => (Some(lease.clone()), "LeaseGranted"),
        Event::LeaseReleased { lease, .. } => (Some(lease.clone()), "LeaseReleased"),
        Event::EccTick { chip, .. } => (Some(chip.clone()), "EccTick"),
        Event::ViewChanged { .. } => (None, "ViewChanged"),
        Event::LinkPeer { port, .. } => (Some(port.clone()), "LinkPeer"),
        Event::Spark { chip, .. } => (chip.clone(), "Spark"),
    }
}

#[cfg(test)]
#[path = "saga_tests.rs"]
mod saga_tests;

#[cfg(test)]
#[path = "fuzz_tests.rs"]
mod fuzz_tests;

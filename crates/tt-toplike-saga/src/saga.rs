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
//!                     hero, quests and chronicle ◄──┘ ─► SagaEvents
//! ```
//!
//! # One observation, in order
//!
//! 1. The observation goes into the [`SeedPool`].
//! 2. A sample on a chip (or on a chip's tile, DRAM channel or Ethernet port)
//!    records the chip as seen, while fewer than [`MAX_CHIPS`] chips are kept
//!    (a chip beyond the cap stops here). A topology reading
//!    (`toplike.harvesting_state`, `toplike.enabled_tensix_col`,
//!    `toplike.eth.live_mask`) updates that chip's floor spec. A sample on
//!    the chip itself goes into the [`TrailBook`]: temperatures through
//!    [`push_temperature`], so the baseline series exists (ledger ruling R15),
//!    and every other number as it is (the vitality inputs through
//!    [`TrailBook::push_reserved`], see "Bounds"). A `toplike.heartbeat` value that
//!    differs from the last one marks the chip's heartbeat as advanced at
//!    this tick.
//! 3. An event updates the list of ready models (a `ModelReady` adds one
//!    while fewer than [`MAX_READY_MODELS`] are listed, a `ModelGone` removes
//!    it). A tick counts one more tick.
//! 4. On a tick, every chip that has ever reported a heartbeat gets one push
//!    of the [`HEARTBEAT_ALIVE`] series: 1.0 if its heartbeat advanced within
//!    the last [`HEARTBEAT_GRACE_TICKS`] ticks (the tick of the change and
//!    the 19 after it), else 0.0 (ruling R15).
//! 5. The [`Lifecycle`] steps. Each world it ends or births becomes events
//!    and chronicle beats (below).
//! 6. On a tick in a world that was not born by this observation, the hero
//!    acts (below).
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
//! specs at its birth and keeps them for its life.
//!
//! # Worlds, heroes and quests
//!
//! When a world is born:
//!
//! * two generators are seeded from its seed. The plan stream names the hero
//!   and plans its quests. The chronicle stream is used by the chronicle
//!   alone (ruling R16): generated names draw more from it than real names
//!   do, and that must not change the maze or the hero's walk;
//! * the hooks are rebuilt: one visit-the-sage hook per ready model, in model
//!   key order, then one explore hook and one rest hook whose cause is the
//!   world's birth;
//! * a hero is born at floor 0's entrance, named by [`Pack::hero_name`] from
//!   the plan stream, and plans its first quest.
//!
//! The events of a birth come in this order: `WorldBorn`, its `Beat`,
//! `HeroBorn`, its `Beat`, `QuestStarted`, its `Beat`. An end comes before
//! the birth that follows it: `WorldEnded`, its `Beat`.
//!
//! On each tick in a world the hero's vitality is read from the trails of
//! its floor's chip, and the hero takes one step. Then:
//!
//! * **Arrived.** `QuestDone` and a beat. A sage hook is dropped once its
//!   sage is visited. The hero then waits [`DWELL_TICKS`] ticks and plans its
//!   next quest, so the chronicle does not fill with arrivals.
//! * **A model goes away.** Its sage hook leaves the current world, whichever
//!   signal made that world, and a hero on its way to that sage plans again
//!   at once.
//! * **No path.** The hook behind the quest is dropped for this world, so the
//!   hero does not retry one unreachable goal forever, and the hero plans
//!   again at once (ruling R15). The explore hook is never dropped: each plan
//!   draws a new room, so the next one may be reachable.
//!
//! # Tone
//!
//! Each beat's tone is chosen from the machine's state (spec 5):
//!
//! * **Tense** when the hero's vitality is below [`REST_BELOW`], or when the
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
//! left out of the event in generated mode (phase 1 quests have none). The
//! state's `quest` keeps the real reason in both modes, so the two modes plan
//! and walk alike.
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
//! | a world's hooks | [`MAX_READY_MODELS`] sage hooks (they come from the ready list), plus explore and rest | here |
//! | undrained events | [`MAX_PENDING_EVENTS`] (4096); the oldest is dropped and counted in [`Saga::dropped_events`] | here |
//! | chronicle lines | [`CAPACITY`] (500) | [`crate::chronicle`] |
//! | generated names | [`crate::chronicle::ALIAS_LIMIT`] (256) | [`crate::chronicle`] |
//! | seed pool | [`crate::seed::RING_LEN`] (64) values, 512 bytes | [`crate::seed`] |
//! | one floor | [`crate::topology::MAX_COLS`] x [`crate::topology::MAX_ROWS`] (120 x 60) cells | [`crate::topology`] |
//! | the hero's path | one floor's cells | [`crate::hero`] |
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
//! The entities the saga holds at once are at most 64 kept chips, 64 ready
//! models and 32 world chips that need not be kept chips: 160, under the
//! spec's 500 (spec 9).
//!
//! **Cost of one record.** At most one world is born per record: an event
//! births at most once, and a sweep's samples can start one training run,
//! after which its tick finds the run fresh and the world newly active. A
//! birth generates one maze per world chip, so it costs at most 32 mazes of
//! at most 120 x 60 cells. The hero searches at most once per tick, and A*
//! expands each cell at most once. Today's floors are 53 x 38 (Blackhole) and
//! 50 x 32 (generic), so the widest birth generates about 64,000 cells. The
//! test-only work counter (`src/cost.rs`) checks this against a budget of a
//! million work units per record. A unit (a maze cell, an A* expansion, a
//! trail push or an event) is not an elementary operation: a maze cell costs
//! roughly 30 of them. Trail pushes grow with the record's own size: one per
//! sweep value, plus one per kept chip on a tick.
//!
//! # The digest
//!
//! [`Saga::digest`] is FNV-1a 64 over one byte stream, written by hand so
//! that it is the same on every platform and compiler (ledger ruling R19).
//! No `Debug` output is hashed. The stream is a sequence of items, each
//! starting with a one-byte tag:
//!
//! | Tag | Item | Fields after the tag, in order |
//! |---|---|---|
//! | `0x01` | `WorldBorn` event | tick, world, biome |
//! | `0x02` | `WorldEnded` event | tick, world, why |
//! | `0x03` | `HeroBorn` event | tick, world, name, floor |
//! | `0x04` | `QuestStarted` event | tick, world, verb, target, reason |
//! | `0x05` | `QuestDone` event | tick, world, verb |
//! | `0x06` | `Beat` event | tick, world, tone, text, cause |
//! | `0x10` | a world's seed, at its birth (before its `WorldBorn`) | seed |
//! | `0x20` | one chronicle line, oldest first (only in [`Saga::digest`]) | text |
//! | `0x30` | the end (only in [`Saga::digest`]) | current world seed, hero floor, hero x, hero y |
//!
//! Field encodings:
//!
//! * `tick`, `world`, `seed` and ordinals: `u64`, 8 bytes little-endian.
//! * `floor`, hero x and y: one byte.
//! * strings and entity keys: the UTF-8 length as a `u32`, 4 bytes
//!   little-endian, then the UTF-8 bytes.
//! * an `Option`: one presence byte (0 absent, 1 present), then the value
//!   when present.
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

use std::collections::{BTreeMap, VecDeque};

use tt_toplike_bus::{EntityKey, Event, Payload, Record};

use crate::chronicle::{
    BeatCtx, BeatKind, Chronicle, NameMode, NameRole, RealName, Tone, CAPACITY,
};
use crate::event::{Biome, EndWhy, SagaEvent};
use crate::hero::{
    push_temperature, vitality, Cause, Hero, ASIC_TEMPERATURE, HEARTBEAT_ALIVE,
    HEARTBEAT_GRACE_TICKS, VITALITY_SIGNALS,
};
use crate::maze;
use crate::observe::{Feed, Observation};
use crate::pack::Pack;
use crate::quest::{plan, Hook, Quest, Verb, REST_BELOW};
use crate::rng::Pcg32;
use crate::seed::{fnv1a64, fnv1a64_extend, SeedPool};
use crate::stats::TrailBook;
use crate::topology::{Arch, FloorSpec};
use crate::world::{Lifecycle, World, MAX_CHIPS, NO_CHIP};

/// Stream selector of the generator that names heroes and plans quests.
pub const PLAN_STREAM: u64 = 1;

/// Stream selector of the chronicle's own generator (ruling R16).
pub const CHRONICLE_STREAM: u64 = 2;

/// Ticks the hero waits after finishing a quest before it plans the next
/// one (3 s at 10 Hz).
pub const DWELL_TICKS: u64 = 30;

/// Ready models the saga keeps (ledger ruling R20). A `ModelReady` for a new
/// model while this many are ready is not added to the list; its world is
/// still born. Each world's sage hooks come from this list, so a world has at
/// most this many sage hooks, plus one explore and one rest hook.
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

/// Everything a view draws.
///
/// Before the first world is born (at most five ticks, ledger ruling R11)
/// `world` is a placeholder with id 0: one generic floor for the chip
/// [`NO_CHIP`], seed 0, and a nameless hero resting at its entrance. Every
/// real world has an id of 1 or more.
#[derive(Debug, Clone)]
pub struct SagaState {
    pub label: Label,
    pub world: World,
    pub hero: Hero,
    /// The hero's current quest. After an arrival it stays the finished
    /// quest until the hero plans the next one.
    pub quest: Quest,
    pub chronicle: Chronicle,
    /// Ticks seen so far (pacemaker sweeps, see [`crate::observe`]).
    pub tick: u64,
}

impl SagaState {
    /// Whether a real world has been born. Before the first world (at most
    /// five ticks) the state holds the placeholder world with id 0, and a
    /// view shows a waiting state.
    pub fn has_world(&self) -> bool {
        self.world.id > 0
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
    /// Ready models and the cause of their `ModelReady`.
    ready: BTreeMap<EntityKey, Cause>,
    /// The current world's hooks.
    hooks: Vec<Hook>,
    /// Names heroes and plans quests. Reseeded at each birth.
    plan_rng: Pcg32,
    /// The chronicle's own stream. Reseeded at each birth.
    story_rng: Pcg32,
    /// The tick at which the hero plans its next quest after an arrival.
    dwell_until: Option<u64>,
    /// Whether the first chronicle line has been written (and marked).
    wrote_first: bool,
    /// Events raised and not yet drained, oldest first, at most
    /// [`MAX_PENDING_EVENTS`].
    events: VecDeque<SagaEvent>,
    /// Events dropped from a full buffer before they were drained.
    dropped_events: u64,
    /// The FNV-1a 64 state after the digest stream so far: world seeds and
    /// events (see "The digest" in the module comment).
    trace: u64,
    /// Scratch buffer for the digest's encoding.
    enc: Vec<u8>,
    /// Scratch buffers, kept to avoid an allocation per record.
    obs: Vec<Observation>,
    world_events: Vec<SagaEvent>,
    state: SagaState,
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
    /// A saga that has seen nothing.
    pub fn new(cfg: SagaConfig) -> Saga {
        let chip = EntityKey::chip(NO_CHIP);
        let floor = maze::generate(&FloorSpec::generic(&chip), 0, 0.0);
        let pos = floor.entrance;
        let placeholder = World {
            id: 0,
            seed: 0,
            born_tick: 0,
            chips: vec![chip.clone()],
            floors: vec![floor],
            biome: Biome::Dungeon,
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
            hooks: Vec::new(),
            plan_rng: Pcg32::new(0, PLAN_STREAM),
            story_rng: Pcg32::new(0, CHRONICLE_STREAM),
            dwell_until: None,
            wrote_first: false,
            events: VecDeque::new(),
            dropped_events: 0,
            trace: fnv1a64(&[]),
            enc: Vec::new(),
            obs: Vec::new(),
            world_events: Vec::new(),
            state: SagaState {
                label: cfg.label,
                world: placeholder,
                hero: Hero::new(String::new(), 0, pos, 0),
                quest: Quest {
                    verb: Verb::Rest,
                    target: None,
                    reason: Cause {
                        entity: chip,
                        signal: String::new(),
                        ordinal: 0,
                    },
                    goal: pos,
                },
                chronicle: Chronicle::new(),
                tick: 0,
            },
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
            buf.push(TAG_LINE);
            put_str(&mut buf, &beat.text);
            h = fnv1a64_extend(h, &buf);
        }
        buf.clear();
        buf.push(TAG_END);
        put_u64(&mut buf, self.state.world.seed);
        let (x, y) = self.state.hero.pos;
        buf.extend_from_slice(&[self.state.hero.floor, x, y]);
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
            } => self.on_sample(entity, signal, *value),
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

        // A hero walking to a sage that just went away plans again at once.
        // When the model's own dungeon ended, the new world already planned.
        if let Observation::Event(Event::ModelGone { model }) = o {
            let walking_to_it = self.state.quest.target.as_ref() == Some(model);
            if !born && walking_to_it && self.state.world.id > 0 {
                self.dwell_until = None;
                self.start_quest(false);
            }
        }

        if matches!(o, Observation::Tick) && !born && self.state.world.id > 0 {
            self.hero_tick();
        }
    }

    /// A sample: chip readings, trails and heartbeats.
    fn on_sample(&mut self, entity: &EntityKey, signal: &str, value: f64) {
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
            ETH_LIVE_MASK => self.topo_dirty |= changed(&mut seen.eth_live_mask),
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

    /// An event: the list of ready models.
    fn on_event(&mut self, e: &Event, ordinal: u64) {
        match e {
            Event::ModelReady { model, .. } => {
                // A model already listed takes the new cause; a new one is
                // listed only below MAX_READY_MODELS.
                if !self.ready.contains_key(model) && self.ready.len() >= MAX_READY_MODELS {
                    return;
                }
                let cause = Cause {
                    entity: model.clone(),
                    signal: "ModelReady".to_string(),
                    ordinal,
                };
                self.ready.insert(model.clone(), cause);
            }
            Event::ModelGone { model } => {
                // The model's sage leaves every world, including one the
                // model did not make (ruling R14 keeps such a world alive).
                self.ready.remove(model);
                self.hooks.retain(|h| h.target.as_ref() != Some(model));
            }
            _ => {}
        }
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
        let tone = self.tone(moment.collapse, false);
        let ctx = self.ctx(
            tone,
            None,
            NameRole::Service,
            None,
            Some(why),
            &moment.cause,
        );
        self.beat(BeatKind::WorldEnded, &ctx);
    }

    /// A world was born: take it from the lifecycle, reseed, make the hero
    /// and its first quest. See the module comment for the event order.
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
        self.plan_rng = Pcg32::new(world.seed, PLAN_STREAM);
        self.story_rng = Pcg32::new(world.seed, CHRONICLE_STREAM);
        self.dwell_until = None;

        // Hooks: the sages first, then explore and rest.
        self.hooks = self
            .ready
            .iter()
            .map(|(model, cause)| Hook {
                verb: Verb::VisitSage,
                target: Some(model.clone()),
                reason: cause.clone(),
            })
            .collect();
        for verb in [Verb::Explore, Verb::Rest] {
            self.hooks.push(Hook {
                verb,
                target: None,
                reason: moment.cause.clone(),
            });
        }

        let name = Pack::builtin().hero_name(&mut self.plan_rng);
        let entrance = world.floors.first().map_or((0, 0), |f| f.entrance);
        self.state.hero = Hero::new(name.clone(), 0, entrance, world.born_tick);
        let (id, biome) = (world.id, world.biome);
        self.state.world = world;

        let triumph = moment.ready_model.is_some();
        let tone = self.tone(moment.collapse, triumph);
        let model_name = moment.ready_model.as_ref().and_then(real_name);
        let role = if model_name.is_some() {
            NameRole::Model
        } else {
            NameRole::Service
        };

        self.emit(SagaEvent::WorldBorn { world: id, biome });
        let ctx = self.ctx(tone, model_name, role, None, None, &moment.cause);
        self.beat(BeatKind::WorldBorn, &ctx);

        self.emit(SagaEvent::HeroBorn {
            world: id,
            name,
            floor: 0,
        });
        let ctx = self.ctx(tone, None, NameRole::Service, None, None, &moment.cause);
        self.beat(BeatKind::HeroBorn, &ctx);

        self.start_quest(moment.collapse);
    }

    /// Plans the hero's next quest and raises `QuestStarted` and its beat.
    fn start_quest(&mut self, collapse: bool) {
        let quest = plan(
            &self.state.world,
            &self.state.hero,
            &self.hooks,
            &mut self.plan_rng,
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
        let tone = self.tone(collapse, false);
        let ctx = self.ctx(tone, name, role, Some(quest.verb), None, &quest.reason);
        let stored = self.write_beat(BeatKind::QuestStarted, &ctx);

        // In generated mode the event takes the beat's cleaned cause (see
        // "Names" in the module comment).
        let (reason, target) = match self.cfg.names {
            NameMode::Real => (quest.reason.clone(), quest.target.clone()),
            NameMode::Generated => {
                let cause = match &stored {
                    SagaEvent::Beat { cause, .. } => cause.clone(),
                    _ => quest.reason.clone(),
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
            verb: quest.verb,
            target,
            reason,
        });
        self.emit(stored);
        self.state.quest = quest;
    }

    /// The hero's tick: vitality, one step, and what follows it.
    fn hero_tick(&mut self) {
        // Vitality first, so it is fresh while the hero dwells and when it
        // plans after dwelling.
        let hero_floor = self.state.hero.floor as usize;
        if let Some(chip) = self.state.world.chips.get(hero_floor) {
            self.state.hero.vitality = vitality(&self.trails, chip);
        }
        if let Some(until) = self.dwell_until {
            if self.state.tick < until {
                return;
            }
            self.dwell_until = None;
            self.start_quest(false);
            return;
        }
        let Some(floor) = self.state.world.floors.get(hero_floor) else {
            return;
        };
        let r = self.state.hero.step(floor, &self.state.quest);
        if r.arrived {
            let quest = self.state.quest.clone();
            if quest.verb == Verb::VisitSage {
                self.drop_hook(&quest);
            }
            self.emit(SagaEvent::QuestDone {
                world: self.state.world.id,
                verb: quest.verb,
            });
            let tone = self.tone(false, true);
            let ctx = self.ctx(
                tone,
                None,
                NameRole::Service,
                Some(quest.verb),
                None,
                &quest.reason,
            );
            self.beat(BeatKind::QuestDone, &ctx);
            self.dwell_until = Some(self.state.tick + DWELL_TICKS);
        } else if r.no_path {
            let quest = self.state.quest.clone();
            if quest.verb != Verb::Explore {
                self.drop_hook(&quest);
            }
            self.start_quest(false);
        }
    }

    /// Removes the hook a quest came from.
    fn drop_hook(&mut self, quest: &Quest) {
        self.hooks
            .retain(|h| !(h.verb == quest.verb && h.target == quest.target));
    }

    /// The tone rule of the module comment.
    fn tone(&self, collapse: bool, triumph: bool) -> Tone {
        if collapse || self.state.hero.vitality < REST_BELOW {
            Tone::Tense
        } else if triumph {
            Tone::Triumphant
        } else {
            Tone::Calm
        }
    }

    /// A beat context in the current world.
    fn ctx(
        &self,
        tone: Tone,
        name: Option<RealName>,
        role: NameRole,
        verb: Option<Verb>,
        why: Option<EndWhy>,
        cause: &Cause,
    ) -> BeatCtx {
        BeatCtx {
            world: self.state.world.id,
            biome: self.state.world.biome,
            tone,
            name,
            role,
            hero: self.state.hero.name.clone(),
            verb,
            why,
            tick: self.state.tick,
            cause: cause.clone(),
        }
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

// ---------------------------------------------------------------------------
// The digest's byte encoding. See "The digest" in the module comment for the
// table; every match below lists every variant, so a new variant does not
// compile until it has a code.
// ---------------------------------------------------------------------------

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
            verb,
            target,
            reason,
        } => {
            put_u64(out, *world);
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
        SagaEvent::QuestDone { world, verb } => {
            put_u64(out, *world);
            out.push(verb_code(*verb));
        }
        SagaEvent::Beat {
            world,
            tone,
            text,
            cause,
        } => {
            put_u64(out, *world);
            out.push(tone_code(*tone));
            put_str(out, text);
            put_cause(out, cause);
        }
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
        Event::Spark { chip, .. } => (chip.clone(), "Spark"),
    }
}

#[cfg(test)]
#[path = "saga_tests.rs"]
mod saga_tests;

#[cfg(test)]
#[path = "fuzz_tests.rs"]
mod fuzz_tests;

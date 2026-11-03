# Saga party and portal crossing: an addendum to the Saga design

Date: 2026-11-03. Status: phase A (the party) built in 0.17.0, on branch `saga-party`; phase B
(crossing, section 8) not built. Author: Claude, with Taylor Singletary. Numbers that moved while
building are corrected in place and marked "(as built)": the grid table in 5.2, the roster rule in
5.4 and the entity count in 3.7. The ledger is `.superpowers/sdd/2026-11-03-saga-party/progress.md`.
Base: `docs/superpowers/specs/2026-11-02-saga-design.md` (called "the spec" below) and the phase 1
code on `what-if` at 0.16.2 (HEAD 6932937d). Section numbers such as "spec 3.5" point into the spec.

## 1. What the user asked for

The user asked how the hero travels between floors. Today it does not. Phase 1 has one hero, on
floor 0. Every chip has a floor, built from the chip's real shape, but the hero never visits any
floor after the first. The `▼` exit is where the sage stands. The Ethernet portals on the north
edge are scenery.

The user's words: "i'd prefer a view that mixed the chips together for saga. or showed you the
sagas happening on each chip at the same time". Offered the choice, the user picked "Both: party
plus portal crossing", in two stages:

1. **Party.** Every chip's floor is on screen at once. Each floor has its own hero, and all of
   them quest at the same time. One merged chronicle holds every line, and each line is tagged
   with its chip.
2. **Crossing.** A hero can step through a portal into the floor of the chip at the other end of
   that Ethernet link, using the real link topology.

This addendum decides how both stages work. Section 9 lists the questions only the user can
answer. Section 10 lists the calls I made myself.

## 2. What stays the same

Everything in the spec that this addendum does not name stays as it is. In particular:

- **The contract (spec 3.5).** The same ordered stream of record contents gives the same saga. The
  core reads only each record's source and payload. No sequence number, no timestamp, no hash-map
  order, and only `+ - * /`, comparisons and `sqrt` in the core.
- **One world at a time (spec 3.2).** The world lifecycle keeps its rules and its ledger rulings
  (R11, R13, R14). A party lives in one world. When the world ends, the whole party ends with it,
  and the next world brings a new party.
- **The core never sees the view.** What is on screen, which tile has focus and the terminal size
  never reach the core. This rules out "simulate only the floors in view" (section 6).
- **Privacy (spec 10).** Command lines never appear. `names = "generated"` leaves no real name in
  any printed text, any cause, or any event.
- **Pace per hero (spec 3.10).** Each hero keeps today's walk speed, its 30-tick dwell after a
  quest, and today's vitality formula.
- **One chip looks like today.** On a box with one chip, the view draws exactly what 0.16.2
  draws, and the chronicle tells exactly the same story. Section 7 says how this is tested.

## 3. The party

### 3.1 One hero per floor

A world already has one floor per chip, up to `MAX_FLOORS` (32) in chip key order (R22). The party
is one hero per floor of the world. Hero `i` is born on floor `i`, at that floor's PCIe gate.

- **Born when.** All heroes are born with their world, in floor order, in the same observation
  that births the world. No hero is born later in a world's life. Phase 2 adds descendants: a
  hero who dies is followed by a descendant on the same floor (section 3.8).
- **Which floors.** Today a `ModelReady` births a dungeon on the model's chips only. With a party,
  that would make the other chips vanish from the view whenever a model comes up on part of the
  box. So a model's dungeon now spans every kept chip, up to 32, the same set as every other
  birth. The model's sage stands only on the floors of the chips the model names (all floors when
  the event names no chips). The spec says "a model on several chips is one dungeon spanning those
  floors" (spec 3.7). This keeps that: the sage's floors are the model's chips. The world now
  also holds the other chips' floors, where heroes explore and rest. On a box with one chip
  nothing changes.
- **The state.** `SagaState` replaces its `hero` and `quest` fields with `party: Vec<Member>`,
  where `Member { hero: Hero, quest: Quest }` and `party[i]` is the hero born on floor `i`. The
  member's index is the hero's identity for the world's life. In the party stage `hero.floor`
  always equals the index. In the crossing stage it can differ.

### 3.2 Names: unique and deterministic

Each member has its own plan generator (section 4.2). Member `i` draws its name from its own
generator with `Pack::hero_name`, as today. If the name equals the name of a member with a lower
index, the member draws again, up to 8 draws. If the eighth draw still matches, the member keeps
it and adds a Roman numeral: the second "Wren the Unthrottled" becomes "Wren the Unthrottled II".
The numeral is one more than the count of lower members with that name. The lexicon has at least
40 first names and 40 titles, so a 32-member party almost never reaches the numeral. The rule
still guarantees unique names. Member 0 draws first and never collides, so a one-chip world names
its hero exactly as today.

### 3.3 Quests and hooks

- **Hooks per member.** Today the world has one hook list. Now each member has its own: one
  visit-the-sage hook for each ready model whose sage stands on the member's floor, in model key
  order, then explore and rest, as today. Visiting a sage drops that hook for that member only.
  `ModelGone` drops the model's hooks from every member, and every member walking to that sage
  plans again at once (today's rule, applied to each member).
- **No path.** The rule from R15 applies per member: an unreachable hook is dropped for that
  member, and the member plans again.
- **`plan` is unchanged.** It already takes a world, a hero, hooks and a generator. It reads the
  hero's floor from `hero.floor`. Each member calls it with its own hooks and its own generator.
- **The quest's target label.** Each `Quest` gains `target_label: Option<String>`, the name the
  view prints for the target. The core fills it after the quest's beat is written: the model's
  real name in real mode, or the alias the chronicle already gave that name in generated mode.
  Today the view rebuilds this label from the newest matching beat (ledger, Task 1.14 minor). With
  a party, many beats are not written (section 3.5), so that lookup would fail. The ledger already
  says the label belongs in the core. A label that the chronicle has not aliased yet is aliased
  through the chronicle's own generator, so the names setting never changes a maze or a walk
  (R16).

### 3.4 Vitality, trails and the step order

- **Vitality** is per member: the trails of the chip of the floor the hero stands on. The trail
  book is already keyed by chip, so nothing in `stats.rs` changes.
- **Step order.** On each tick, members act in index order: vitality, dwell, one step, then
  arrival or replanning. A member's step reads only its own floor and its own quest. A member
  never sees another member's position, so the order matters only for the order of events and
  beats, and that order is fixed.
- **Tone.** A member's beats take that member's vitality. A world's beats (born, ended) take the
  lowest vitality in the party. With one member both rules are today's rule.

### 3.5 The chronicle and `Beat`

- **A floor on each beat.** `BeatCtx` and `Beat` gain `floor: Option<u8>`. A member's beats carry
  its floor. A world's beats (born, ended, the party's birth) carry `None`. The floor is a field,
  outside the text, so the text of every beat stays as today's grammar writes it. The view prints
  the tag (section 5.4). With one chip it prints none.
- **Cause.** Unchanged. Every beat keeps its cause: the entity, the signal and the record's
  ordinal in the stream. Every cause still exists in the log, except in generated mode, where
  causes are aliased (R17).
- **The party's birth.** With one floor, a world's birth writes `WorldBorn`, `HeroBorn` and the
  first `QuestStarted` beat, as today. With two or more floors, 32 `HeroBorn` lines at once would
  fill the screen. So a world with a party writes one new `PartyBorn` beat after `WorldBorn` (for
  example "Four heroes wake in the dungeon, one on each floor"). Each member still raises its
  `HeroBorn` event with its name and floor. The pack gains `PartyBorn` templates and a `{count}`
  placeholder.
- **Notable and routine lines.** A world born or ended, the party's birth and (in the crossing
  stage) a crossing are notable. A member's quest started, quest done, rest and ambient lines are
  routine.
- **The pace of routine lines.** With 32 heroes each starting and ending quests, the merged
  chronicle would take one or two routine lines every tick: 10 to 20 lines a second, too fast to
  read. So routine lines share one budget. Ticks are grouped into windows of
  `ROUTINE_WINDOW_TICKS` (10, one second at 10 Hz). In each window, the first floor to write a
  routine line owns the window. That floor writes all its routine lines in the window. Routine
  lines from other floors in that window are not written. A notable line is always written.
  With one floor, that floor owns every window, so nothing is ever dropped and a one-chip story
  is today's story.
- **Dropped lines still raise events.** A member whose routine line is not written still raises
  its `QuestStarted` and `QuestDone` events, so tensixinet and the digest see every quest.
  Today, in generated mode, `QuestStarted` takes its aliased cause from the beat just written. A
  dropped beat has no stored cause, so the chronicle gains `clean_cause(&Cause) -> Cause`, which
  aliases a cause the same way `push_beat` does without writing a line. Its draws come from the
  chronicle's generator.
- **Counting the silence.** Each member counts its routine lines that were not written
  (`Member::unwritten`). The tile's one-line HUD (section 5.3) shows every member's current quest
  anyway, so a person can still follow a hero whose lines are not in the chronicle.
- **Names.** Hero names come from the lexicon and are never real names. The only real names a
  beat or a HUD can print are model, process and service names. Every one of them goes through
  the alias table in generated mode: the beat text, the beat's cause, the events, and the new
  `target_label`. The privacy test runs over every member, every tile HUD and every chronicle line
  of the 4-chip and 32-chip fixtures (section 7).

### 3.6 The digest

The digest stays an explicit byte encoding through FNV-1a (R19). Its stream gains a version item,
and the party fields join the existing items:

| Tag | Item | Fields after the tag | Change |
|---|---|---|---|
| `0x00` | the encoding version, once, first | version, one byte: 2 | new |
| `0x03` | `HeroBorn` | tick, world, name, floor | unchanged |
| `0x04` | `QuestStarted` | tick, world, floor, verb, target, reason | `floor` added |
| `0x05` | `QuestDone` | tick, world, floor, verb | `floor` added |
| `0x06` | `Beat` | tick, world, floor option, tone, text, cause | `floor` added |
| `0x20` | one chronicle line | floor option, text | `floor` added |
| `0x30` | the end | world seed, member count (one byte), then floor, x, y for each member in index order | the hero's three bytes become one triple per member |

Every other item keeps its encoding. The version byte goes into the hash when the saga is built,
so a saga that has seen nothing has a version 2 digest. The crossing stage adds its events and
moves the version to 3 (section 8.6).

This moves all three pinned goldens on purpose. `bringup.jsonl` births no world, so only the
version item moves it. `quiet.jsonl` (3 chips) and `busy.jsonl` (2 chips) also move because their
worlds now have parties. The single-chip fixture pinned before the change (section 7) moves only by
the encoding: its story print stays identical.

### 3.7 Caps for 32 chips and more

The deferred item from R22 ("lazy floors or a birth rate limit") is settled here: **every floor of
the world is simulated, and every floor has a hero. No floors are lazy.**

- **Why every floor.** The view shows every floor at once, so the story must exist on every
  floor. "Only floors in view" would let the terminal's size and the focused tile decide which
  heroes exist. That breaks the contract: two terminals of different sizes would tell different
  stories from one log.
- **Why no birth rate limit.** R22 measured a 32-floor birth at 1.30 ms worst case on a loaded box
  and accepted it. A birth is rare. Spreading floor generation over the following ticks would add
  a state where a hero has no floor yet, and buy little.
- **The caps.** `MAX_CHIPS` stays 64 and `MAX_FLOORS` stays 32, so a party has at most 32
  members. A box with more than 32 kept chips (the mock "Quad Galaxy" has 128) shows the first 32
  in key order, and the view says how many chips have no floor. Hooks per member: at most
  `MAX_READY_MODELS` (64) sage hooks plus explore and rest, so at most 32 x 66 hooks in all.
  Entities held at once stay within spec 9's 500 (as built): 64 kept chips, 64 ready models, 64
  chip keys that ready models name but the saga does not keep (one shared set, ruling R31), and 32
  heroes, 224 in all. A world's chips are kept chips, so they add none.
- **Cost per tick.** Each member steps once per tick. A* runs only when a member replans and
  expands each cell at most once: at most 32 x 2,014 expansions (a 53 x 38 Blackhole floor) in a
  tick where every member replans, which happens only at a birth. The test-only work counter
  checks that a record costs under a million work units with 32 chips, birth included.
- **Snapshot cost.** The driver clones `SagaState` at most ten times a second (R7). Floors do not
  change during a world's life, so `World::floors` becomes `Arc<[Floor]>`, and a snapshot copies
  one pointer. Thirty-two heroes' paths are at most 32 x 2,014 cells, about 128 KB per snapshot.
  The plan measures it.

### 3.8 Phase 2 and the party

Phase 2 (monsters, combat, death) is not built. When it is, each monster lives on the floor of
the chip whose signal raised it, and only that floor's member fights it. A member who dies is
followed by a descendant on the same floor. `EndWhy::HeroDied` ends the world only when the last
living member dies. This addendum does not build any of that. It only fixes where it will attach.

## 4. Determinism

### 4.1 What keeps the contract

- Members act in index order, and index order is chip key order, which comes from a `BTreeMap`.
- The routine budget is a pure function of ticks and the order of beats.
- Name uniqueness compares strings in index order.
- No view state reaches the core.

### 4.2 Generators

| Generator | Seed | Stream | Used for |
|---|---|---|---|
| member 0's plan | world seed | `PLAN_STREAM` (1), as today | member 0's name and quests |
| member `i >= 1`'s plan | world seed | `PARTY_STREAM_BASE + i` (16 + i) | member `i`'s name and quests |
| the chronicle | world seed | `CHRONICLE_STREAM` (2), as today, shared by all | grammar choices, generated names, `clean_cause` |

Member 0 keeps today's stream, so one chip gives today's draws. Each member has its own stream, so
one member's extra draw (a no-path replan, say) never changes another member's walk. The
chronicle's stream is shared because the chronicle is one log. Its draws happen in beat order,
which is fixed.

## 5. The view

### 5.1 The layout: a grid of tiles

I considered three layouts.

- **A grid of tiles,** one per floor, each centred on its hero.
- **A paged strip,** one floor at a time, with `[` and `]` to change chips, as Die 3D does.
- **A mini-map plus focus:** one floor large, the others as small dots.

The user asked to see "the sagas happening on each chip at the same time". Only the grid shows
several heroes moving at once. The strip shows one. The mini-map shows dots where the user asked
for sagas. So the view is a **grid**. Two parts of the other layouts join it, for the cases the
grid cannot cover:

- **Pages,** when more floors exist than tiles fit. `[` and `]` move the focus from tile to tile,
  as they change boards in Die 3D, and the page follows the focus.
- **A roster row,** one glyph per member, so no chip is hidden even when it is on another page.

### 5.2 How N floors tile the screen

A floor is 53 x 38 cells (Blackhole) or 50 x 32 (generic), larger than a tile at 80 x 24. So a tile
draws a window of its floor, centred on its hero, the way the map pane already scrolls today. The
grid sits where the map sits today: above the chronicle under 100 columns, and to its left from
100 columns.

- **Minimum tile:** 24 columns by 7 rows (a one-line HUD and 6 map rows). A window of 23 x 6
  holds the hero's light across (it is 19 cells wide) and most of it from top to bottom (9 rows).
- **The grid's shape** comes from a pure function `party_grid(n, w, h) -> Grid`. It picks the
  column and row counts that show the most floors. On a tie it takes the larger tiles, and on a
  further tie the fewer columns. Tiles share the width and height evenly, with one blank column
  between tiles. A tile never grows past its floor's size plus its HUD.
- **The grid's area.** Under 100 columns the grid takes every row the chronicle does not need,
  and the chronicle keeps its header and at least four lines. From 100 columns the grid takes up
  to three fifths of the width, as the map pane does today, and the chronicle has the rest.
- **When even one tile does not fit,** the view draws the focused floor alone, exactly as today's
  view draws its one floor at that size (map only, or title only). The chronicle keeps its chip
  tags.

What that gives (as built; the last row of the terminal is the app's status bar, and the tiles
share every row the chronicle leaves):

| Terminal | 1 chip | 2 chips | 4 chips | 32 chips |
|---|---|---|---|---|
| 80 x 24 | today's view | 2 tiles side by side, 39 x 16 | 2 x 2 tiles, 39 x 8 | 6 tiles a page (3 x 2, 26 x 7), 6 pages, roster |
| 140 x 40 | today's view | 2 tiles, 41 x 37, chronicle right | 2 x 2 tiles, 41 x 18 | 15 tiles a page (3 x 5, 27 x 7), 3 pages, roster |
| 200 x 60 | today's view | 2 tiles, 54 x 39 | 2 x 2 tiles, 54 x 28 | 4 x 8 = 32 tiles at 29 x 7, one page |

`party_grid_gives_the_addendum_table` (`src/ui/tui/saga_party_tests.rs`) computes these from
`party_grid` and pins them. The draft of this table said 39 x 11 and 39 x 7 at 80 x 24; the built
rule gives the tiles every row the chronicle leaves (ruling R34).

### 5.3 What a tile shows

- **The HUD row,** in a dark band like today's title band: the floor tag (`F3`) in the floor's
  hue, the hero's name, a short vitality bar (`▰▰▰▱`), and the quest (`explore`, `rest`, `visit
  Llama-3.1-8B` with the core's `target_label`). The focused tile's band is brighter. The row is
  cut to the tile's width and ends in a blank column.
- **The map window,** centred on the hero: the same walls, fog, light, remembered cells, trail,
  glow, goal pulse and heat colour as today. Each floor has its own memory of lit cells.
- **No borders on the right.** Tiles are separated by a blank column. Each tile's last column is
  blank, as the map pane's is today, so a right-edge wall glyph can never end a row.

### 5.4 The rest of the screen

- **Title row:** `▌ SAGA`, the label mark, the world's id and biome, and `party of N` in place of
  today's floor and chip.
- **Roster row** (as built: when the grid has more than one page, or when chips have no floor,
  even on a single page, so no chip is hidden; ruling R34): `party` and one glyph per member in
  index order, two columns each (32 members take 64 columns). A member on the shown page is bright,
  others are dim, the focused member is drawn on its hue, and a member below half vitality is
  orange. When chips have no floor, the row ends with `+N chips without a floor`, then the page.
- **Chronicle:** as today, with a floor tag after the tone mark on each member's line: `t42 · F3
  Wren the Unthrottled sets out to explore`. The tag has the floor's hue. World lines have no tag.
  With "why" on, the cause line also names the chip's PCI address. Chip keys are hardware
  identifiers and stay visible in both name modes (today's rule).
- **Status row:** the focused hero's name, vitality and quest, as today, then `[ ] focus`, `z
  zoom` and the tick.
- **Floor hues:** a fixed cycle of eight built from the brand palette (teal `#74C5DF`, blue
  `#1B8EB1`, green `#6FABA0`, yellow `#F6BC42`, red `#FF9E8A`, orange `#FA512E`, and the `+` tints
  of teal and green). Floor `i` takes hue `i mod 8`. Through `colors::rgb`, as everything else.

### 5.5 Keys

| Key | What it does |
|---|---|
| `[` `]` | Move the focus to the previous or next floor. The page follows. |
| `z` | Zoom: draw the focused floor alone, as today's view, with the merged chronicle. `z` again returns to the grid. |
| `y`, `S`, `Esc` | As today. |

`[`, `]` and `z` are taken only while Saga is on screen. The existing `the_entry_key_is_free`
style of test checks that no handler in Saga mode already takes them. If `z` is taken, the
builder picks another key and the test names it.

### 5.6 One chip, and 80 x 24

- **One chip.** A world with one floor draws through today's path. No grid, no roster, no tags,
  no new keys shown. The plan pins four frames of a one-chip fixture on today's code before any
  change and keeps them byte-identical through the party stage.
- **80 x 24.** Every case above fits: title, the grid's rows, the chronicle's header and at least
  four lines, and the status row. The help, legend and explain overlays keep their row targets
  (Help 24, Legend 30, Explain 30 rows at 80 columns).
- **Sizes from 0 x 0 to 500 x 200** never panic and never overflow, for 1, 2, 4 and 32 chips.

## 6. Why the core simulates every floor

This is the R22 item again, from the view's side. Floors in view depend on the terminal. Two
people watching one host with different terminals would see different heroes exist. A replay at
another size would tell another story. So the core simulates every floor, and the view only
chooses which tiles to draw. Section 3.7 shows the cost stays inside the budgets.

## 7. Testing the party

- **New fixtures first.** A generator in `crates/tt-toplike-saga/tests/support/party.rs`, built
  like `busy.rs` (made-up names, made-up PCI addresses, its own small generator for wobble),
  writes a log for any chip count. It commits `solo.jsonl` (1 chip) and `party4.jsonl` (4 chips,
  under 1 MB). The 2-chip and 32-chip logs are generated in memory by the tests, because a 32-chip
  file would be about 9 MB.
- **Pinned before the change.** On today's code, the first task pins `solo.jsonl`'s digest in
  both name modes, a "story print" (every chronicle line's text and tone, and the hero's position
  at every tick, hashed), and four view frames (80 x 23 and 140 x 39, why off and on). The story
  print and the frames must stay identical through the party stage. The digest moves once, by the
  encoding change only, and the task that moves it says so.
- **Determinism:** the 4-chip and 32-chip logs give the same digest twice, and again after the
  replay host's renumbering and retiming (the existing `renumber` test, with a stride of 3).
- **Names:** the 32-chip party has 32 distinct names. A test with a stub lexicon of two names
  forces the numeral rule.
- **Pace:** over the 32-chip log, the routine lines written in each window all come from one
  floor, and every floor writes at least one routine line per 60
  seconds of ticks.
- **Privacy:** generated mode over the 4-chip and 32-chip logs leaves no fixture model name in
  any chronicle line, cause, event, `target_label` or tile HUD.
- **Bounds:** 33, 64 and 128 chips give 32 members. A record costs under a million work units
  with 32 chips.

## 8. Crossing

### 8.1 What the bus knows about links today

This is the stage's main constraint. I checked the bus crate, `src/bus/source.rs` and the
telemetry model:

- **`toplike.eth.live_mask`**, a 64-bit sample on each chip: bit `n` set means Ethernet port `n`
  has a live link (tt-smi's `ETH_LIVE_STATUS`). The source writes it only when it changes.
- **`Event::LinkChanged { chip, mask: u16 }`**, raised when that mask changes.
- **`EntityKey::eth_port(chip, port)`** exists (`eth:chip:0000:01:00.0/3`) but nothing samples it.

**Nothing on the bus says which chip and port a link connects to.** tt-smi's snapshot has
`board_info.coords` and `board_type` and no peer list. `src/animation/topology.rs` guesses a full
mesh between boards. The peer table lives in UMD's cluster descriptor
(`ethernet_connections[chip][chan] = {remote chip, remote chan}`), which UMD builds when a program
opens the devices. Saga reads only the bus and opens no device (spec 10).

So the crossing stage needs a new bus record that names both ends of a link, and a source that
writes it. Section 9, question 1, asks the user where the source should come from. The design
below holds whatever the source is.

### 8.2 Binding portals to links

- **A new bus event:** `Event::LinkPeer { port: EntityKey, peer: EntityKey }`, where both keys are
  Ethernet port keys. It says port `port` is cabled to port `peer`. A source raises it when it
  learns the pair, and again when the pair changes. It is an additive variant, so the bus crate
  goes to 0.2.0 (the same step the spec plans for `Event::Saga`, spec 6). The builder checks how a
  0.1 reader treats an unknown `t` first and adds a tolerant path if it fails.
- **Portals know their port.** `FloorSpec` gains `portal_ports: Vec<u8>`: the live ports in the
  mask at the world's birth, ascending. The maze places one portal per entry, west to east, with
  the same draws it makes today, and `Floor` gains `portals: Vec<(u8, (u8, u8))>`, each port with
  its cell. The k-th live port, counted from the lowest, gets the k-th portal from the west. The plan checks that order
  against Die 3D's Ethernet cage order. A generic floor today has two portals whatever the mask
  says. It now follows the mask too, so a generic floor's portal count can change.
- **A link table in the core.** `Links` keeps, for each kept chip, its latest live mask and, for
  each port, its peer from the latest `LinkPeer`. A portal is **bound** when its port has a peer
  whose chip has a floor in this world. A bound portal is **open** while both ends' live bits are
  set. A peer chip whose mask was never seen counts as not live.
- **Portals stay fixed for a world.** A link that comes up during a world has no portal until the
  next world, because floors never change during a world's life. A link that goes down leaves its
  portal in place, closed.

### 8.3 When a hero crosses

A crossing is a quest with the new verb `Verb::Cross` (digest code 7). Its target is the far port
(`eth:chip:.../n`). Its reason is a real signal. A `Cross` hook comes from one of two signals:

1. **A model that spans the link.** When a model is ready on both chips of an open bound link,
   each of those floors offers a `Cross` hook through that link. Its reason is the `ModelReady`
   event. The fiction: pilgrims carry the model's work across the bridge, and the hero walks
   with them.
2. **A link coming up.** When a `LinkChanged` sets the live bit of a bound portal, that floor
   offers a `Cross` hook through it. Its reason is that `LinkChanged` event.

`plan` takes a `Cross` hook after a visit to the sage and before rest. A floor keeps at most one
`Cross` hook per port. A crossing drops its hook. A link that flaps adds at most one hook per port
each time it comes up.

### 8.4 What a crossing looks like

1. The hero walks to the portal cell, like any goal.
2. **On arrival,** if the portal is still open, the hero enters the link. The event
   `HeroCrossed { world, member, from_floor, from_port, to_floor, to_port, cause }` is raised, and
   a notable beat is written on the departure floor: "F2 Wren the Unthrottled steps into the
   portal". The cause is the quest's reason.
3. **In transit** for `TRANSIT_TICKS` (3) ticks, the hero is on neither map. Both portals glow in
   the hero's colour. Both tiles' HUDs say `crossing F2 → F5`.
4. **Arrival on the far floor:** the hero stands on the far port's portal cell. A notable beat is
   written on the arrival floor: "F5 Wren the Unthrottled steps out of the portal". The hero
   dwells 30 ticks, then plans with the far floor's hooks. From then on its vitality comes from
   the far chip.
5. **If the portal closed while the hero walked to it,** the hero does not enter. A notable,
   tense beat says the portal is dark. The hook is dropped, and the hero plans again.

Each beat carries its own floor tag, so the merged chronicle shows the crossing on both sides.
The view's floor memories belong to floors, so a floor keeps its lit cells when its hero leaves.

### 8.5 When the link goes down mid-crossing

If either end's live bit clears while the hero is in transit, the crossing fails. The hero returns
to the departure portal at the next tick. The event `CrossingFailed { world, member, floor, port,
cause }` is raised, and a notable, tense beat is written on the departure floor ("the bridge
fails, and Wren is thrown back"). Its cause is the record that cleared the bit: the
`toplike.eth.live_mask` sample or the `LinkChanged` event, with its ordinal. A link that goes
down because a reset ended the world needs no rule: the world ends, and the party with it.

### 8.6 Determinism and caps for crossing

- **Determinism.** Link state comes only from records, in stream order. Transit is counted in
  ticks. Hooks are read in a fixed order. The digest's version becomes 3. It adds the tags `0x07`
  `HeroCrossed` (tick, world, member, from floor, from port, to floor, to port, cause) and `0x08`
  `CrossingFailed` (tick, world, member, floor, port, cause), and verb code 7. The end item adds
  one byte per member: 0 on a floor, 1 in transit.
- **Floors can empty and crowd.** A hero who crosses leaves its home floor. A floor can have no
  hero, and another can have several. The party still has the same members, at most 32. Section
  9, question 3, asks the user whether that is wanted.
- **Caps.** At most 32 heroes in transit. The link table holds at most 64 chips x 64 ports. Each
  floor holds at most one `Cross` hook per portal (12 on Blackhole). Crossing beats are notable,
  so they skip the routine budget. Each crossing is a quest followed by a 30-tick dwell, so the
  party writes at most about one crossing line per tick even if every hook fires.
- **Mock and replay.** The mock backend writes `LinkPeer` events for its own chips (a ring: chip
  `i`'s highest live port to chip `i + 1`'s lowest), marked `Estimated` like every mock reading.
  So `--mock` and CI see crossings, and every mock crossing carries the `~` mark (spec 3.9). On
  real hardware, crossings happen only once a real source writes `LinkPeer`. Until then the
  portals stay scenery, and the legend says the far side is unknown.

## 9. Open questions for the user

Most important first.

1. **Where should link peers come from?** The bus knows which Ethernet ports are live. It does not
   know what each port connects to, and without that a hero cannot cross on real hardware. The
   options I see: (a) a new bus source that reads the cluster descriptor file tt-metal writes when
   it opens the devices; it opens no device, but it exists only after a tt-metal run, and its
   chip ids still have to be matched to PCI addresses; (b) ask for the peer list in tt-smi's
   snapshot or in the official `tt` CLI; (c) cross only in `--mock` and in recorded logs until (a)
   or (b) exists. My default is (c) now and (a) later.
2. **A reset on one chip:** should it end the whole party's world, as today, or collapse only
   that chip's floor and give that floor a new hero? My default keeps today's rule (one world, it
   ends). Collapsing one floor would tell each chip's story more on its own, and it changes the
   lifecycle.
3. **After crossings, some floors have no hero and others have several.** Is that the story you
   want, or should heroes come home, or trade places so each floor keeps one hero?
4. **The chronicle's pace:** with many chips, I let one floor's routine lines into the chronicle
   per second and leave the rest to the tiles' HUDs. Is one floor a second right, or do you want
   more of the party in the log?

## 10. Calls I made

Each is mine and can be overturned.

- A party is one hero per floor, all born with the world, in floor order (3.1).
- A model's dungeon now spans every kept chip, with the sage only on the model's floors (3.1).
- Names stay unique by redrawing up to 8 times, then adding a Roman numeral (3.2).
- Member 0 keeps today's plan stream. Member `i` uses stream 16 + `i`. The chronicle's stream is
  shared (4.2).
- `target_label` moves into the core's `Quest` (3.3).
- World beats take the party's lowest vitality for their tone (3.4).
- `Beat` carries the floor as a field outside its text (3.5).
- A party's birth writes one `PartyBorn` line in place of one `HeroBorn` line per member (3.5).
- Routine lines share a budget: one floor per 10-tick window (3.5).
- The digest gains a version item (2 for the party, 3 for crossing) and moves all three goldens
  on purpose (3.6).
- Every floor is simulated and has a hero. No lazy floors and no birth rate limit, which closes
  the R22 deferral (3.7, 6).
- `World::floors` becomes `Arc<[Floor]>` for cheap snapshots (3.7).
- The view is a grid of hero-centred tiles, with pages and a roster row when the floors do not
  fit. `[` `]` move the focus and `z` zooms (5).
- The minimum tile is 24 x 7. Below it the view draws the focused floor alone (5.2).
- One chip draws through today's path, pinned by frames taken before the change (5.6, 7).
- Crossing needs a new additive bus event, `LinkPeer`, and the bus crate goes to 0.2.0 (8.2).
- Portals are fixed per world, follow the live mask at birth, and match ports west to east (8.2).
- `Cross` hooks come from a model that spans a link, or a link coming up (8.3).
- A crossing takes 3 ticks in transit, writes a beat on each floor, and fails back to the start if
  the link drops (8.4, 8.5).
- The mock backend writes a ring of `LinkPeer` events, marked estimated (8.6).
- Versions: the party is 0.17.0 (saga crate 0.2.0). Crossing is 0.18.0 (saga crate 0.3.0, bus
  crate 0.2.0), because it adds a bus event and a quest verb.

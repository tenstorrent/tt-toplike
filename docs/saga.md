# Saga

Saga is a view where the machine's activity becomes a story. Each chip is a floor of a dungeon
built from this box's own signals, and each floor has its own hero. The heroes quest at the same
time, and the view shows every floor at once. Worlds are born and end when the signals say so, and
each new world has a new maze. One chronicle beside the maps tells what happens on every floor, and
every line of it has a cause in the signal stream.

Press `S` in any view to open Saga, or start in it with `tt-toplike --mode saga`. This is phase 1
with the party and crossing (0.18.0): a hero on every chip's floor, their quests, worlds that begin
and end, the merged chronicle, and heroes who step through an Ethernet portal into the floor of the
chip at the other end of the link. Monsters, treasure and fights come in phase 2.

The design is in `docs/superpowers/specs/2026-11-02-saga-design.md`, and the party in its addendum,
`docs/superpowers/specs/2026-11-03-saga-party-design.md`.

## How a world is built

Each chip is one floor of the dungeon.

* **The footprint follows the chip.** A Blackhole chip's floor is its tile grid, 17 by 12 tiles,
  each tile drawn as a block of cells. Harvested Tensix columns (from `toplike.enabled_tensix_col`)
  become ruins: fallen walls the hero can walk across. Four GDDR vaults sit on the west edge and
  four on the east, one portal sits on the north edge for each live Ethernet link (from
  `toplike.eth.live_mask`), and the PCIe gate is on the south edge. The hero enters at the gate.
* **Other chips get a default floor.** The bus carries no tile layout for Wormhole or Grayskull, so
  every chip that is not Blackhole gets a grid of 16 by 10 tiles with two vaults on each side and
  two portals.
* **The seed places the rooms.** At a world's birth the saga hashes the last 64 sample values it
  has seen, each as its whole 64-bit float pattern, into a 64-bit seed. Readings vary in
  different bits: a temperature or power reading (a 32-bit float widened to 64 bits) in the middle
  of the pattern, a counter or clock in its high bits, a native 64-bit rate in its low bits.
  Keeping every bit lets any of them change the world, so two runs on one box start with
  different worlds. The seed decides where rooms and corridors go, and each chip's key is mixed
  in, so two chips of the same shape get different floors.
* **Every room is reachable.** A spanning tree of corridors joins the rooms, and a few extra
  corridors add loops. The exit is the room farthest from the gate.

The code is in `crates/tt-toplike-saga/src/topology.rs` (the footprint) and `maze.rs` (rooms and
corridors).

## When worlds begin and end

There is always one world once the first is born. The first world is born at the first tick at
which the saga has seen 64 samples, or at the fifth tick, whichever comes first, so the seed is
never taken from an almost empty pool. After that, signals decide:

| Signal | What happens |
|---|---|
| A model becomes ready (`ModelReady`) | The world ends and a dungeon is born on every chip, with a sage for the model on the floors of the chips the model names (on every floor when it names none). |
| That model goes away (`ModelGone`) | Its dungeon collapses and a dungeon on every chip follows. A model that did not make the current world can go away without ending it. |
| A chip resets (`Reset`) | If the reset names one of the world's chips, or names no chip, the world collapses and a dungeon on every chip follows. |
| A training run starts (the first `toplike.train.step`) | A forge is born on every chip. When no training sample has arrived for 600 ticks, the run is over and a dungeon follows. |
| A quiet season (20 minutes by default) | The world fades and a dream is born. A dream that stays quiet fades into another dream, so the cycle never stops. |

Any bus event except `ViewChanged` counts as activity, and so does any `toplike.train.*` sample.
Ordinary samples do not count, because a quiet machine's readings still wobble. The rules are in
`crates/tt-toplike-saga/src/world.rs`.

## The cast in phase 1

* **The heroes (`@`).** One per floor, born with the world at their floor's gate and named from the
  lexicon. Each walks with A* pathfinding and picks its quests on its own: visit the sage of a
  ready model, explore a room, or rest. Its vitality comes from its own chip's heartbeat, clock and
  temperature. Below half, it is drawn orange and its next quest is a rest. After reaching a goal it
  waits 30 ticks before it plans the next quest. A hero can cross an Ethernet portal into another
  chip's floor (see "Crossing").
* **Sages (`Ψ`).** One for each model that is ready when the world is born, on the floors of the
  chips the model names. The sage is drawn at the goal while a hero is on its way to visit it.
* **Beats.** Each chronicle line is a beat, written from the pack's sentence shapes in one of three
  tones: calm, tense or triumphant. The tone follows the machine's state: tense when the hero is
  weak or a world collapsed, triumphant for a quest done and for the birth of a world from a ready
  model.

Monsters (from ECC errors, link changes, resets, thermal trips and stalls), other characters and
treasure come in phase 2.

## The party

A world has one floor per chip, up to 32 in chip key order, and one hero per floor. Hero `i` is born
on floor `i`. On a box with one chip, the view and the story are the same as before the party
(0.16): the same frames and the same chronicle.

* **Names.** Each hero draws its name from its own generator. A name that a hero on a lower floor
  already has is drawn again, up to 8 times; after that the hero keeps it with a Roman numeral
  ("Wren the Unthrottled II"). No two heroes in a party share a name.
* **Quests.** Each hero has its own list of quests and its own generator, so one hero's choices
  never change another hero's walk. A hero visits only the sages on its own floor, and a model
  going away sends every hero walking to its sage to plan again.
* **The pace of the chronicle.** With many heroes, every quest started and finished would put one
  or two lines in the chronicle each tick, too fast to read. So a hero's routine lines (a quest
  started or finished, a rest, an ambient line) share one budget. Ticks fall into windows of 10
  (one second at the default interval), and the first floor to write a routine line in a window
  owns it: only that floor's routine lines are written in that window. A hero that has written
  nothing for 300 ticks gets the next open window first, so on the 32-chip test log every floor
  writes a line at least once a minute, and the chronicle is never silent for more than 26 ticks. A world's birth opens
  the window again. Lines about the world (born, ended, the party's birth) are always written. A
  line that is not written still raises its event, so the digest and every reader of the events
  see every quest, and the tile of that hero still shows its quest.
* **The party's birth.** With two or more floors, a birth writes the world's line and one line for
  the whole party ("The floors tremble, and four heroes scramble to their feet."), in place of one
  line per hero.
* **Chips past 32 have no floor.** A box with more than 32 chips (the mock "Quad Galaxy" has 128)
  gets floors for the first 32 in key order. The roster row says how many chips have no floor.

### The party view

A world of two or more floors draws as a grid of tiles, one per floor. Each tile is a HUD row over
a window of its floor's map, with the same walls, fog and trail as the single-floor view. Under 100
columns the grid sits above the chronicle, which keeps its header and four lines; from 100 columns
it takes up to three fifths of the width, left of the chronicle.

The tiles move less than the single-floor view, so that the terminal is not sent more than the
view's budget (20 KB/s at 140 x 40):

* **A window moves in steps.** It stays put while the hero walks inside a dead zone around its
  centre (9 columns to each side, 3 rows above and 2 below), and moves when the hero steps out of
  it, so that the hero then stands at the zone's far side. The hero can stand on a tile's top or
  bottom row.
* **The focused tile** has the light, the glow around the hero, the trail, the pulsing goal and
  the portals' shimmer, with the light's falloff and the trail's fade in three shades each.
* **The other tiles are calm.** Everything they have seen is drawn in one colour (the floor's,
  with its heat) at one light level, with no glow; the goal, the sage and the portals hold still;
  and only the newer half of the trail shows, in one colour. Each draws a picture of its floor
  taken every five ticks, so its hero hops up to five cells at a time. A hero that has crossed
  away can still show on such a tile for up to five ticks, under a HUD that already says `no hero
  here`.
* **The heat tint** of a tile changes when its chip's temperature has moved 3 °C.
* **The chronicle** shows the lines up to the last multiple of 20 ticks, so new lines arrive in a
  batch about every two seconds. A stopped story shows every line.

* **Tile HUD:** `▌F3`, the hero's name, a four-cell vitality bar (`▰▰▰▱`) and the quest (`explore`,
  `rest`, `visit` and the model's name). The band is tinted with the floor's hue, and the focused
  tile's band is brighter with its tag reversed. A narrow tile drops the hero's title first, then
  the quest's target. Only the focused tile marks a reached goal with `✓`.
* **Floor hues:** a cycle of eight from the brand palette (teal `#74C5DF`, blue `#1B8EB1`, green
  `#6FABA0`, yellow `#F6BC42`, red `#FF9E8A`, orange `#FA512E`, and the lighter teal `#9BDBF0` and
  green `#92C9BF`). Floor `i` takes hue `i mod 8`. The tile band, the chronicle tag and the roster
  glyph share it.
* **Title row:** `party of N` where the single-floor view names the floor and chip.
* **Roster row:** shown when the floors need more than one page, or when some chips have no floor.
  `party`, then one glyph per hero, two columns each: bright on the page shown, dim on other pages,
  orange below half vitality, and drawn on its hue when focused. Then `+N chips without a floor`
  and the page (`page 2 of 6`, or `2/6` when the row is short).
* **Chronicle:** each hero's line has its floor's tag after the tone mark (`t42 · F3 Wren sets out
  to explore`). World lines have no tag. With `y` on, the cause line of a hero's line ends with the
  chip of its floor (`· F3 is chip 0000:03:00.0`).
* **Status row:** the focused hero's name, vitality and quest, then `[ ] focus  z zoom` (`[ ] z`
  when the row is short), the tick and the mapped share.
* **Pages and zoom.** `[` and `]` move the focus from floor to floor, and the page follows the
  focus. `z` draws the focused floor alone, as the single-floor view draws it, with the merged
  chronicle; `z` again returns to the grid. When not even one tile fits (the smallest tile is 24
  columns by 7 rows), the view draws the focused floor alone.

What the grid gives (the view's area, without the app's status bar; pinned by
`party_grid_gives_the_addendum_table` in `src/ui/tui/saga_party_tests.rs`):

| View area | 2 chips | 4 chips | 32 chips |
|---|---|---|---|
| 80 x 23 | 2 tiles of 39 x 16 | 2 x 2 tiles of 39 x 8 | 3 x 2 tiles of 26 x 7 a page, 6 pages |
| 140 x 39 | 2 tiles of 41 x 37 | 2 x 2 tiles of 41 x 18 | 3 x 5 tiles of 27 x 7 a page, 3 pages |
| 200 x 59 | 2 tiles of 54 x 39 | 2 x 2 tiles of 54 x 28 | 4 x 8 tiles of 29 x 7, one page |

One chip draws the single-floor view at every size.

## Crossing

A hero can leave its floor through an Ethernet portal and arrive on the floor of the chip at the
other end of the link. This needs the link's far end to be known. The bus event `LinkPeer` names
both ports of a link (`eth:<chip>/<n>`), and the saga keeps a link table from those events.

* **Portals are bound to ports.** The portals on a floor's north edge are listed west to east and
  match the chip's live Ethernet ports in port order. A portal with a `LinkPeer` record is bound
  to its far port. A portal with none stays scenery, and no hero crosses through it.
* **Portal colours.** A bound, open portal is drawn bold in the hue of the floor on its far side.
  A bound portal whose link is closed is drawn dim, in a mix of that hue and the base colour. A
  portal with a hero in transit through either end glows yellow. An unbound portal keeps the old
  teal shimmer. A link is open while both of its ends are live in their chips' live masks.
* **When a hero crosses.** A model that names both chips of a link gives each floor a crossing
  quest through that link, and a link coming up gives its chip's floor one. A hero plans a
  crossing after a sage visit and before rest or exploring. It walks to the portal and, if the
  link is open, steps in. If the link is closed when it arrives, the quest ends with a line that
  the portal has gone dark, and the hero takes another quest.
* **In transit.** A crossing takes 3 ticks. The hero is on no floor then: both tiles say `crossing
  F2 → F1` in their HUD, the roster marks the hero with `»`, and both portals glow. If the link
  drops while the hero is in transit, the hero is thrown back out of the portal it left from, and
  the chronicle says so. Otherwise it arrives on the far portal, takes its vitality from the far
  chip at once, and dwells 30 ticks before it plans again.
* **The quest text.** The chronicle writes a line on the floor the hero leaves, one on the floor it
  reaches, and one for a dark portal or a failed crossing. Floors are counted from 1, like the
  `F2` tags. For example: "Kestrel the Checkpointed steps into the portal on port 0 and heads for
  floor 1. ~", "Sorrel the Many-Headed steps out of the portal on port 0, arriving from floor 1.
  ~", "... reaches the portal on port 1, but it has gone dark. The way to floor 2 is closed.", and
  "The link to floor 2 fails mid-crossing, and ... is thrown back out of the portal on port 1." The
  quest's start line ("heads for the portal to F2") is routine and may be dropped by the pace.
  The status row says `heading for the portal to F2`, `crossing F2 → F1`, `arrived on F1 through
  a portal` or `thrown back at the portal: the link went down`.
* **The `~` mark.** A crossing line ends in ` ~` when the last live-mask sample of either chip of
  the link was an estimate. Every `--mock` reading is one. The mark is part of the line's text, so
  it is in the digest and a replay keeps it.
* **Empty and crowded floors.** A hero who crosses leaves its home floor. A floor can have no hero,
  and its tile says `no hero here` and keeps its memory. A floor can also have several heroes, and
  the tile draws all of them. The party keeps the same members, at most 32.
* **Calm tiles lag.** A tile that is not focused draws a held picture every 5 ticks, so a hero who
  crossed away can still show on it for up to 5 ticks after its HUD says `no hero here`.
* **The far side is unknown on real hardware until a link source exists.** Only the mock backend
  (a ring: each chip's highest live port in the low 12 to the next chip's lowest live port, marked
  as an estimate) and recorded logs carry `LinkPeer` today. No real backend writes it, so on real
  hardware every portal is unbound and no hero crosses. A real source is future work. A plain
  `--mock` run binds its portals but never crosses: its live masks never change and it announces
  no model, so no crossing quest is offered. Crossings appear in recorded logs that carry a model
  on two linked chips or a link coming up, such as the `cross4.jsonl` test fixture.
* **Limits.** At most 32 heroes are in transit. The link table holds at most 64 chips by 64 ports.
  A floor holds at most one crossing hook per portal. Crossing lines skip the routine pace, and a
  hero dwells 30 ticks after each crossing, so the party writes at most about one crossing line per
  tick.

## What the map shows

| Glyph | What it is |
|---|---|
| `@` | The hero, bold on yellow, or on orange when its vitality is below half. The cells around it glow. |
| `•` | The hero's trail over its last 12 steps. It fades with age, and the oldest steps show as floor. |
| `Ψ` | A sage, at the goal while the hero is on its way to visit it. It pulses from teal to green. |
| `◈` `◆` | The goal of any other quest. The two glyphs take turns as it pulses. |
| `─ │ ┌ ┬ ┼ …` | Walls inside a floor, drawn as thin lines that join the walls beside them. |
| `═ ║ ╔ ╤ ╟ …` | The floor's outer shell, in double lines. `╤`, `╧`, `╟` and `╢` mark where an inner wall meets it. |
| `▪` | A pillar: a wall cell that joins no other wall. |
| blank | Solid rock (a wall no walkable cell touches), or a cell the hero has not seen yet. |
| `·` | Floor: rooms and corridors. |
| `░` | A ruin: a fallen wall over a harvested Tensix column or row. The hero can walk across it. |
| `▯` | A door, where a corridor meets a room. The wall lines run up to it. |
| `▣` | A GDDR vault, on the west or east edge. |
| `◎` | A portal for a live Ethernet link, on the north edge. An unbound portal shimmers in teal. A bound one is bold in the far floor's hue when its link is open, dim when closed, and yellow with a glow while a hero crosses (see "Crossing"). |
| `⌂` | The PCIe gate on the south edge, where the hero enters. |
| `▼` | The exit: the centre of the room farthest from the gate. |

### The light and the memory

The hero carries a light. A cell is lit when it lies within about nine columns and four rows of the
hero (an ellipse, because a terminal cell is twice as tall as it is wide) and no wall stands on the
straight line between them. The whole room the hero stands in is lit too, walls included. Lit cells
are brightest next to the hero.

The view remembers every cell that has been lit and draws it in the same colour at a low
brightness. Each floor has its own memory. A cell that has never been lit is blank, so the map
fills in as the hero explores, and the status row says how much of the floor's walkable cells are
mapped. The quest's goal is always drawn, so the hero's destination shows through the dark.

The memory belongs to the view, and the saga core never sees it, so it cannot change a digest. The
TUI updates it on every backend update in every view, so the map is still there when you come back
to Saga. If updates were missed and the hero moved more than one cell, the view fills in the walk
with the core's own shortest path. Every floor's memory starts over when a new world is born.

## The chronicle and the why key

The chronicle keeps the last 500 lines. Each line starts with the tick it was written at (`t42`)
and a mark for its tone: `·` calm in teal, `▲` tense in orange, `★` triumphant in yellow. The
names of heroes, places, sages and monsters are bold. Older lines are drawn a little dimmer.
Press `y` to show what set each line off, in a sentence under it: `why: this goes back to the model
mock-llama coming online (record 379)`. The line says "goes back to" because the cause is the signal
the hero had in front of it when it chose its quest, so several lines in a row can share one cause.
When nothing set the hero's next move off, the line says so: `why: nothing set this off, so the hero
follows its own curiosity near chip 0000:01:00.0`. The record number is the signal's place in the stream the core
was fed. Signals the view has no words for are shown by name (`reporting a new fan rpm`).

## The same records give the same saga

The saga core is a pure library with no clock, no I/O and no terminal. Its contract: **the same
ordered stream of records gives the same saga.**

* **Time is counted in sweeps.** The backend sweeps every chip once per update. The first chip
  whose sweep the saga sees is its clock, and each sweep of that chip is one tick. A season is a
  number of ticks. Only a chip can be the clock: a process scan also sends sweeps, but only every
  2 seconds. If the clock's chip stops sweeping (it went away, or `b` switched the backend and
  every chip key changed), the saga notices a missed update each time another chip sweeps a second
  time since the clock last did. After three missed updates the chip that noticed the third becomes
  the clock, so there is still one tick per update. After a backend switch this takes one update
  more, because the first update of new keys repeats no chip. The timeout counts updates, never
  time.
* **Records about the TUI itself play no part.** A TUI that hosts its bus publishes a `ViewChanged`
  when its first frame is drawn and on every view key, and a replay host publishes a gap at each
  hole it finds in the log. Their positions depend on key presses and timing, so the saga skips
  them, as it skips the bus's own disk-outage gaps.
* **Sequence numbers and timestamps play no part.** The saga reads only each record's source and
  payload. A replay host numbers and times every record afresh, and the story is still the same.
* **Records from source `saga` are skipped**, so a recording that carries the saga's own output
  never doubles it.

Check it with the real binary. `--saga-digest` feeds every record of a recorded log to a fresh saga
and prints its digest, a hash of every world seed, every event, each line's floor, the chronicle's
text and where each hero ends:

```
$ tt-toplike-tui --bus-inspect /var/tmp/run1 --saga-digest
saga digest: 0xad8cec85d321c8a0
worlds born: 1, beats written: 10, records: 9048 (skipped 0 bad lines), holes: 0
```

That output is from a 12-second `--headless --mock 4` recording (0.18.0). The digest uses the
replay label, real names and the season the log's header records (see "Config"), so the reader's
own config does not change it.
Record a run, replay it into a second recording with `--replay`, and the two recordings print the
same digest. A second 4-chip recording of the same length printed `0x6f82483e31a9010c`: each run
starts with its own world. The digest's encoding has a version; 0.17.0 writes version 2, which adds
the floor of each line and event and one end position per hero, so a digest printed by 0.16 differs
from one printed by 0.17 on the same log. 0.18.0 writes version 3. It adds the tags `0x07` (`HeroCrossed`) and `0x08`
(`CrossingFailed`) and the quest verb code 7, and marks a hero in transit with bit `0x80` of its
floor byte in the end item (a floor index is below 32, so the bit is free), so a digest from 0.17 differs from one from 0.18 on the same log. A live saga labels its first line "(mock)" or not at all, and the digest's saga
labels it "(replay)", so a live saga's own digest differs from this one on the same records. Exit
status: 0 when the digest was printed (or the output was a closed pipe), 1 when the log cannot be
read or the output fails.

## Labels

A saga fed by `--mock` writes "(mock)" in its title and its first chronicle line, because the mock
backend's readings are estimates. A saga fed by a replayed log writes "(replay)".

## Keys

| Key | What it does |
|---|---|
| `S` | Opens Saga from any view. In Saga, goes back to the view you came from. |
| `y` | Shows or hides what set each chronicle line off. |
| `[` `]` | With two or more floors: moves the focus to the previous or next floor. The page follows the focus, and the status row shows the focused hero. |
| `z` | With two or more floors: draws the focused floor alone, as the single-floor view draws it. `z` again returns to the grid. |
| `Esc` | Leaves Saga for the view you came from. |
| `/saga`, `/mode saga` | Opens Saga from the command bar. |
| `l`, `?`, `!` | The legend (every map glyph, the light and the chronicle's marks), the help and the explain text, as in every view. |

`[`, `]` and `z` are taken only while Saga is on screen; in other views they keep their own meaning.
With one floor they do nothing. Saga is not in the `v` cycle and not in `--rotate`.

## The arcade panel

Saga can also be a panel of the arcade (`a`). The built-in layout `now-serving` shows it on the
right of its bottom row, beside the Wafer Waffler and under the inference server, and any
`[[arcade.layouts]]` table can name it with `view = "saga"` (see `docs/arcade-layout.md`). The
rule over it says `Ψ SAGA`.

The panel draws the same story with the same map renderer, so the fog, the hero's glow and trail,
the biome colours and the heat are the view's own. With a party it draws the lead hero's floor
(floor 1, the hero born on the first chip) and that floor's memory, so its map is the full view's
map of that floor.

| Panel size | What it shows |
|---|---|
| under 20 x 6 | nothing: the panel is taken out and its rule says `◌ SAGA needs 20x6` |
| 20 x 6 and up | the HUD row over the map |
| 60 x 12 and up | a chronicle strip of the newest lines under the map, a third of the rows |
| 100 columns and up (and 12 rows) | the chronicle strip right of the map, as in the full view |

The HUD is the full view's status row after the label: `(mock)` or `(replay)` when the story has
one, the hero's name, the `▰▱` vitality bar, the quest, how much of the floor is mapped, and the
tick. The segments at the end are dropped when they do not fit. A panel too narrow for the row up
to the end of the quest gets a short form: the label, the hero's name, its vitality as a
percentage and the quest, for example ` (mock) Yarrow 99% • exploring` in the 39-column panel of
`now-serving` at 80x24. The short form drops the hero's title ("the Well-Cached") when the whole
name does not fit.

When there is no map to draw, the panel says why on one line, wrapped when the panel is narrow:

| State | The note |
|---|---|
| no world yet | `saga waiting for the first world` |
| `[saga] enabled = false` | `saga is off: set [saga] enabled = true in config.toml` |
| on, but the driver did not start | `saga is not running; the log says why` |
| the driver stopped before the first world | `the story stopped before its first world` |

A driver that stops after a world is born shows the full view's banner, `the story has stopped ·
showing its last state`, under the HUD and over the last map.

* `y` does nothing in the arcade, so the strip has no `y` hint. Press `S` for the full view.
* Generated names (`[saga] names = "generated"`) reach the panel exactly as they reach the view,
  because both draw the same beats, causes and quest text.
* The panel takes no backdrop, as the view takes none.
* No row of the panel ends in a right-edge border glyph: the map leaves its last column blank and
  the strip's header ends in `═`.
* Cost: the panel feeds once per update. The TUI hands the arcade the driver's state (an `Arc`, so
  no copy) and copies its map memory into the panel's buffers, which allocates nothing once the
  floor is known. A frame of the panel at 70 x 16 took 66 µs in a release build, against 54 µs for
  the full view at 70 x 16 and 183 µs for both at 140 x 39 (`panel_cost` in
  `src/ui/tui/saga_view_tests.rs`, a shared box with a load average near 11).

## Config

`[saga]` in `~/.config/tt-toplike/config.toml`:

| Key | Values | Default |
|---|---|---|
| `enabled` | `true` or `false`. `false` starts no saga thread at all. | `true` |
| `names` | `"real"` or `"generated"`. Any other value is reported and hides names (`"generated"`). | `"real"` |
| `season_minutes` | whole minutes of quiet that end a world, 1 to 1440 | `20` |

```toml
[saga]
enabled = true
names = "generated"
season_minutes = 30
```

The story runs from start-up whether or not the view is on screen, so it has no gaps. Minutes become
ticks with `--interval` (one tick per backend update), and the host that opens the bus writes the
result into the bus header as the tag `saga.season_ticks`. A headless host reads `[saga]` for this
too. Every saga that reads the bus takes its season from that tag: the TUI's own, a TUI attached to
a headless host, a `--replay` of the recording (a replay host copies the recorded tag), and
`--saga-digest`. So a recording made with `season_minutes = 5` or `--interval 200` tells the same
story when it is replayed or digested on a box with other settings. A log recorded before the tag
existed uses the default, 20 minutes at 10 updates a second; an attached TUI whose host is too old
to send the tag uses its own config at 10 updates a second. `tt-toplike --check-config` prints the
saga's settings and every problem in the section.

## Privacy

* Command lines never appear. A name from the box is accepted only as a single token: no spaces, no
  control characters, no `=`, no leading `-`, at most 32 characters.
* With `names = "generated"`, every model, process and service name is replaced by a name from the
  lexicon, in the chronicle's text, in each line's cause, and in the events the core raises. A name
  keeps its alias from line to line until the alias table reaches 256 names and is emptied.
* Chip, tile, DRAM channel and Ethernet port keys stay as they are in both modes. They are PCI
  addresses and indexes: hardware identifiers that name no person or program.
* Saga reads only the bus. It opens no device and runs no command.

## Limits

* Any local process can write to the bus. A writer that sends samples for chip keys that sort
  before the real ones (`chip:0` sorts before `chip:0000:01:00.0`) fills the floors before the
  real chips: the saga keeps the first 64 chips it sees and a world's first 32 in key order. The
  cost is bounded, and the dungeon then shows made-up chips.
* No person has run Saga on real Tenstorrent hardware yet. It has run on the mock backend and on
  recorded fixtures.
* Chips past 32 have no floor and no hero. The roster row counts them (`+N chips without a floor`).
* On real hardware no source writes `LinkPeer` yet, so no portal is bound and no hero crosses.
  The Ethernet portals on each floor's north edge are scenery there (see "Crossing").
* A real name longer than 32 characters is cut to its first 32, in the chronicle and in a tile's
  quest label alike. Two models whose ids share their first 32 characters show the same name.
* On tiles 26 to 29 columns wide the HUD shows only the hero's first name, so two tiles can both
  say "Lark"; the floor tags tell them apart. Tiles are separated by one blank column, so the maps
  of neighbouring tiles can look joined.
* Terminal traffic, measured with `python3 tools/party_traffic.py <binary> --runs 3 -- --mock N
  --mode saga` (a 140 x 40 pty, bytes written from 20 s to 40 s of a 40 s run): `--mock 32`, whose
  heroes cross the mock's ring of links, sends about 18 KB/s (17.5, 18.1, 18.7, 17.7 and 19.6 KB/s
  in five runs), inside the 20 KB/s budget; before the calmer tiles it sent about 185 KB/s. With
  crossings and a held picture every three ticks single runs reached 22 KB/s, so an unfocused tile
  now holds its picture for five ticks. One chip sends about 16 KB/s (16.0, 16.8 and 13.8), as
  before. The margin is small: a busier story with more walking can pass 20 KB/s. The test
  `the_party_view_stays_inside_its_traffic_budget` bounds the bytes a frame on two 32-chip test
  logs, one with no links and one in which every link of a ring offers crossings.
* The calm tiles show no light around their heroes, so a hero on an unfocused tile is easiest to
  find by its `@` and its trail.
* Generated-name aliasing was checked on the busy fixture only.
* A log directory reused by sessions with different seasons keeps the first segment's tag, so
  `--saga-digest` and `--replay` of the whole directory use the oldest session's season.
* Monsters, treasure, fights, the hero's death and descendants, and the `saga.*` bus events come in
  phase 2.
* The hero cannot be steered yet (phase 3).
* Saga is not a stop in `--rotate`, and the shared backdrop is not drawn behind it or behind its
  arcade panel.
* The lexicon names some monsters after chips and parts, for example "Blackhole Flare", "Tensix
  Torch Moth" and "Clogged NoC". Whether chip names should name monsters is a decision for the
  project.

## The lexicon and the look

The pack (`crates/tt-toplike-saga/src/pack.rs`) holds the words, sentence shapes, glyphs and
colours. Its word groups are tagged by what they stand for (`npc:model`, `place:vault`,
`monster:thermal`, `hero:name`, `lore:concept` and more), so every name fits its meaning. The
words come from:

* Tenstorrent and its chips and tools: Blackhole, Wormhole, Grayskull, Tensix, the NoC, TT-Metalium,
  TTNN, tt-smi and others.
* AI and ML: attention, softmax, the KV cache, tokens, gradients, loss, checkpoints and others.
* Real names from the box, in real-name mode: model, process and service names.

The colours are the Tenstorrent brand palette: dark forest teal `#092221`, blue accent `#1B8EB1`,
teal `#74C5DF`, green `#6FABA0`, yellow `#F6BC42` and orange `#FA512E`.

* **Biomes.** A dungeon has teal floors and blue walls, a forge yellow floors and orange walls, and a
  dream green floors and teal walls. The outer shell is bold.
* **Heat.** The TUI reads the temperature of each floor's chip from its backend, matched by bus
  address. Above 45 °C the floor's colour moves towards yellow at 65 °C and orange at 85 °C. A
  replay or an attached TUI whose backend has no matching chip draws the biome's own colour.
* **Colour depth.** Every colour goes through the app's colour function, so a terminal without
  truecolor (and every tmux session) gets the nearest 256-colour shade, as the other views do. The
  remembered colours are chosen so they do not fall to black there. The glyph shapes alone still
  tell walls, floors, doors and the hero apart on a 16-colour terminal.
* **Glyphs.** `Glyphs` in `pack.rs` holds every map role's character and the four wall tile
  tables. Every glyph is one column wide. The role glyphs are never a right-edge
  border shape. The wall tiles include some (`┐`, `┤`, `╗`), so the map leaves its pane's last
  column blank, and no row of the view can end in a border on the right.
* **Animation.** The goal's pulse, the sage's pulse and the portals' shimmer follow the view's own
  frame counter, which moves once per update while Saga is on screen. No clock is read.

## Architecture

* `crates/tt-toplike-saga`: the pure core. `Saga::feed` takes records in order. No clock, no I/O.
* Its `clippy.toml` bans wall clocks, environment reads and hash-ordered collections.
* `src/saga/driver.rs`: a background thread that feeds the core from the bus, local or attached.
* The driver publishes a snapshot of the state at most 10 times a second.
* `src/saga/config.rs`: the `[saga]` section as the core's config. `src/saga/digest.rs`: `--saga-digest`.
* `src/ui/tui/saga_view.rs`: the view draws the snapshot and never feeds the core. Its
  `PartyMemory` lives in the TUI's loop: one `SagaMemory` per floor (the lit cells, a trail for
  each hero on the floor, and the heat of the floor's chip). A world of one floor draws as
  before; with more floors `src/ui/tui/saga_party.rs` draws a tile per floor, each from its own
  memory. The focus and zoom are view state in the TUI's loop and never reach the core.
  `render_saga_panel` draws the arcade panel from the same pieces (`map_rows`, the status row,
  `chronicle_rows`); the arcade keeps the last `PanelFeed` and a copy of the memory
  (`ArcadeVisualization::set_saga`).
* Cost with 32 chips: every floor is simulated and has a hero, whatever the view shows. A test-only
  counter (`src/cost.rs` in the saga crate) counts work units: maze cells built, A* cells expanded,
  trail pushes and events. On the 32-chip test log the worst record is a 32-floor birth at 64,520
  units, 6.5% of the million-unit budget, and a tick costs 391 units on average. A hero's step
  along its path is not a work unit, so the 32 steps of each tick are not in these figures; a step
  moves one cell and costs the same on any floor.

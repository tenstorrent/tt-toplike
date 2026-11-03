# Saga

Saga is a view where the machine's activity becomes a story. A hero quests through a dungeon built
from this box's own signals. Worlds are born and end when the signals say so, and each new world
has a new maze. A chronicle beside the map tells what happens, and every line of it has a cause in
the signal stream.

Press `S` in any view to open Saga, or start in it with `tt-toplike --mode saga`. This is phase 1:
a hero, its quests, worlds that begin and end, and the chronicle. Monsters, treasure and fights come
in phase 2.

The design is in `docs/superpowers/specs/2026-11-02-saga-design.md`.

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

There is always one world once the first is born. The first world is born at the first sweep at
which the saga has seen 64 samples, or at the fifth sweep, whichever comes first, so the seed is
never taken from an almost empty pool. After that, signals decide:

| Signal | What happens |
|---|---|
| A model becomes ready (`ModelReady`) | The world ends and a dungeon is born on the model's chips, with a sage for the model. |
| That model goes away (`ModelGone`) | Its dungeon collapses and a dungeon on every chip follows. A model that did not make the current world can go away without ending it. |
| A chip resets (`Reset`) | If the reset names one of the world's chips, or names no chip, the world collapses and a dungeon on every chip follows. |
| A training run starts (the first `toplike.train.step`) | A forge is born on every chip. When no training sample has arrived for 600 sweeps, the run is over and a dungeon follows. |
| A quiet season (20 minutes by default) | The world fades and a dream is born. A dream that stays quiet fades into another dream, so the cycle never stops. |

Any bus event except `ViewChanged` counts as activity, and so does any `toplike.train.*` sample.
Ordinary samples do not count, because a quiet machine's readings still wobble. The rules are in
`crates/tt-toplike-saga/src/world.rs`.

## The cast in phase 1

* **The hero (`@`).** One per world, named from the lexicon. It walks with A* pathfinding and picks
  its quests on its own: visit the sage of a ready model, explore a room, or rest. Its vitality
  comes from its chip's heartbeat, clock and temperature. Below half, it is drawn orange and its
  next quest is a rest. After reaching a goal it waits 30 sweeps before it plans the next quest.
* **Sages (`&`).** One for each model that is ready when the world is born. The sage is drawn at the
  goal while the hero is on its way to visit it.
* **Beats.** Each chronicle line is a beat, written from the pack's sentence shapes in one of three
  tones: calm, tense or triumphant. The tone follows the machine's state: tense when the hero is
  weak or a world collapsed, triumphant for a quest done and for the birth of a world from a ready
  model.

Monsters (from ECC errors, link changes, resets, thermal trips and stalls), other characters and
treasure come in phase 2.

## The chronicle and the why key

The chronicle keeps the last 500 lines. Each line starts with the sweep it was written at (`t42`).
Press `y` to show each line's cause under it: the entity, the signal, and the record's position in
the stream (`why: chip:0000:01:00.0 · tt_asic_temperature_celsius · #1234`). A hero resting with
nothing to do has no signal behind its rest, and its line says `no signal`.

## The same records give the same saga

The saga core is a pure library with no clock, no I/O and no terminal. Its contract: **the same
ordered stream of records gives the same saga.**

* **Time is counted in sweeps.** The backend sweeps every chip once per update. The first chip
  whose sweep the saga sees is its clock, and each sweep of that chip is one tick. A season is a
  number of ticks. Only a chip can be the clock: a process scan also sends sweeps, but only every
  2 seconds. If the clock's chip misses three updates in a row (it went away, or `b` switched the
  backend and every chip key changed), the next chip to sweep becomes the clock, so there is still
  one tick per update. The timeout counts updates, never time.
* **Records about the TUI itself play no part.** A TUI that hosts its bus publishes a `ViewChanged`
  when its first frame is drawn and on every view key, and a replay host publishes a gap at each
  hole it finds in the log. Their positions depend on key presses and timing, so the saga skips
  them, as it skips the bus's own disk-outage gaps.
* **Sequence numbers and timestamps play no part.** The saga reads only each record's source and
  payload. A replay host numbers and times every record afresh, and the story is still the same.
* **Records from source `saga` are skipped**, so a recording that carries the saga's own output
  never doubles it.

Check it with the real binary. `--saga-digest` feeds every record of a recorded log to a fresh saga
and prints its digest, a hash of every world seed, every event and the chronicle's text:

```
$ tt-toplike-tui --bus-inspect /var/tmp/run1 --saga-digest
saga digest: 0x1ec90317adb4fa0a
worlds born: 1, beats written: 6, records: 6813 (skipped 0 bad lines), holes: 0
```

That output is from a 12-second `--headless --mock` recording. The digest uses the replay label,
real names and the default 20-minute season, so the reader's own config does not change it.
Record a run, replay it into a second recording with `--replay`, and the two recordings print the
same digest. A live saga labels its first line "(mock)" or not at all, and the digest's saga
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
| `y` | Shows or hides each chronicle line's cause. |
| `Esc` | Leaves Saga for the view you came from. |
| `/saga`, `/mode saga` | Opens Saga from the command bar. |
| `l`, `?`, `!` | The legend, the help and the explain text, as in every view. |

Saga is not in the `v` cycle and not in `--rotate`.

## Config

`[saga]` in `~/.config/tt-toplike/config.toml`:

| Key | Values | Default |
|---|---|---|
| `enabled` | `true` or `false`. `false` starts no saga thread at all. | `true` |
| `names` | `"real"` or `"generated"` | `"real"` |
| `season_minutes` | whole minutes of quiet that end a world, 1 to 1440 | `20` |

```toml
[saga]
enabled = true
names = "generated"
season_minutes = 30
```

The story runs from start-up whether or not the view is on screen, so it has no gaps. Minutes become
sweeps with `--interval` on a TUI that hosts its own bus. A TUI attached to a headless host assumes
10 sweeps a second. `tt-toplike --check-config` prints the saga's settings and every problem in the
section.

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

* No person has run Saga on real Tenstorrent hardware yet. It has run on the mock backend and on
  recorded fixtures.
* Generated-name aliasing was checked on the busy fixture only.
* A TUI attached to a headless host counts 10 sweeps a second for the season, whatever the host's
  interval.
* Monsters, treasure, fights, the hero's death and descendants, and the `saga.*` bus events come in
  phase 2.
* The hero cannot be steered yet (phase 3).
* Saga is not a stop in `--rotate`, and the shared backdrop is not drawn behind it.
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

## Architecture

* `crates/tt-toplike-saga`: the pure core. `Saga::feed` takes records in order. No clock, no I/O.
* Its `clippy.toml` bans wall clocks, environment reads and hash-ordered collections.
* `src/saga/driver.rs`: a background thread that feeds the core from the bus, local or attached.
* The driver publishes a snapshot of the state at most 10 times a second.
* `src/saga/config.rs`: the `[saga]` section as the core's config. `src/saga/digest.rs`: `--saga-digest`.
* `src/ui/tui/saga_view.rs`: the view draws the snapshot and never feeds the core.

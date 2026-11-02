# Saga: a generative quest drawn from the machine's own signals

Working title. Date: 2026-11-02. Status: draft for review. Author: Claude, with Taylor Singletary.

## 1. Intent

tt-toplike shows what a Tenstorrent machine is doing. Saga is a view where that activity becomes a
story. A hero quests through a dungeon that the machine's signals generate. The dungeon is built,
destroyed and built again, always with a new maze. Monsters, NPCs, treasure and a running chronicle
all come from signals the bus already carries: model bring-up, requests, kernels, temperatures,
errors, resets, training steps, processes, leases.

The user's words: "tt-toplike remains primarily a TUI based app for watching and witnessing what your
machine is already doing. Taking a moment to have some fun and move the hero around is definitely on
the table but the hero should naturally also quest on its own. where and how it quests is what we
want to be totally generative here. worlds are created and destroyed and created again. mazes always
coming out new. monsters. NPCs. treasure. storytelling! but all kind of derived from tt-toplike's
signal set. tensixinet playing beside it would vibe because they're all derived from the same
signals."

The Ming Mecca user's guide (a Eurorack module that builds video-game worlds from control voltages)
is a loose inspiration: signals are voltages, collisions are outputs that go back into the patch,
terrain can be changed by signals and remembers the change, and worlds come as shareable text packs.
It is not a spec to follow.

Why now: the signal bus gives every view one ordered, replayable stream with provenance. A story
generator is a good test of that design. It needs entities, signals, events and replay, and it makes
the whole stream easy to enjoy at a glance.

## 2. Goals, non-goals, anti-goals

Goals
- **Watchable on its own.** Left alone, the hero quests, fights, collects and the chronicle scrolls.
  A person glancing at it sees something happening and can tell roughly why.
- **Generative.** No two worlds match. Mazes, casts and quests differ each time.
- **Derived from signals.** Every world, monster, NPC, treasure and story beat has a cause in the
  signal set, and the beat says which one.
- **Deterministic from a log.** The same recorded bus log gives the same saga on any machine. Replay
  at any speed gives the same story. That lets tensixinet and Saga agree.
- **Alive on a quiet box.** An idle machine gets a calm kingdom with weather, journeys and rest, and
  its worlds still end and begin.
- **Fun to touch.** A person can take over the hero with the keyboard for a while, and the hero goes
  back to questing when they stop.
- **A consumer and a producer on the bus.** Saga reads the bus and publishes its own events back
  (`saga.*`) so tensixinet, tt-station and other views can follow the story.

Non-goals
- Not a full game: no inventory screens, save slots, levelling menus or networked play.
- Not a replacement for any monitoring view. The numbers stay where they are.
- No hardware access of its own. Saga reads the bus and nothing else.

Anti-goals (things that make it a failure even if it runs)
- A story that misstates the machine. Fiction may dress a real event. It must not invent a fault.
  A throttle monster exists because the chip throttled.
- Burning CPU or terminal bandwidth. Budgets are in section 9.
- Leaking private data into the story. Command lines never appear. Section 10 covers names.
- Two consumers of one log telling different stories.

## 3. How it works

```
bus records ──► intake/query (shared) ──► Saga core (pure, deterministic) ──► saga events
                                                │                                 │
                                                ▼                                 ▼
                                      world state + chronicle           published to the bus
                                                │                         (tensixinet, tt-station)
                                                ▼
                                  TUI view (draw, steer, pause)
```

- **Saga core** is a library crate with no I/O, no clock and no terminal. It takes bus records in
  order and returns saga events and a world state. Time is bus time (`t_mono_ns` of the records), never
  the local clock. Its random numbers come from a generator seeded from record contents and consumed in a
  fixed order, so a replay repeats exactly.
- **The view** draws the world state, takes keys, and publishes the saga events. Drawing and key
  handling do not feed back into the core while the hero is on autopilot.
- **Shared intake and query** (phase 0) is the module that replaces the three copies of the
  "history makes no pulse, away makes none" rule (arcade, Die 3D and `bus_intake`) and adds a small
  query call ("entities of kind K that have signals A and B"). Saga is its first heavy user.

### 3.1 Where randomness comes from

The machine. At the moment a world is born, the core hashes a window of recent record contents (the
low bits of temperatures, power, counter deltas, request counts, the sequence number) into the world's
seed. A quiet box still wobbles, so seeds differ. No part of the seed depends on the host clock or on
the process's random state. Tests can inject a fixed window.

### 3.2 World lifecycle

Birth, life, death, rebirth. A world ends and the next begins when one of these happens in the
signals:

| Signal event | What happens in the story |
|---|---|
| A model reaches `Ready` | A gate opens. A new dungeon is born, sized by the model's chips and shaped by its name. |
| A model goes `Down`, or a chip resets | A collapse. The world ends and the chronicle records how. |
| A training run starts | A forge biome is born. A run ending closes it. |
| The machine has been quiet for a season (default 20 minutes of bus time) | The world fades into a dream. The next world begins from the dream's last moments. |
| The hero dies (section 6) | The world ends. A descendant starts the next one. |

At least one world always exists. The cycle never stalls: the quiet-season rule alone guarantees a
new world even if nothing else ever happens.

### 3.3 Building the maze

The footprint comes from the hardware's real shape, the seed decides the rest.
- The grid follows the board: the Tensix array is the chamber floor, harvested rows and columns
  (`toplike.harvesting_state`, `enabled_tensix_col`) are collapsed walls and ruins, the GDDR channels
  are vaults on the west and east sides (the documented layout: channels 0 to 3 west, 4 to 7 east),
  PCIe is the gate to the surface and the Ethernet ports are portals that lead to other chips.
- The generator carves rooms and corridors from the seed (a room-and-corridor algorithm, for example
  binary space partition plus a spanning tree; the builder chooses). Corridor density follows NoC and
  PCIe traffic, light follows power, weather follows temperature.
- Every room is reachable from the entrance. A test checks it for many seeds.

### 3.4 The cast

First-pass mapping. The builder may rename things freely, and every beat must still cite its signal.

| Fiction | Cause in the signal set |
|---|---|
| Hero (`@`) | One per world. Vitality comes from measured health: heartbeat, faults, thermal headroom, throttle. The arcade hero's `!`, `?` and `%` faces carry over. |
| Frost wyrm | A thermal throttle (`throttler` register, clock below base). Its size follows the temperature excess. |
| Bit-rot swarm | Corrected ECC counters rising. Uncorrectable ECC makes a Corruptor. |
| Dragon | A thermal trip (`therm_trip_count`). |
| Troll under a severed bridge | An Ethernet link going down (`eth.live_mask` loses a bit). |
| Sage or oracle | A ready model. Named after the model. Requests are pilgrims who bring it questions. |
| Librarian, blacksmith, warden | Service and process roles: a model manager, a training run (the forge heats as loss falls), a lease holder (gozer's warden grants keys). |
| Rumours and whispers | Hivemind sweeper events. |
| Coins | Completed requests, scaled by count. |
| Scrolls | Compiled kernels (`KernelCompiled`). |
| Relics and chests | A model ready, a checkpoint saved, a token milestone. |
| Weather, tides | Power, clock and PCIe traffic. |

A monster lives while its cause lives. When the signal settles the monster is slain, and the
chronicle credits the hero if the hero was there. Combat is resolved by simple deterministic rules from
the two measured magnitudes involved. The rules are data, in the pack (section 7), with defaults built
in.

## 4. The hero

- **Autopilot.** The hero picks a quest from the world's hooks and walks it, using pathfinding over
  the maze. A quest is a verb, a target entity and a reason signal: slay the throttle, escort the
  request stream, rebuild the bridge when a model loads, cleanse the corrupted vault. With no hooks the
  hero explores, rests and visits the sage. A stuck detector replans if the hero stops making progress.
- **Steering.** Arrow keys or `hjkl` move the hero one step. While a person steers, the hero is under
  hand control. After a few seconds of no keys, or on `Space`, autopilot resumes. A run that was ever
  steered is marked in the chronicle ("a hand guided the hero") and is not replay-stable from that
  point. Autopilot-only runs stay exactly replayable.
- **Death and descendants.** The hero dies when the story's danger exceeds its vitality, at a chip reset
  or a collapse, or of old age at the end of a season. A descendant is born in the next world. The
  chronicle keeps a lineage: who, which world, cause of death. The lineage is the saga's memory.

## 5. The chronicle

A scrolling log beside the map. Each line is a beat: a template filled from a deterministic grammar
(word lists and sentence shapes in the pack), conditioned on a tone chosen from the machine's state
(calm, tense, triumphant). Each beat carries a hidden cause (entity, signal, record sequence number),
shown on demand (a key toggles "why"). Fiction is decoration, and the cause shows that the event was
measured. Phase 4 may add an optional narrator that asks a local model server to rewrite beats in a
richer voice. The grammar stays the default and the fallback.

## 6. Events the saga publishes

Source name `saga`, provenance `Derived`, a new payload family `saga.*` carried as ordinary bus events:
`saga.world_born`, `saga.world_ended`, `saga.hero_born`, `saga.hero_died`, `saga.quest_started`,
`saga.quest_done`, `saga.monster_born`, `saga.monster_slain`, `saga.treasure_found`, `saga.beat`.
Each carries a world id, an intensity from 0 to 1, a position, and its cause (entity key, signal name,
record sequence number). tensixinet and tt-station can subscribe through the existing socket or
compute the same stream with the crate. The events are written to the log like any other, so a replay of
a recording carries the saga with it.

## 7. Packs

Saga's rules and flavour live in data: names, glyphs, tile sets, word lists, grammar shapes, the
signal-to-fiction table and combat rules. The built-in pack is compiled in. A user pack is a plain
text file in a human-editable format (loosely like the Ming Mecca world packs: readable, ASCII art for
tiles and sprites, missing parts fall back to the defaults). Phase 4 builds the loader. Before then
the same data is a Rust table so the format can follow what the code needs.

Phase 4 may also add a **glitch mode** after the manual's idea: terrain drawn from real raw data
(tt-smi words, NoC counter arrays, log records), shown only when switched on and labelled raw.

## 8. Where it appears

- A new view, `DisplayMode::Saga`, entered by its own key (the builder picks one that is free in every
  view and mode), in `--rotate` as an optional stop, and as a panel kind `saga` in arcade layouts. It
  works with the shared backdrop (transparent layering).
- Config in the shared `config.toml`: `[saga]` with `names` (`real` or `generated`), `season_minutes`,
  `pack` (phase 4), and whether it joins the rotation.
- Help, legend and explain text follow the overlay row targets (Help 24, Legend 30, Explain 30 rows at
  80 columns).
- The view must be legible at 80x24 and degrade at smaller sizes (map only, then chronicle only).

## 9. Budgets

- Simulation steps at no more than 10 Hz of bus time and costs under 1 ms per step on the developer
  box.
- World grid at most 120 x 60 cells. At most 500 entities. Chronicle ring of 500 lines.
- Terminal traffic: redraw only changed cells. On a quiet world, under 20 KB/s at 140x40. Measure it
  with the pty method used for fast start.
- No allocation per frame beyond the view's own line building.

## 10. Safety and privacy

- Command lines never appear. A process appears only as an NPC with its process name. A model or
  service appears by its model name, which the `i` view already shows.
- `names = "generated"` replaces every real name with a generated one (for screen sharing).
- Saga reads only the bus. It opens no device and runs no command. It works attached to a headless
  host and on a replayed log.

## 11. Testing

- **Determinism:** a recorded fixture log gives an identical saga digest (world seeds, events, chronicle
  text) in a test, on every platform CI builds. Replay at two speeds gives the same digest.
- **Maze:** for many seeds every room is reachable, every treasure and exit is reachable, no entity
  spawns in a wall.
- **Lifecycle:** worlds never stall, a world always exists, the quiet-season rule fires.
- **Hero:** never stuck for more than a bounded number of steps; dies and is reborn on the listed signals.
- **Cause:** every published beat has a cause that exists in the log.
- **Robustness:** random record streams (fuzz) and terminal sizes from 0x0 to 500x200 never panic.
- **Provenance and privacy:** no beat contains a command line; `names = "generated"` leaves no real name.
- The usual rules: fixed clocks, no wall-clock assertions, every empty-world and pinned golden unchanged.

## 12. Phases

- **0. Shared intake and query.** One module for the history and away rules and a small query call. The
  arcade, Die 3D and `bus_intake` move onto it with their pinned frames unchanged.
- **1. Saga core and a watch-only view.** Seed, maze from topology, hero autopilot, a first chronicle,
  world lifecycle, the view, determinism tests. The first minute on a live box shows a world, a hero
  walking, and a few beats.
- **2. The cast and the fights.** Monsters, NPCs, treasure, combat rules, death and descendants, lineage,
  `saga.*` events on the bus.
- **3. Hands on.** Steering, quest hooks shown, the `saga` panel in arcade layouts, the rotation stop,
  config, copy.
- **4. Packs, glitch mode, narrator.** User packs, raw-data terrain, an optional local-model narrator,
  and the tensixinet event contract written down as a document.

## 13. Left to the builder

Names of every monster and place. The maze algorithm. Pathfinding details. Combat arithmetic. The
grammar's shape. Glyph choices. The entry key. How the chronicle and map share a small screen.

## 14. Open items and my guesses

- Working title "Saga". Alternatives: Silicon Saga, Rogue Tensix, Tensix Tales, Dungeon Die. The user
  likes a fun name (the Wafer Waffler), so this is theirs to pick.
- **Names in the story:** my guess is real model and process names by default, with `generated` as an
  opt-in, because the `i` view already shows them.
- **Where the core lives:** my guess is a separate crate (`tt-toplike-saga`) beside `tt-toplike-bus`,
  so tensixinet and tt-station can use it. The view lives in `tt-toplike`.
- **The local-model narrator** is a later, optional step. The grammar is the baseline.
- **The first thing to show:** my guess is a world born when a model reaches `Ready`, with the hero
  walking to the sage and a handful of beats.

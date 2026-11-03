# Saga Party and Portal Crossing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every chip's floor on screen at once, each with its own hero questing at the same time, one merged chronicle with each line tagged by its floor (Phase A, 0.17.0). Then heroes step through Ethernet portals into the floor of the chip at the other end of the link (Phase B, 0.18.0).

**Architecture:** The pure core (`crates/tt-toplike-saga`) replaces its one hero with a party: one `Member` per floor of the world, each with its own hooks and plan generator, sharing one chronicle whose routine lines share a budget. The digest gains a version item and per-floor fields. The root crate's view draws a grid of hero-centred tiles with a roster row and focus keys, and keeps today's drawing for one chip. Phase B adds an additive bus event `LinkPeer`, a link table in the core, portals bound to ports, and a `Cross` quest.

**Tech Stack:** Rust 2021 (MSRV 1.93), ratatui 0.30, `tt-toplike-bus` (0.1.0, 0.2.0 in Phase B), `tt-toplike-saga` (0.1.0 to 0.2.0 in Phase A, 0.3.0 in Phase B). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-03-saga-party-design.md` (the addendum, "the addendum" below), on top of `docs/superpowers/specs/2026-11-02-saga-design.md` ("the spec"). Ledger of phase 1: `.claude/worktrees/saga/.superpowers/sdd/2026-11-02-saga/progress.md` (rulings R1 to R28).

**Before Phase B:** the user answers the addendum's question 1 (where link peers come from). Task B.2 is written for the default answer, (c) now and (a) later. If the answer differs, rewrite B.2 before starting it. Phase A does not depend on any open question. Question 2 (one chip's reset) changes only Task A.3 if the user picks "collapse one floor": stop and re-plan A.3 in that case.

## Global Constraints

- **Determinism contract (spec 3.5):** the same ordered stream of record contents gives the same saga. The core reads no sequence number and no timestamp for any decision, iterates only `BTreeMap`/`BTreeSet`/`Vec` in a fixed order, and uses only `+ - * /`, comparisons, `sqrt` and integer operations. No view state (focus, page, terminal size) ever reaches the core.
- **Pure core:** the saga crate's fences stay as they are (`clippy.toml` bans `Instant::now`, `SystemTime::now`, `env::var`, `HashMap`, `HashSet`; `tests/fences.rs` checks the dependency list). `#![forbid(unsafe_code)]`.
- **One chip is today's saga.** The story print and the four view frames of `solo.jsonl` pinned in Task A.1 stay byte-identical through Phase A. Only the digest moves, once, in Task A.2, by the encoding change.
- **Golden re-pins are deliberate.** Every task says which digest goldens (`tests/determinism.rs`, `a_snapshot_of_the_digests_matches`) and which view goldens (`src/ui/tui/saga_view_tests.rs`) it moves. A task that moves a golden it did not name stops and reports. Each re-pin goes in the commit of the change that caused it, names the rule that changed, and adds a row to the re-pin table in `AGENTS.md` (Saga section).
- **Budgets (spec 9 and the addendum 3.7):** a record costs under a million work units with 32 chips, birth included (the test-only counter in `src/cost.rs`, R9); at most 32 members; at most 500 entities; chronicle ring of 500 lines; quiet-world terminal traffic under 20 KB/s at 140x40.
- **Caps stay:** `MAX_CHIPS` 64, `MAX_FLOORS` 32, `MAX_READY_MODELS` 64, `MAX_PENDING_EVENTS` 4096, 256 bytes per key or name (R20 to R22). No lazy floors: every floor of a world is simulated and has a member (the addendum 3.7, closing the R22 deferral).
- **Privacy (spec 10, R5, R17):** a command line never appears. With `names = "generated"` no real model, process or service name appears in any beat text, cause, event, `target_label`, tile HUD or roster row. Chip and Ethernet port keys stay visible in both modes.
- **Fixtures carry no real names (R6).** Every new fixture comes from a generator in `crates/tt-toplike-saga/tests/support`, with made-up names (`demo-*`) and made-up PCI addresses. Committed fixtures stay under 1 MB each.
- **Repository rules:** plain-English prose in docs, comments and every string a person reads (run the sweep for `, not `, `rather than`, `instead of`, `reads as`, `honest`, `the one`, `which is why`, `This is what` over every file the task adds or changes); no right-side border characters (the view's rows never end in a glyph from `RIGHT_EDGE_GLYPHS`); bump the version with each change; branch from `what-if`, default branch `main`; no hardware (every test and smoke run uses `--mock`, fixtures or generators); never `pgrep -f`/`pkill -f`; do not touch `~/.local/bin`.
- **Tests:** fixed clocks and fixed inputs only. Before a task is done, run the CI forms in `.github/workflows/ci.yml` (fmt, clippy for both feature sets and for `-p tt-toplike-saga` and `-p tt-toplike-bus`, the saga and bus crate tests) and the lib tests the way CI does: `env -u COLORTERM -u TMUX TERM=dumb cargo test --locked --lib --features tui`. A test that has never been seen to fail is a claim: put each guarded bug back once, see red, restore. Every pinned golden outside Saga (Die 3D, `i`, arcade, backdrop, every empty-world one) stays byte-identical.

## Review Focus

The places most likely to bite a person using this, most likely first. Each line names the test that guards it and the task that owns it.

1. **One chip no longer looks or reads like 0.16.2.** `solo_story_print_is_unchanged` and `solo_frames_are_the_pre_party_frames` (A.1 pins them; every later Phase A task runs them).
2. **The story depends on the terminal or the focus.** `the_core_never_reads_view_state` (A.7: drives one saga, draws it at 80x23 and 500x200 with every focus, and compares digests) and `four_and_thirty_two_chips_give_the_same_digest_after_renumbering` (A.3).
3. **One member's draws change another member's walk.** `a_member_that_replans_leaves_the_others_unchanged` (A.3).
4. **A real name leaks through a dropped line, a target label or a tile HUD.** `generated_mode_leaves_no_fixture_name_anywhere_in_a_party` (A.4, core) and `no_tile_hud_or_roster_shows_a_fixture_name` (A.7, view).
5. **The merged chronicle floods, or one floor is never heard.** `routine_lines_come_from_one_floor_per_window` and `every_floor_is_heard_within_a_minute` (A.4).
6. **Cost at 32 chips:** `a_record_costs_under_a_million_units_with_32_chips`, `more_than_32_chips_give_32_members` (A.5), and `the_party_view_fits_every_size_for_1_2_4_and_32_chips` (A.7).
7. **Two heroes share a name.** `a_party_of_32_has_32_names` and `a_two_name_lexicon_forces_the_numeral` (A.3).
8. **A crossing on real hardware invents a link.** `a_portal_without_a_peer_record_never_opens` (B.3).
9. **A link that drops mid-crossing strands a hero.** `a_link_down_in_transit_throws_the_hero_back` (B.4).
10. **Portals point at the wrong port.** `portal_order_matches_die3d_cage_order` (B.3).

---

Left to the builder: template wording for `PartyBorn` and the crossing beats, the exact floor hues within the brand palette, tile gap tie-breaks beyond the rule in `party_grid`, the roster glyphs, and the zoom key if `z` is taken (the key-free test names it).

## Phase A: the party (0.17.0)

### Task A.1: Party fixtures, and pins taken on today's code

Do this before any core change. It is the instrument for "one chip is today's saga".

**Files:**
- Create: `crates/tt-toplike-saga/tests/support/party.rs` (register in `tests/support/mod.rs`)
- Create: `crates/tt-toplike-saga/tests/fixtures/solo.jsonl`, `crates/tt-toplike-saga/tests/fixtures/party4.jsonl` (written by the generator)
- Create: `crates/tt-toplike-saga/tests/party.rs` (the party integration tests; later tasks add to it)
- Modify: `crates/tt-toplike-saga/tests/determinism.rs` (`FIXTURES` gains `solo.jsonl` and `party4.jsonl`; the goldens gain their rows)
- Modify: `src/ui/tui/saga_view_tests.rs` (solo golden frames)

**Interfaces:**
- Produces: `pub fn records(chips: u8, ticks: u64) -> Vec<Record>` and `pub fn file_text(chips: u8, ticks: u64) -> String` in `support::party`; `pub fn chip(i: u8) -> EntityKey` (`chip:0000:{i+1:02x}:00.0`); constants `SOLO_TICKS = 1200`, `PARTY4_TICKS = 600`.
- Produces: `pub fn story_print(saga: &Saga) -> u64` in `tests/support/mod.rs` (FNV-1a over each chronicle line's tone code and text, oldest first) and a per-tick position trace collected by `pub fn story_trace(records: &[Record]) -> u64` (feeds every record, and after each tick folds the lead hero's floor, x and y into the hash). It reads only public state. Task A.3 changes a single accessor line (`state.hero` to `state.party[0].hero`) and nothing else.
- What the generator plays, for `n` chips (document it in a table at the top of `party.rs`, as `busy.rs` does): chip 0 is the pacemaker and sweeps first; one sweep per chip per tick (power, temperature, AI clock, low-bit wobble from its own LCG); topology at tick 1 (chips alternate between two Tensix masks, every chip has an Ethernet mask with a different popcount); heartbeats every 5 ticks, chip `n-1` stalls for 150 ticks; `demo-sage` loads at tick 30 and is ready at 80 on chips `0..=n/2`; `demo-scribe` ready at 300 on chip `n-1`; requests every 10 ticks; one throttle on chip 1 (when `n > 1`); a `LinkChanged` on chip 0 at tick `ticks - 200`; quiet after that. Record time 100 ms per tick. No reset (the reset rule is question 2; busy.jsonl keeps covering it).

- [ ] **Step 1: Write the failing tests** in `tests/party.rs`: `the_party_generator_is_deterministic` (two calls give equal records for 1, 2, 4 and 32 chips), `the_committed_party_fixtures_match_the_generator` (file text equals `file_text`; message names the ignored writer), `the_party_fixtures_carry_no_real_names` (every model key starts `model:demo-`, every chip key matches the made-up pattern, and no record is a process sample), `the_committed_fixtures_are_under_a_megabyte`, and the ignored writers `write_solo_fixture` and `write_party4_fixture`.
- [ ] **Step 2: Run them** (`cargo test -p tt-toplike-saga --test party`) and see them fail (module absent).
- [ ] **Step 3: Implement** the generator, run the ignored writers, commit the two files.
- [ ] **Step 4: Pin on today's code.** Add to `tests/party.rs`: `solo_story_print_is_unchanged` (`story_print` after the whole solo log and `story_trace`, both modes, pinned constants) and to `determinism.rs` the `solo.jsonl` and `party4.jsonl` rows of the golden table (today's encoding). Add to `saga_view_tests.rs`: `solo_frames_are_the_pre_party_frames` (80x23 and 140x39, why off and on, after the whole solo log, with a `SagaMemory` observed after every record, as `golden_frames` does). Run, copy the printed values, pin. Mutate once: change one wobble constant in the generator, see the fixture test and the pins go red, restore.
- [ ] **Step 5: Commit** `test(saga): party fixtures, with the single-chip story and frames pinned on 0.16.2`.

Goldens moved: none. Goldens added: `solo.jsonl` and `party4.jsonl` digests (today's encoding), the solo story print and trace, four solo view frames.

### Task A.2: Digest version 2 and a floor on every beat

The encoding change alone, with one hero still, so its golden moves are separate from the party's.

**Files:**
- Modify: `crates/tt-toplike-saga/src/chronicle.rs` (`BeatCtx`, `Beat`, `Beat::event`)
- Modify: `crates/tt-toplike-saga/src/event.rs` (`QuestStarted`, `QuestDone`, `Beat` gain floors)
- Modify: `crates/tt-toplike-saga/src/saga.rs` (encoding, module doc table, `Saga::new`, `digest`)
- Test: `crates/tt-toplike-saga/src/saga_tests.rs`, `crates/tt-toplike-saga/tests/determinism.rs`

**Interfaces:**
- `pub struct BeatCtx { ..., pub floor: Option<u8> }`; `pub struct Beat { ..., pub floor: Option<u8> }`.
- `SagaEvent::QuestStarted { world: u64, floor: u8, verb, target, reason }`, `SagaEvent::QuestDone { world: u64, floor: u8, verb }`, `SagaEvent::Beat { world: u64, floor: Option<u8>, tone, text, cause }`.
- `pub const DIGEST_VERSION: u8 = 2;` and `const TAG_VERSION: u8 = 0x00;` in `saga.rs`. `Saga::new` starts `trace` from `fnv1a64(&[TAG_VERSION, DIGEST_VERSION])`. Encodings as in the addendum's table 3.6: `put_opt_u8` for an `Option<u8>` (presence byte, then the byte). `TAG_END` writes the member count (1 here) then the hero's floor, x, y.
- World beats get `floor: None`; the hero's and quests' beats get `Some(hero.floor)`.

- [ ] **Step 1: Write the failing tests:** `an_empty_saga_digest_has_the_version_item` (the digest of `Saga::new` equals a hand-computed FNV-1a over `[0x00, 0x02]` then the end item), `quest_and_beat_events_carry_the_floor`, `world_beats_have_no_floor`, `the_encoding_table_matches_the_code` (encode one of each event by hand from the doc table and compare bytes).
- [ ] **Step 2: Run** and see red.
- [ ] **Step 3: Implement.** Update the module comment's table and field list.
- [ ] **Step 4: Run.** `solo_story_print_is_unchanged` and `solo_frames_are_the_pre_party_frames` pass unchanged (the view draws no floor yet). The 8 busy view frames pass unchanged. Re-pin the digest goldens: all five rows move (`quiet`, `bringup`, `busy`, `solo`, `party4`, both modes), because of the version item and the floor fields. Mutate once: drop the floor byte from `QuestDone`, see `the_encoding_table_matches_the_code` red, restore.
- [ ] **Step 5: Commit** `feat(saga): digest version 2, with a floor on every beat and quest event`.

Goldens moved: every digest row, both modes (the encoding). View goldens: none. Story print: unchanged.

### Task A.3: The party in the core

**Files:**
- Modify: `crates/tt-toplike-saga/src/saga.rs` (state, members, hooks, birth, tick, tone)
- Modify: `crates/tt-toplike-saga/src/world.rs` (a `ModelReady` dungeon spans every kept chip and remembers the model's chips)
- Modify: `crates/tt-toplike-saga/src/pack.rs` (`hero_name_unique`)
- Modify: `crates/tt-toplike-saga/src/lib.rs` (export `Member`)
- Modify: `src/ui/tui/saga_view.rs`, `src/saga/driver.rs`, `src/saga/digest.rs` (compile only: read `state.party[0]` where they read `state.hero` or `state.quest`)
- Test: `crates/tt-toplike-saga/src/saga_tests.rs`, `crates/tt-toplike-saga/src/world_tests.rs`, `crates/tt-toplike-saga/src/pack_tests.rs`, `crates/tt-toplike-saga/tests/party.rs`

**Interfaces:**
- `pub struct Member { pub hero: Hero, pub quest: Quest, pub unwritten: u32 }` (`unwritten` stays 0 until A.4).
- `pub struct SagaState { pub label: Label, pub world: World, pub party: Vec<Member>, pub chronicle: Chronicle, pub tick: u64 }` with `SagaState::lead(&self) -> &Member` (index 0) and `SagaState::on_floor(&self, floor: u8) -> impl Iterator<Item = (usize, &Member)>`. The placeholder state has one member.
- Internal per member: `hooks: Vec<Hook>`, `plan_rng: Pcg32`, `dwell_until: Option<u64>`, held in a private `Vec<MemberCore>` index-aligned with `state.party`.
- `pub const PARTY_STREAM_BASE: u64 = 16;` member `i` plan stream: `PLAN_STREAM` for `i == 0`, else `PARTY_STREAM_BASE + i`.
- `Pack::hero_name_unique(&self, rng: &mut Pcg32, taken: &[String]) -> String`: up to `HERO_NAME_TRIES = 8` draws, then a Roman numeral suffix (`II` to `XXXII`) one more than the count of `taken` equal to the last draw.
- `World` gains `pub sage_chips: Vec<EntityKey>` (the model's chips, empty for non-model worlds). `Lifecycle`'s `ModelReady` births on `all_chips(topo)` and sets `sage_chips` to the event's chips (all chips when it names none). Floors: `chips.truncate(MAX_FLOORS)` as now.
- A member's sage hooks: one per ready model whose chips include the member's floor chip (or that names no chips). `ModelGone` removes the model's hooks from every member; members walking to it replan.
- Tone: a member's beats use its vitality; world beats use `party.iter().map(vitality).min`.
- Birth order of events: `WorldBorn` and its beat; for each member in index order, `HeroBorn` and (for a one-member party only) its beat; then for each member, `QuestStarted` and its beat. (A.4 adds `PartyBorn` and the budget.)

- [ ] **Step 1: Write the failing tests:** `a_world_on_four_chips_has_four_members_each_on_its_floor`, `a_party_of_32_has_32_names`, `a_two_name_lexicon_forces_the_numeral` (pack test with a stub lexicon), `a_member_that_replans_leaves_the_others_unchanged` (two sagas over the 4-chip log, one with an extra unreachable hook injected on member 2 through a test-only constructor; members 0, 1 and 3 walk identically), `a_model_dungeon_spans_every_chip_with_the_sage_on_its_own` (world test), `a_sage_hook_is_dropped_only_for_the_member_who_visited`, `model_gone_replans_every_member_walking_to_it`, `four_and_thirty_two_chips_give_the_same_digest_after_renumbering` (the `renumber` helper with a stride of 3, the ledger's deferred minor).
- [ ] **Step 2: Run** and see red.
- [ ] **Step 3: Implement.** Update the module comments of `saga.rs` ("Worlds, heroes and quests", "Tone", the bounds table: members at most `MAX_FLOORS`) and `world.rs` (the `ModelReady` rule).
- [ ] **Step 4: Run.** `solo_story_print_is_unchanged` passes with its accessor moved to `state.party[0]` and nothing else changed. `solo_frames_are_the_pre_party_frames` passes. Re-pin the digest goldens for `quiet.jsonl`, `busy.jsonl` and `party4.jsonl` (both modes): more members, and `demo-scribe`'s dungeon now spans both busy chips. `bringup.jsonl` and `solo.jsonl` do not move. The 8 busy view frames move (the lead hero's world changed at `demo-scribe`); re-pin them and say why. Mutate once: give every member `PLAN_STREAM`, see `a_member_that_replans_leaves_the_others_unchanged` and `a_party_of_32_has_32_names` go red, restore.
- [ ] **Step 5: Commit** `feat(saga): a party, one hero per floor, each with its own hooks and generator`.

Goldens moved: digests of `quiet`, `busy`, `party4` (both modes); the 8 busy view frames. Unchanged: `bringup`, `solo`, the solo story print and frames.

### Task A.4: The merged chronicle: pace, the party's birth, target labels

**Files:**
- Modify: `crates/tt-toplike-saga/src/chronicle.rs` (`BeatKind::PartyBorn`, `{count}`, `clean_cause`, `label_for`)
- Modify: `crates/tt-toplike-saga/src/pack.rs` (`PartyBorn` templates in all three tones)
- Modify: `crates/tt-toplike-saga/src/quest.rs` (`Quest::target_label`)
- Modify: `crates/tt-toplike-saga/src/saga.rs` (the routine budget, `PartyBorn`, labels, `unwritten`)
- Modify: `src/ui/tui/saga_view.rs` (`quest_target` reads `quest.target_label`; delete the beat lookup)
- Test: `crates/tt-toplike-saga/src/chronicle_tests.rs`, `crates/tt-toplike-saga/src/saga_tests.rs`, `crates/tt-toplike-saga/tests/party.rs`, `src/ui/tui/saga_view_tests.rs`

**Interfaces:**
- `BeatKind::PartyBorn` (appended to `BeatKind::ALL`); `BeatCtx` gains `pub count: u8`; the `{count}` placeholder writes the number in words up to twelve ("four"), digits above.
- `Chronicle::clean_cause(&mut self, cause: &Cause, names: NameMode, rng: &mut Pcg32) -> Cause` (the aliasing `push_beat` applies to a cause, with no line written).
- `Chronicle::label_for(&mut self, name: &RealName, role: NameRole, names: NameMode, rng: &mut Pcg32) -> String` (the real name, or its alias, drawing one only if the table lacks it).
- `pub struct Quest { ..., pub target_label: Option<String> }`; `plan` leaves it `None`; the front door fills it after the quest's beat is written, or, for a dropped beat, through `label_for`.
- `pub const ROUTINE_WINDOW_TICKS: u64 = 10;` Window `k` is ticks `[10k, 10k + 10)`. Routine kinds: `QuestStarted`, `QuestDone`, `Rest` and `Ambient`. A routine beat from floor `f` is written when the window has no owner (then `f` owns it) or `f` is the owner; else it is not written, its event's cause goes through `clean_cause`, and the member's `unwritten` counts up. Notable beats are always written.
- A party of two or more writes one `PartyBorn` beat (floor `None`, `count` = members) after `WorldBorn`'s beat and writes no `HeroBorn` beats.

- [ ] **Step 1: Write the failing tests:** `routine_lines_come_from_one_floor_per_window` (32-chip log: for each window, the written routine beats name one floor), `every_floor_is_heard_within_a_minute` (32-chip log: every floor writes a routine line within every 600 ticks after the first world's birth), `a_one_member_party_never_drops_a_line` (solo log: `unwritten` stays 0), `a_party_birth_writes_one_party_line`, `a_dropped_quest_still_raises_its_event_with_a_clean_cause` (generated mode), `target_label_is_the_beat_name_in_both_modes`, `generated_mode_leaves_no_fixture_name_anywhere_in_a_party` (4-chip and 32-chip logs: chronicle text, causes, every drained event, every member's `target_label`, contain no `demo-sage`/`demo-scribe`), `every_lexicon_tag_a_template_uses_exists` still passes with `PartyBorn`. In the view tests: `the_status_row_names_the_target_from_the_core`.
- [ ] **Step 2: Run** and see red.
- [ ] **Step 3: Implement.** Update the chronicle and saga module comments (placeholders table, "Names", the new "Pace" section).
- [ ] **Step 4: Run.** Solo story print and solo frames unchanged (one member owns every window; the view's target text equals the old lookup's on the solo log). Re-pin digests of `quiet`, `busy`, `party4` (both modes): dropped lines, `PartyBorn`. Re-pin the 8 busy view frames (chronicle lines changed). Mutate once: let every floor write in every window, see `routine_lines_come_from_one_floor_per_window` red; remove the `clean_cause` call, see the privacy test red; restore both.
- [ ] **Step 5: Commit** `feat(saga): a merged chronicle with a shared pace, a party line, and target labels in the core`.

Goldens moved: digests of `quiet`, `busy`, `party4` (both modes); the 8 busy view frames. Unchanged: `bringup`, `solo`, the solo story print and frames.

### Task A.5: Caps and cost at 32 chips and beyond

**Files:**
- Modify: `crates/tt-toplike-saga/src/world.rs` (`World::floors: Arc<[Floor]>`)
- Modify: `crates/tt-toplike-saga/src/saga.rs` (bounds table, cost section)
- Modify: `src/ui/tui/saga_view.rs`, `src/saga/driver.rs` (follow the `Arc`)
- Test: `crates/tt-toplike-saga/tests/fuzz.rs`, `crates/tt-toplike-saga/src/fuzz_tests.rs`, `src/saga/driver_tests.rs`

**Interfaces:**
- `pub floors: Arc<[Floor]>` in `World`; `World` keeps `Clone` and `PartialEq`.
- No new constants. The tests use the A.1 generator for 32 chips and a variant `support::party::records_with_extra_chips(n: u8, ticks: u64)` for 33, 64 and 128 chips (keys after chip 31 sort after the first 32).

- [ ] **Step 1: Write the failing tests:** `a_record_costs_under_a_million_units_with_32_chips` (every record of the 32-chip log, birth included, with the work counter), `more_than_32_chips_give_32_members` (33, 64, 128), `members_never_exceed_max_floors_under_fuzz` (extend `random_record_streams_never_panic` to assert `party.len() <= MAX_FLOORS` and `== world.floors.len()`), `a_snapshot_shares_its_floors` (driver test: two published snapshots' `world.floors` are `Arc::ptr_eq` within one world).
- [ ] **Step 2: Run** and see the snapshot test red (floors are cloned today) and the others green or red as they stand. A cost test that is green at once is mutated in step 4.
- [ ] **Step 3: Implement** the `Arc` and any bound the tests find missing.
- [ ] **Step 4: Run.** Measure and record in the task report: work units per record at 32 chips (birth and steady), and the driver's snapshot time for 32 chips on the developer box (a note, never an assertion). Mutate once: raise `MAX_FLOORS` to 64 in a scratch build and see `more_than_32_chips_give_32_members` red; restore.
- [ ] **Step 5: Commit** `perf(saga): shared floors in snapshots, and bounds checked at 32 chips and more`.

Goldens moved: none (the `Arc` changes no byte of any digest or frame).

### Task A.6: Floor memories and heat for every floor

**Files:**
- Modify: `src/ui/tui/saga_view.rs` (`PartyMemory`; `SagaMemory` lights around every member on its floor)
- Modify: `src/ui/tui/mod.rs` (the loop keeps a `PartyMemory` and sets each floor's heat)
- Test: `src/ui/tui/saga_view_tests.rs`

**Interfaces:**
- `pub struct PartyMemory { floors: Vec<SagaMemory> }`; `PartyMemory::new()`, `PartyMemory::observe(&mut self, state: &SagaState)` (one `SagaMemory` per floor of the world, each keyed by world id, seed and floor index; starts over on a new world), `PartyMemory::floor(&self, i: usize) -> Option<&SagaMemory>`, `PartyMemory::set_heat(&mut self, floor: usize, celsius: Option<f32>)`.
- `SagaMemory::observe_floor(&mut self, state: &SagaState, floor: usize)` lights around every member standing on `floor` and keeps one trail per member. `SagaMemory::observe(&mut self, state)` stays as `observe_floor(state, lead floor)` for the single-chip path.
- `pub fn floor_chip(state: &SagaState, floor: usize) -> Option<&EntityKey>` replaces `hero_chip` (which becomes `floor_chip(state, lead floor)`).

- [ ] **Step 1: Write the failing tests:** `each_floor_remembers_its_own_cells`, `a_new_world_clears_every_floor_memory`, `heat_is_per_floor`, `a_party_memory_of_one_equals_a_saga_memory` (over the solo log, `PartyMemory::floor(0)` and a plain `SagaMemory` draw identical frames).
- [ ] **Step 2: Run** and see red.
- [ ] **Step 3: Implement.** The TUI loop reads each floor chip's temperature by bus address, as it does for the hero's chip today.
- [ ] **Step 4: Run.** Solo frames and the 8 busy frames unchanged (the view still draws the lead floor). Mutate once: key the memory by world id only, see `a_new_world_clears_every_floor_memory` red, restore.
- [ ] **Step 5: Commit** `feat(saga): a lit-cell memory and a heat reading for every floor`.

Goldens moved: none.

### Task A.7: The party view

**Files:**
- Create: `src/ui/tui/saga_party.rs` (the grid, tiles, roster; `saga_view.rs` is already 1,829 lines) and `src/ui/tui/saga_party_tests.rs`
- Modify: `src/ui/tui/saga_view.rs` (`render_saga` dispatches: one floor draws through today's code; two or more through `saga_party`; chronicle rows print floor tags; `map_rows` takes a floor index and a window centre; `HELP_KEYS`, `EXPLAIN_TEXT`, `legend_lines`)
- Modify: `src/ui/tui/mod.rs` (`[`, `]`, `z` routed only in Saga mode; `SagaFlags` gains focus and zoom)
- Test: `src/ui/tui/saga_view_tests.rs`, `src/ui/tui/saga_party_tests.rs`

**Interfaces:**
- `pub struct Grid { pub cols: usize, pub rows: usize, pub tile_w: usize, pub tile_h: usize, pub per_page: usize }`; `pub fn party_grid(n: usize, w: usize, h: usize) -> Option<Grid>` (`None` when one tile of `TILE_MIN = (24, 7)` does not fit). Rule from the addendum 5.2.
- `pub struct SagaFlags { pub stopped: bool, pub why: bool, pub focus: usize, pub zoom: bool }`; `pub fn render_saga(state: &SagaState, memory: &PartyMemory, flags: SagaFlags, area: (u16, u16), frame: u64) -> Vec<Line<'static>>`.
- `pub fn floor_hue(palette: &Palette, floor: u8) -> Rgb` (the eight-hue cycle, addendum 5.4).
- `fn tile_rows(state, memory, look, floor, w, h, focused, frame) -> Vec<Row>`; `fn roster_row(state, flags, page, w) -> Row`.
- Keys: `pub const PREV_FLOOR_KEY: char = '['; pub const NEXT_FLOOR_KEY: char = ']'; pub const ZOOM_KEY: char = 'z';` and `saga_key_for` accepts them in Saga mode only.

- [ ] **Step 1: Write the failing tests:** `party_grid_gives_the_addendum_table` (pins the cases of the addendum 5.2 for 1, 2, 4, 32 chips at 80x23, 140x39, 200x59), `below_the_minimum_tile_the_focused_floor_draws_alone`, `the_party_view_fits_every_size_for_1_2_4_and_32_chips` (0x0 to 500x200, no line wider than the area, no more lines than rows), `no_party_row_ends_in_a_right_side_border` (80x23, 140x39, 200x59, every cell seen), `the_roster_shows_every_member_and_the_chips_without_a_floor` (32 and 40 chips), `focus_moves_the_page`, `zoom_draws_the_focused_floor_like_today`, `one_floor_ignores_focus_and_zoom` (solo frames equal for every flag value), `member_lines_carry_their_floor_tag_and_world_lines_none`, `no_tile_hud_or_roster_shows_a_fixture_name` (generated mode, 4 and 32 chips, every size in the table), `the_core_never_reads_view_state` (feed one saga over the 4-chip log while drawing every 50 records at alternating sizes and focus values; its digest equals a saga fed with no drawing), `the_party_keys_are_free_in_saga_mode` and `the_party_keys_are_not_taken_outside_saga`, the overlay row-target test extended with the new help and legend rows, and golden frames `party_frames_are_pinned` for `party4.jsonl` and the 32-chip log at 80x23 and 140x39, why off and on.
- [ ] **Step 2: Run** and see red.
- [ ] **Step 3: Implement.** Reuse `map_rows`, `Look`, `Row`, the fog and the trail. Update the module comment's "What is drawn", "Sizes" and "Borders" sections.
- [ ] **Step 4: Run.** `solo_frames_are_the_pre_party_frames` unchanged. Re-pin the 8 busy view frames (two tiles). Pin the new party frames. Mutate once: let a tile draw into its last column, see the border test red; let `party_grid` return a tile of 23 columns, see the table test red; restore both.
- [ ] **Step 5: Commit** `feat(saga): the party view, a tile per floor with a roster and focus keys`.

Goldens moved: the 8 busy view frames. Added: 8 party frames. Digests: none.

### Task A.8: Docs, versions, CI, verification (0.17.0)

**Files:**
- Modify: `docs/saga.md` (the party, the tiles, the keys, the pace, the digest version, limits), `AGENTS.md` (Saga section: the party, the re-pin table rows from A.2 to A.7, new rulings), `README.md`, `CHANGELOG.md`, `debian/changelog`, `QUICK_START.md`, `site/index.html` (if it names Saga), `Cargo.toml` (0.17.0), `crates/tt-toplike-saga/Cargo.toml` (0.2.0), `Cargo.lock`
- Modify: `docs/superpowers/specs/2026-11-03-saga-party-design.md` (status: built; any number that moved, such as the 5.2 table)

- [ ] **Step 1:** write the docs. `docs/saga.md` gets a "The party" section, the new glyphs and keys in its tables, and a limits line for "chips past 32 have no floor".
- [ ] **Step 2:** run the plain-English sweep over every file added or changed in Phase A.
- [ ] **Step 3:** run every CI form, then the smoke: record `--headless --mock 4` and `--headless --mock 32` for 12 seconds each (no `--mock-workloads`), print `--bus-inspect DIR --saga-digest` for each, `--replay` each into a second directory and print again: the pairs match. Record a second 4-chip session: its digest differs. Run the TUI with `--mock 1`, `--mock 4`, `--mock 32` at 80x24 and 140x40 in a pty and look at the frames (a person or the `run` skill). Measure quiet-world traffic at 140x40 with `--mock 32` by the pty method: under 20 KB/s.
- [ ] **Step 4:** build the release binary; install only when the user asks.
- [ ] **Step 5: Commit** `feat(saga): a party of heroes, one per chip, in one merged chronicle (0.17.0)`.

---

## Phase B: crossing (0.18.0)

Start only after the user answers the addendum's question 1. Phase B's goldens move under digest version 3.

### Task B.1: `Event::LinkPeer` on the bus (bus crate 0.2.0)

**Files:**
- Modify: `crates/tt-toplike-bus/src/record.rs` (the variant), `crates/tt-toplike-bus/src/inspect.rs` (entities of the event), `crates/tt-toplike-bus/Cargo.toml` (0.2.0)
- Modify: the reader path in `crates/tt-toplike-bus/src/log.rs` or `read.rs` only if the tolerance test fails
- Modify: `src/bus/view.rs` (`knows` arm), `crates/tt-toplike-saga/src/saga.rs` (`event_cause` arm: `(Some(port), "LinkPeer")`)
- Test: `crates/tt-toplike-bus/src/record.rs` tests, a new `crates/tt-toplike-bus/tests/unknown_event.rs`

**Interfaces:**
- `Event::LinkPeer { port: EntityKey, peer: EntityKey }` (both `EntityKind::EthPort`; serde tag `"LinkPeer"`).
- If needed: the 0.1 reader treats a record with an unknown event `t` as a skipped line and counts it (as it counts bad lines today), and reads the rest.

- [ ] **Step 1: Write the failing tests:** `link_peer_round_trips`, `a_reader_skips_an_event_kind_it_does_not_know_and_reads_the_rest` (a log line with `"t":"FutureKind"` between two good records), `link_peer_names_both_ports_as_entities`.
- [ ] **Step 2: Run** and see red (variant absent; the tolerance test shows how today's reader behaves).
- [ ] **Step 3: Implement** the variant, and the tolerant path only if step 2 showed one is needed.
- [ ] **Step 4: Run** every bus test and the root crate's bus tests. Mutate once: give `LinkPeer` a different tag name and see the round trip red; restore.
- [ ] **Step 5: Commit** `feat(bus): LinkPeer, an event that names both ends of an Ethernet link (bus 0.2.0)`.

Goldens moved: none (no fixture holds the event).

### Task B.2: Link peers from the mock backend, and the crossing fixture

Written for the default answer to question 1: mock and recorded logs now, a real source later.

**Files:**
- Modify: `src/bus/source.rs` (emit `LinkPeer` once per pair when the backend reports peers, and again on change), `src/backend/mock.rs` (a ring of peers for its chips), `src/models/telemetry.rs` (an optional peer list on the device, `None` for every real backend)
- Modify: `crates/tt-toplike-saga/tests/support/party.rs` (`records_with_links(chips: u8, ticks: u64) -> Vec<Record>`: the party log plus a ring of `LinkPeer` events at tick 1, a model on chips 0 and 1 at tick 80, a link down on chip 1's ring port at tick 400 and up again at 430)
- Create: `crates/tt-toplike-saga/tests/fixtures/cross4.jsonl`
- Test: `src/bus/source.rs` tests, `src/backend/mock.rs` tests, `crates/tt-toplike-saga/tests/party.rs`

**Interfaces:**
- `pub eth_peers: Option<Vec<EthPeer>>` with `pub struct EthPeer { pub port: u8, pub peer_bus_id: String, pub peer_port: u8 }` on the device telemetry. The mock fills it: chip `i`'s highest live port to chip `(i + 1) mod n`'s lowest live port, marked `Estimated` like every mock reading.

- [ ] **Step 1: Write the failing tests:** `the_mock_reports_a_ring_of_peers`, `a_peer_list_emits_one_link_peer_per_pair_and_none_when_unchanged`, `a_real_backend_emits_no_link_peer`, `the_committed_cross_fixture_matches_the_generator`, `the_cross_fixture_carries_no_real_names`.
- [ ] **Step 2: Run** and see red. **Step 3: Implement.** **Step 4: Run;** mutate once (emit on every tick) and see the "none when unchanged" test red; restore. **Step 5: Commit** `feat(bus): the mock backend reports Ethernet peers as LinkPeer events`.

Goldens moved: none in the saga (no saga fixture changes; `cross4.jsonl` is new and pinned in B.4). Root-crate goldens for `--mock` frames do not move: the event draws nothing.

### Task B.3: Portals bound to ports, and the link table

**Files:**
- Modify: `crates/tt-toplike-saga/src/topology.rs` (`FloorSpec::portal_ports`), `crates/tt-toplike-saga/src/maze.rs` (`Floor::portals`), `crates/tt-toplike-saga/src/saga.rs` (record peers and masks)
- Create: `crates/tt-toplike-saga/src/links.rs` (register in `lib.rs`) and `crates/tt-toplike-saga/src/links_tests.rs`
- Modify: `src/animation/die3d/saga_floor_tests.rs` (portal order against Die 3D's cage order)
- Test: `crates/tt-toplike-saga/src/topology_tests.rs`, `crates/tt-toplike-saga/src/maze_tests.rs`

**Interfaces:**
- `pub struct FloorSpec { ..., pub portal_ports: Vec<u8> }` replacing `portals: u8` (the count is `portal_ports.len()`). Blackhole: live ports among the low 12, ascending; a missing mask gives 0 to 11. Generic: the live ports among the low 16 when the mask is known, else `[0, 1]`.
- `pub struct Floor { ..., pub portals: Vec<(u8, (u8, u8))> }`, west to east, one per `portal_ports` entry, from the same `spread` draws as today.
- `pub struct Links`; `Links::observe_mask(&mut self, chip: &EntityKey, mask: u64, cause: Cause)`; `Links::observe_peer(&mut self, port: &EntityKey, peer: &EntityKey, cause: Cause)`; `Links::peer(&self, chip: &EntityKey, port: u8) -> Option<(EntityKey, u8)>`; `Links::live(&self, chip: &EntityKey, port: u8) -> bool`; `Links::open(&self, world: &World, floor: u8, port: u8) -> Option<(u8, u8)>` (the far floor and port when bound and both ends live). Capped at `MAX_CHIPS` chips and 64 ports each; a peer record for a chip that is not kept is ignored.
- `DIGEST_VERSION` becomes 3 here, before the crossing events exist, so B.3's golden moves stand apart from B.4's.

- [ ] **Step 1: Write the failing tests:** `portal_ports_follow_the_live_mask`, `portals_are_listed_west_to_east_with_their_ports`, `a_blackhole_floor_keeps_its_cells` (for 500 seeds and masks, the cells equal what today's `portals: popcount` gives), `a_portal_without_a_peer_record_never_opens`, `a_portal_opens_only_while_both_ends_are_live`, `a_peer_on_a_chip_without_a_floor_leaves_the_portal_unbound`, `the_link_table_is_bounded` (fuzz: 64 chips x 64 ports of peers), and in the root crate `portal_order_matches_die3d_cage_order`.
- [ ] **Step 2: Run** and see red. **Step 3: Implement.** **Step 4: Run.** Re-pin digest goldens: every row moves by the version byte; generic floors with a known mask change cells (`quiet.jsonl` if its chips are generic). Re-pin the view frames that draw a generic floor whose portal count changed, and say which. Mutate once: drop the far end's live check in `open`, see `a_portal_opens_only_while_both_ends_are_live` red; restore.
- [ ] **Step 5: Commit** `feat(saga): portals know their Ethernet port, and a link table from the bus (digest version 3)`.

Goldens moved: every digest row (version 3); view frames of generic floors whose mask popcount is not 2. Solo story print: unchanged if the solo chip is Blackhole-shaped (the A.1 generator makes chip 0 Blackhole, so it is).

### Task B.4: The crossing quest

**Files:**
- Modify: `crates/tt-toplike-saga/src/quest.rs` (`Verb::Cross`, its place in `plan`), `crates/tt-toplike-saga/src/event.rs` (`HeroCrossed`, `CrossingFailed`), `crates/tt-toplike-saga/src/saga.rs` (hooks, transit, failure, beats, encoding), `crates/tt-toplike-saga/src/chronicle.rs` and `pack.rs` (`BeatKind::CrossOut`, `CrossIn`, `CrossDark`, `CrossFailed` templates), `crates/tt-toplike-saga/src/hero.rs` (`Hero::transit`)
- Test: `crates/tt-toplike-saga/src/saga_tests.rs`, `crates/tt-toplike-saga/tests/party.rs`, `crates/tt-toplike-saga/tests/determinism.rs`

**Interfaces:**
- `Verb::Cross` (digest code 7), placed in `plan` after the sage and before rest; its goal is the portal's cell; its target is the far port's key.
- `pub struct Transit { pub to_floor: u8, pub to_port: u8, pub from_port: u8, pub until: u64 }`; `Hero` gains `pub transit: Option<Transit>`. `pub const TRANSIT_TICKS: u64 = 3;`
- `SagaEvent::HeroCrossed { world: u64, member: u8, from_floor: u8, from_port: u8, to_floor: u8, to_port: u8, cause: Cause }` (tag `0x07`), `SagaEvent::CrossingFailed { world: u64, member: u8, floor: u8, port: u8, cause: Cause }` (tag `0x08`). `TAG_END` adds one byte per member: 0 on a floor, 1 in transit.
- Hooks: a `ModelReady` whose chips include both ends of an open bound link gives each of those floors one `Cross` hook through it (reason: the `ModelReady` cause); a `LinkChanged` that sets the bit of a bound portal gives its floor one (reason: that event). At most one `Cross` hook per port per floor. Crossing beats are notable (outside the routine budget).
- Rules exactly as the addendum 8.4 and 8.5.

- [ ] **Step 1: Write the failing tests:** `a_model_on_both_ends_sends_a_hero_across`, `a_link_coming_up_offers_one_crossing`, `a_crossing_writes_a_line_on_each_floor`, `a_hero_in_transit_is_on_no_map_for_three_ticks`, `a_link_down_in_transit_throws_the_hero_back`, `a_dark_portal_drops_the_hook_and_replans`, `a_flapping_link_adds_one_hook_per_rise`, `a_crossed_hero_takes_its_vitality_from_the_far_chip`, `crossing_beats_skip_the_routine_budget`, `the_cross_log_gives_the_same_digest_after_renumbering`, and the `cross4.jsonl` rows of the golden table.
- [ ] **Step 2: Run** and see red. **Step 3: Implement.** Update the module comments (saga "Crossing" section, the event table, quest order).
- [ ] **Step 4: Run.** Pin `cross4.jsonl`. The other fixtures hold no `LinkPeer`, so no crossing happens in them: their digests do not move from B.3. Mutate once: skip the link check during transit, see `a_link_down_in_transit_throws_the_hero_back` red; restore.
- [ ] **Step 5: Commit** `feat(saga): heroes cross Ethernet portals into the linked chip's floor`.

Goldens moved: none of the existing rows. Added: `cross4.jsonl` (both modes).

### Task B.5: Crossing in the view

**Files:**
- Modify: `src/ui/tui/saga_party.rs`, `src/ui/tui/saga_view.rs` (portal colour by the far floor's hue, open or closed; transit glow on both portals; HUD `crossing F2 → F5`; a tile with no hero; several heroes on one tile; the roster's transit glyph; legend rows)
- Test: `src/ui/tui/saga_party_tests.rs`, `src/ui/tui/saga_view_tests.rs`

**Interfaces:**
- `fn portal_style(state, links_view, floor, port, frame) -> Style`; the core exposes what the view needs through `SagaState`: `pub portals: Vec<PortalState>` with `pub struct PortalState { pub floor: u8, pub port: u8, pub far: Option<(u8, u8)>, pub open: bool }`, rebuilt by the core on each link change (part of the state, so it is published with the snapshot; it is not part of the digest).
- A tile with no member centres its window on the floor's gate and shows `no hero here` in its HUD.

- [ ] **Step 1: Write the failing tests:** `an_open_portal_takes_the_far_floor_hue`, `a_closed_portal_is_dim`, `a_hero_in_transit_glows_on_both_portals`, `an_empty_floor_tile_says_so`, `two_heroes_on_one_tile_both_draw`, `a_floor_keeps_its_memory_after_its_hero_leaves`, the size and border tests re-run over the cross log, and golden frames for `cross4.jsonl` at 80x23 and 140x39 (at tick 410, mid-failure, and at the end).
- [ ] **Step 2: Run** and see red. **Step 3: Implement.** **Step 4: Run.** Solo frames unchanged; party4 and busy frames unchanged (no peers). Mutate once (draw an unbound portal as open) and see red; restore.
- [ ] **Step 5: Commit** `feat(saga): portals, crossings and travellers drawn on every tile`.

Goldens moved: none existing. Added: the cross frames.

### Task B.6: Docs, versions, verification (0.18.0)

**Files:** `docs/saga.md` (crossing, portal colours, "the far side is unknown" on real hardware until a source exists), `AGENTS.md` (re-pin rows for B.3 to B.5, the version 3 encoding), `docs/bus.md` (`LinkPeer`), `README.md`, `CHANGELOG.md`, `debian/changelog`, `Cargo.toml` (0.18.0), `crates/tt-toplike-saga/Cargo.toml` (0.3.0), `Cargo.lock`, the addendum's status line.

- [ ] **Step 1:** write the docs. **Step 2:** the plain-English sweep. **Step 3:** every CI form; the smoke from A.8 with `--mock 4` and `--mock 32`: digests of recording and replay match, and the chronicle of the 4-chip run shows at least one crossing with a `~` mark. **Step 4:** build the release binary; install only when asked. **Step 5: Commit** `feat(saga): heroes cross Ethernet links between chips (0.18.0)`.

---

## Self-review

**Coverage against the addendum.**

| Addendum section | Task |
|---|---|
| 3.1 one hero per floor, born with the world, model dungeon spans every chip | A.3 |
| 3.2 unique names | A.3 |
| 3.3 hooks per member, `target_label` | A.3, A.4 |
| 3.4 vitality, step order, tone | A.3 |
| 3.5 floor on `Beat`, `PartyBorn`, routine budget, `clean_cause`, `unwritten`, names | A.2, A.4 |
| 3.6 digest version 2 | A.2 |
| 3.7 caps, no lazy floors, cost at 32, `Arc` floors | A.5 |
| 3.8 phase 2 attachment points | none (documented only) |
| 4 determinism, streams | A.3 (streams), A.7 (`the_core_never_reads_view_state`) |
| 5.1 to 5.5 grid, tiles, roster, chronicle tags, keys | A.6, A.7 |
| 5.6 one chip unchanged, 80 x 24 | A.1 pins, A.2 to A.7 keep |
| 7 fixtures and tests | A.1 first, then each task |
| 8.1, 8.2 `LinkPeer`, portals bound, link table | B.1, B.2, B.3 |
| 8.3 to 8.5 crossing quest, transit, failure | B.4 |
| 8.6 version 3, caps, mock peers | B.2, B.3, B.4 |
| drawing of crossing | B.5 |

**Type consistency.** `Member` (A.3) is used by A.4 (`unwritten`), A.6 (`on_floor`), A.7 and B.4 (`hero.transit`). `Beat::floor` and `BeatCtx::floor` (A.2) feed A.4's budget and A.7's tags. `SagaFlags` grows once, in A.7. `PartyMemory` (A.6) is what `render_saga` takes in A.7. `FloorSpec::portal_ports` replaces `portals` in B.3; the root crate's R10 cross-check test reads it there. `World::floors` becomes `Arc<[Floor]>` in A.5, after the tasks that build worlds, so A.3 and A.4 keep `Vec<Floor>`. `DIGEST_VERSION` is 2 from A.2 and 3 from B.3.

**Proportion.** Phase A has eight tasks. A.3 and A.7 are the largest. A.3 cannot split further without leaving a state where hooks or streams are shared by members, and A.7 is one screen. A.2 and A.5 are small on purpose: each isolates golden moves (A.2 the encoding, A.5 none) so a reviewer can tell a story change from an encoding change. Phase B has six. B.1 and B.2 are small and gate on the user's answer. The pinned story print in A.1 is the cheapest test in the plan and guards the requirement most likely to break unnoticed.

**Known gaps.** The real link-peer source is not in this plan (question 1). Per-floor collapse on a reset is not in this plan (question 2). Floors emptying after crossings follow the addendum's default (question 3).

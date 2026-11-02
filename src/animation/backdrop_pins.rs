// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pinned frames of every view that draws a backdrop, taken before the shared
//! backdrop existed (`what-if` at d6560a27), so the default looks can be shown
//! to be byte-identical after it.
//!
//! What is pinned:
//!
//! * the Inference view's backdrop: the atmosphere's canvas for each scene,
//!   composited under a fixed foreground, at several sizes and frames (the
//!   snake itself reads the wall clock, so its own frames cannot be hashed);
//! * the Training view (its aurora and stars) for no run, a run with a log
//!   and a run with no log, at several sizes and updates;
//! * the arcade, with the cloudfield off and on, idle, loading and serving,
//!   at several sizes and updates.
//!
//! The Die 3D view's clouds are pinned in `die3d/backdrop_pins.rs`, which can
//! use that module's test backend.
//!
//! `backdrop_pin_hashes` prints the table; the test compares against it.

use crate::animation::atmosphere::{composite, Atmosphere};
use crate::animation::bus_test_support::{frame_hash, tc, Fixed};
use crate::animation::{ArcadeVisualization, TrainView};
use crate::backend::TelemetryBackend;
use crate::workload::inference_server::metrics::ServingStats;
use crate::workload::inference_server::{mock::mock_services, Phase, ServiceState};
use crate::workload::train::{LogSource, StepSample, TrainEvent, TrainProcess, TrainState};
use ratatui::text::Line;

/// The sizes every pin is taken at.
pub(crate) const SIZES: [(usize, usize); 4] = [(40, 12), (80, 24), (120, 40), (200, 60)];

/// One service row in `phase`, with `f` applied to its serving stats when
/// it has them.
pub(crate) fn row(phase: Phase, f: impl FnOnce(&mut ServingStats)) -> ServiceState {
    let mut s = mock_services(0.0).remove(0);
    s.key = "m".into();
    s.label = "m".into();
    s.phase = phase;
    s.cpu_pct = 0.0;
    s.kernel_count = 0;
    s.loaded_count = 0;
    s.media = None;
    let mut sv = ServingStats::zeroed();
    f(&mut sv);
    s.serving = Some(sv);
    s
}

/// The rosters the scene pins feed, one per update `i`: each scene the
/// atmosphere draws, with counters that rise so flares, sparks and the
/// forge's beams appear.
pub(crate) fn scene_rosters() -> Vec<(&'static str, Box<dyn Fn(u64) -> Vec<ServiceState>>)> {
    vec![
        ("none", Box::new(|_| Vec::new())),
        (
            "forge",
            Box::new(|i| {
                let mut s = row(Phase::Loading, |_| {});
                s.serving = None;
                s.kernel_count = 100 + 40 * i as usize;
                s.loaded_count = 2 * i as usize;
                s.cpu_pct = 180.0;
                vec![s]
            }),
        ),
        (
            "storm",
            Box::new(|_| {
                vec![row(Phase::Ready, |s| {
                    s.requests_running = 2;
                    s.requests_waiting = 3;
                })]
            }),
        ),
        (
            "aurora",
            Box::new(|i| {
                vec![row(Phase::Ready, |s| {
                    s.generation_tps = 80.0;
                    s.prompt_tps = 400.0;
                    s.requests_running = 3;
                    s.requests_waiting = 2;
                    s.kv_cache_usage = 0.6;
                    s.counters.requests_succeeded_total = 2 * i;
                })]
            }),
        ),
        ("dream", Box::new(|_| vec![row(Phase::Ready, |_| {})])),
        (
            "stalled",
            Box::new(|_| {
                let mut s = row(Phase::Alarm, |_| {});
                s.serving = None;
                vec![s]
            }),
        ),
    ]
}

/// A foreground with text on some rows and blanks on others, as a view
/// draws.
pub(crate) fn foreground(w: usize, h: usize) -> Vec<Line<'static>> {
    (0..h)
        .map(|y| {
            Line::from(match y % 4 {
                0 => format!("row {y} ").repeat(w / 8),
                1 => String::new(),
                2 => " ".repeat(w / 2) + "x",
                _ => "  ab  cd".to_string(),
            })
        })
        .collect()
}

/// The updates after which a frame is hashed.
pub(crate) const HASHED_AT: [u64; 3] = [3, 25, 90];

/// The atmosphere's frames for one roster at `w` x `h`.
fn scene_hashes(rows: &dyn Fn(u64) -> Vec<ServiceState>, w: usize, h: usize) -> Vec<u64> {
    let mut a = Atmosphere::new();
    let mut out = Vec::new();
    for i in 1..=*HASHED_AT.last().unwrap() {
        a.observe(&rows(i));
        if HASHED_AT.contains(&i) {
            let c = a.render_backdrop(w, h);
            out.push(frame_hash(&composite(&c, foreground(w, h), w)));
        }
    }
    out
}

/// The trainer process the pinned runs use.
fn proc_of(pid: i32) -> TrainProcess {
    TrainProcess {
        pid,
        binary: "nano_gpt".into(),
        config_path: None,
        start_time: Some(99),
    }
}

/// No run, a run with a log, and a run whose stdout is not redirected (the
/// same states `train_bus_tests` pins).
pub(crate) fn train_states() -> Vec<(&'static str, TrainState)> {
    let scanning = TrainState::new();
    let mut live = TrainState::new();
    live.proc = Some(proc_of(4242));
    live.log = Some(LogSource::File("/tmp/train.log".into()));
    live.apply_event(TrainEvent::MaxSteps(500));
    for i in 1..=60u64 {
        live.apply_event(TrainEvent::Step {
            step: i,
            loss: 4.0 - (i as f32) * 0.04 + ((i % 5) as f32) * 0.03,
        });
    }
    live.step_history = (1..=40u64)
        .map(|i| StepSample {
            step: 20 + i,
            seq: i,
            ms: 300.0 + ((i * 37) % 90) as f32,
            cache_delta: u32::from(i == 3),
            checkpoint: i == 30,
        })
        .collect();
    live.step_seq = 40;
    live.step_ms = 320.0;
    let mut blind = TrainState::new();
    blind.proc = Some(proc_of(77));
    blind.log = Some(LogSource::NotRedirected);
    vec![("scanning", scanning), ("live", live), ("blind", blind)]
}

/// The Training view's frames for `st` at `w` x `h`.
pub(crate) fn train_hashes(st: &TrainState, w: usize, h: usize) -> Vec<u64> {
    let backend = Fixed::new(2);
    let mut v = TrainView::new(w, h);
    let mut out = Vec::new();
    for i in 1..=*HASHED_AT.last().unwrap() {
        v.update();
        if HASHED_AT.contains(&i) {
            out.push(frame_hash(&v.render(st, &backend)));
        }
    }
    out
}

/// The arcade rosters: idle, a model loading, a model serving.
pub(crate) fn arcade_rosters() -> Vec<(&'static str, Box<dyn Fn(u64) -> Vec<ServiceState>>)> {
    let all = scene_rosters();
    let mut out: Vec<(&'static str, Box<dyn Fn(u64) -> Vec<ServiceState>>)> = Vec::new();
    for (name, f) in all {
        if ["none", "forge", "aurora"].contains(&name) {
            out.push((
                match name {
                    "none" => "idle",
                    "forge" => "loading",
                    _ => "serving",
                },
                f,
            ));
        }
    }
    out
}

/// The default arcade's frames at `w` x `h` with `rows` fed each update,
/// the cloudfield on or off. `setup` runs once after construction.
pub(crate) fn arcade_hashes_with(
    rows: &dyn Fn(u64) -> Vec<ServiceState>,
    w: usize,
    h: usize,
    setup: impl FnOnce(&mut ArcadeVisualization),
) -> Vec<u64> {
    let backend = Fixed::new(2);
    let mut a = ArcadeVisualization::new(w, h);
    a.initialize_from_devices(backend.devices());
    a.initialize_topology(&backend);
    setup(&mut a);
    let mut out = Vec::new();
    for i in 1..=*HASHED_AT.last().unwrap() {
        a.set_services(rows(i));
        // A fixed clock: one backdrop step (a quarter second) per update.
        a.set_clock(i as f64 / 4.0);
        a.update(&backend);
        if HASHED_AT.contains(&i) {
            out.push(frame_hash(&a.render(&backend)));
        }
    }
    out
}

/// Every pin, by name.
pub(crate) fn all_hashes() -> Vec<(String, Vec<u64>)> {
    tc(|| {
        let mut out = Vec::new();
        for (name, rows) in scene_rosters() {
            for (w, h) in SIZES {
                out.push((format!("scene {name} {w}x{h}"), scene_hashes(&*rows, w, h)));
            }
        }
        for (name, st) in train_states() {
            for (w, h) in SIZES {
                out.push((format!("training {name} {w}x{h}"), train_hashes(&st, w, h)));
            }
        }
        for (name, rows) in arcade_rosters() {
            for on in [false, true] {
                for (w, h) in SIZES {
                    out.push((
                        format!("arcade {name} cloudfield {on} {w}x{h}"),
                        arcade_hashes_with(&*rows, w, h, |a| a.set_cloudfield(on)),
                    ));
                }
            }
        }
        out
    })
}

/// Prints the table for [`PINS`]. Run with
/// `cargo test --locked --lib --features tui backdrop_pin_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn backdrop_pin_hashes() {
    for (name, h) in all_hashes() {
        println!("    ({name:?}, {h:?}),");
    }
}

#[test]
fn every_view_that_draws_a_backdrop_draws_its_pinned_frames() {
    let got = all_hashes();
    let mut bad = Vec::new();
    for (name, h) in &got {
        match PINS.iter().find(|(n, _)| n == name) {
            Some((_, want)) if want.as_slice() == h.as_slice() => {}
            Some((_, want)) => bad.push(format!("{name}: {h:?}, pinned {want:?}")),
            None => bad.push(format!("{name}: not pinned")),
        }
    }
    assert_eq!(got.len(), PINS.len(), "pins added or lost");
    assert!(bad.is_empty(), "frames changed:\n{}", bad.join("\n"));
}

/// The frames at d6560a27, before the shared backdrop, except the eight
/// `arcade loading|serving cloudfield true` pins: those were taken again
/// when the shared scene moved to the 4 Hz step clock with banded colours
/// (with the old clock and no banding they match the d6560a27 frames).
const PINS: &[(&str, [u64; 3])] = &[
    (
        "scene none 40x12",
        [
            5341728465396292711,
            5341728465396292711,
            5341728465396292711,
        ],
    ),
    (
        "scene none 80x24",
        [
            2537827879127095013,
            2537827879127095013,
            2537827879127095013,
        ],
    ),
    (
        "scene none 120x40",
        [
            6167713035941879295,
            6167713035941879295,
            6167713035941879295,
        ],
    ),
    (
        "scene none 200x60",
        [
            5663255985622181805,
            5663255985622181805,
            5663255985622181805,
        ],
    ),
    (
        "scene forge 40x12",
        [
            13988148242237696957,
            17917602971682783209,
            3449599755938238049,
        ],
    ),
    (
        "scene forge 80x24",
        [
            12116164879473776353,
            5621863147395944253,
            13167193712620402559,
        ],
    ),
    (
        "scene forge 120x40",
        [
            12529036441686393089,
            4668274563728489910,
            10333147515859293765,
        ],
    ),
    (
        "scene forge 200x60",
        [
            16384949390528190805,
            7626973917396082831,
            9054979931941884903,
        ],
    ),
    (
        "scene storm 40x12",
        [
            14729487889020419774,
            8751089629064971824,
            16584785482101805098,
        ],
    ),
    (
        "scene storm 80x24",
        [
            12471837299693213556,
            6788624752559027423,
            6477928052392470292,
        ],
    ),
    (
        "scene storm 120x40",
        [
            7501660118701226915,
            3477683195348792462,
            14074163767476702080,
        ],
    ),
    (
        "scene storm 200x60",
        [
            7898636473611231575,
            14410812775682168383,
            1774710449407968686,
        ],
    ),
    (
        "scene aurora 40x12",
        [
            7477069411451181023,
            16796590962908486305,
            15835115172421696277,
        ],
    ),
    (
        "scene aurora 80x24",
        [
            9413081724237758708,
            2054795534590360555,
            1018351696725929886,
        ],
    ),
    (
        "scene aurora 120x40",
        [
            8602798012847796534,
            16407929915392665731,
            16324034820339556156,
        ],
    ),
    (
        "scene aurora 200x60",
        [
            6144851731216295499,
            11159895581297793643,
            4634695987423896270,
        ],
    ),
    (
        "scene dream 40x12",
        [
            14547231913768758346,
            16264459957370458306,
            14140979329169136543,
        ],
    ),
    (
        "scene dream 80x24",
        [
            9137913729387286710,
            16785227753839891775,
            8701996507463393789,
        ],
    ),
    (
        "scene dream 120x40",
        [
            14622136581540599677,
            14883461528966004024,
            1997196390881771015,
        ],
    ),
    (
        "scene dream 200x60",
        [
            9038653232911666544,
            1069330461992608639,
            6166598809585792951,
        ],
    ),
    (
        "scene stalled 40x12",
        [
            14547231913768758346,
            16264459957370458306,
            14140979329169136543,
        ],
    ),
    (
        "scene stalled 80x24",
        [
            9137913729387286710,
            16785227753839891775,
            8701996507463393789,
        ],
    ),
    (
        "scene stalled 120x40",
        [
            14622136581540599677,
            14883461528966004024,
            1997196390881771015,
        ],
    ),
    (
        "scene stalled 200x60",
        [
            9038653232911666544,
            1069330461992608639,
            6166598809585792951,
        ],
    ),
    (
        "training scanning 40x12",
        [
            18090733122687633631,
            12551326265808885122,
            2011210303599161346,
        ],
    ),
    (
        "training scanning 80x24",
        [
            17682413439743215164,
            16757878009566212716,
            17625630610450000900,
        ],
    ),
    (
        "training scanning 120x40",
        [
            321066735817156220,
            5733568942461704964,
            14321109213845301861,
        ],
    ),
    (
        "training scanning 200x60",
        [
            14328292197297663978,
            12063380421452970088,
            7973199095477444135,
        ],
    ),
    (
        "training live 40x12",
        [
            10772930409603924451,
            5669181307519630455,
            17284601775017052787,
        ],
    ),
    (
        "training live 80x24",
        [
            12974181464502405817,
            2016983562302333439,
            1211253356945335620,
        ],
    ),
    (
        "training live 120x40",
        [
            15395696198852481617,
            14322019147958845771,
            13796001712425230965,
        ],
    ),
    (
        "training live 200x60",
        [
            2062409992366281271,
            7576882870517006985,
            7471228736607269017,
        ],
    ),
    (
        "training blind 40x12",
        [
            8924940495053689317,
            8924940495053689317,
            8924940495053689317,
        ],
    ),
    (
        "training blind 80x24",
        [
            15733359841053993914,
            15733359841053993914,
            15836822951462636212,
        ],
    ),
    (
        "training blind 120x40",
        [
            9080913495227729381,
            10855437526555863612,
            16198769285973583893,
        ],
    ),
    (
        "training blind 200x60",
        [
            17492162004082069185,
            10377356677659381414,
            16112052983920006419,
        ],
    ),
    (
        "arcade idle cloudfield false 40x12",
        [
            7197954536597923748,
            10706632041158650043,
            7637469603997005235,
        ],
    ),
    (
        "arcade idle cloudfield false 80x24",
        [
            11082959296705005186,
            17109170901360076133,
            1404937221007063206,
        ],
    ),
    (
        "arcade idle cloudfield false 120x40",
        [
            2715560885361463847,
            9886456109182263071,
            12677530648568637389,
        ],
    ),
    (
        "arcade idle cloudfield false 200x60",
        [
            7839240221613493132,
            16070939611133602559,
            4180234795475111666,
        ],
    ),
    (
        "arcade idle cloudfield true 40x12",
        [
            7197954536597923748,
            10706632041158650043,
            7637469603997005235,
        ],
    ),
    (
        "arcade idle cloudfield true 80x24",
        [
            11082959296705005186,
            17109170901360076133,
            1404937221007063206,
        ],
    ),
    (
        "arcade idle cloudfield true 120x40",
        [
            2715560885361463847,
            9886456109182263071,
            12677530648568637389,
        ],
    ),
    (
        "arcade idle cloudfield true 200x60",
        [
            7839240221613493132,
            16070939611133602559,
            4180234795475111666,
        ],
    ),
    (
        "arcade loading cloudfield false 40x12",
        [
            7197954536597923748,
            10706632041158650043,
            7637469603997005235,
        ],
    ),
    (
        "arcade loading cloudfield false 80x24",
        [
            11082959296705005186,
            17109170901360076133,
            1404937221007063206,
        ],
    ),
    (
        "arcade loading cloudfield false 120x40",
        [
            2715560885361463847,
            9886456109182263071,
            12677530648568637389,
        ],
    ),
    (
        "arcade loading cloudfield false 200x60",
        [
            7839240221613493132,
            16070939611133602559,
            4180234795475111666,
        ],
    ),
    (
        "arcade loading cloudfield true 40x12",
        [
            17600810937517185883,
            5298132627360190466,
            13522251953021424455,
        ],
    ),
    (
        "arcade loading cloudfield true 80x24",
        [
            6166752679609268426,
            4548656637848181461,
            17744006136028677657,
        ],
    ),
    (
        "arcade loading cloudfield true 120x40",
        [
            17485118963406918052,
            10511013143972236741,
            8543344910002935063,
        ],
    ),
    (
        "arcade loading cloudfield true 200x60",
        [
            3133112314097811679,
            8672895268254122397,
            13578347475956915347,
        ],
    ),
    (
        "arcade serving cloudfield false 40x12",
        [
            7197954536597923748,
            10706632041158650043,
            7637469603997005235,
        ],
    ),
    (
        "arcade serving cloudfield false 80x24",
        [
            11082959296705005186,
            17109170901360076133,
            1404937221007063206,
        ],
    ),
    (
        "arcade serving cloudfield false 120x40",
        [
            2715560885361463847,
            9886456109182263071,
            12677530648568637389,
        ],
    ),
    (
        "arcade serving cloudfield false 200x60",
        [
            7839240221613493132,
            16070939611133602559,
            4180234795475111666,
        ],
    ),
    (
        "arcade serving cloudfield true 40x12",
        [
            5559341245018801355,
            6558307764349356329,
            8179624805102117375,
        ],
    ),
    (
        "arcade serving cloudfield true 80x24",
        [
            9977941912129369391,
            5841659665422848906,
            16178686722491176411,
        ],
    ),
    (
        "arcade serving cloudfield true 120x40",
        [
            11736144332369776670,
            7192376284964381782,
            10830265772596426576,
        ],
    ),
    (
        "arcade serving cloudfield true 200x60",
        [
            3514092556317983135,
            18230619572327720344,
            14500107786641651981,
        ],
    ),
];

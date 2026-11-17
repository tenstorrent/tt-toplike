//! The generator behind `tests/fixtures/busy.jsonl`.
//!
//! Ledger ruling R6 says a committed fixture must not carry this machine's
//! process list. A `--mock-workloads` recording would hold one, so this
//! script writes the busy log. Every name in it is made up (`demo-sage`,
//! `demo-scribe`, `demo-run`) and every chip key is a made-up PCI address.
//!
//! # What the script plays
//!
//! [`TICKS`] backend ticks at 10 Hz of record time (100 ms of `t_mono_ns`
//! per tick). Chip A (`chip:0000:01:00.0`) sweeps first on every tick, so it
//! is the pacemaker. Chip B (`chip:0000:02:00.0`) sweeps second.
//!
//! | Ticks | What happens |
//! |---|---|
//! | 1 | slow-lane topology samples for both chips: harvesting state, Tensix column mask (A has columns 6 and 9 harvested), Ethernet live mask |
//! | every tick | one sweep per chip: power, temperature, AI clock; temperature wobbles in its low bits |
//! | every 5 ticks | each chip's ARC heartbeat counter advances (B stalls from tick 550 to 700) |
//! | every 4 ticks | two DRAM channel temperatures on chip A |
//! | 30 to 80 | `demo-sage` loads, with kernel compiles at 40, 55 and 70 |
//! | 80 | `demo-sage` is ready on both chips |
//! | 90 to 440 | request completions every 10 ticks; the temperature climbs from tick 100 |
//! | 300 to 380 | chip B throttles: its clock falls to 900 MHz while its temperature rises |
//! | 200, 260, 320 | a corrected ECC error on chip A channel 3 (counter sample and event) |
//! | 450 | chip B resets; 455: `demo-sage` goes away |
//! | 500 | the view changes (not machine activity) |
//! | 600 | `demo-scribe` is ready on chip A |
//! | 750 to 850 | a training run steps every 2 ticks |
//! | 900 | chip B's link mask changes |
//! | 901 to 1200 | nothing but sweeps and heartbeats: a quiet stretch longer than the tests' 200-tick season |
//!
//! The wobble comes from a small linear congruential generator written out
//! here, so the fixture does not depend on the crate's own generator.

use tt_toplike_bus::{EntityKey, Event, Payload, Provenance, Record, Sample, Sweep, Value};

/// Backend ticks in the log.
pub const TICKS: u64 = 1200;

/// The header line. `backend` and `mode` say where the log came from.
pub const HEADER: &str = r#"{"schema":1,"kind":"Live","tool_versions":{"backend":"generator","mode":"mock","tt-toplike-bus":"0.1.0"}}"#;

/// Nanoseconds of record time per tick (10 Hz).
const TICK_NS: u64 = 100_000_000;

/// The LCG's starting state: a fixed number.
const LCG_START: u64 = 0x5eed_0fb0_5eed_0fb0;

/// A made-up wall clock start, in milliseconds.
const WALL_START_MS: u64 = 1_790_000_000_000;

/// Chip A, the pacemaker.
pub fn chip_a() -> EntityKey {
    EntityKey::chip("0000:01:00.0")
}

/// Chip B.
pub fn chip_b() -> EntityKey {
    EntityKey::chip("0000:02:00.0")
}

/// Builds records with rising sequence numbers and times.
struct Writer {
    out: Vec<Record>,
    tick: u64,
    /// Records written within the current tick, for the time offset.
    within: u64,
    /// LCG state for the wobble.
    lcg: u64,
}

impl Writer {
    fn push(&mut self, source: &str, payload: Payload) {
        let seq = self.out.len() as u64 + 1;
        let t_mono_ns = self.tick * TICK_NS + self.within * 1_000;
        self.within += 1;
        self.out.push(Record {
            seq,
            t_mono_ns,
            t_wall_ms: WALL_START_MS + t_mono_ns / 1_000_000,
            source: source.to_string(),
            payload,
        });
    }

    fn sample(&mut self, source: &str, entity: EntityKey, signal: &str, value: Value) {
        self.push(
            source,
            Payload::Sample(Sample {
                entity,
                signal: signal.to_string(),
                value,
                prov: Provenance::Estimated,
            }),
        );
    }

    fn event(&mut self, source: &str, e: Event) {
        self.push(source, Payload::Event(e));
    }

    /// A number in 0..1 from the LCG (Knuth's MMIX constants).
    fn wobble(&mut self) -> f64 {
        self.lcg = self
            .lcg
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.lcg >> 11) as f64 / (1u64 << 53) as f64
    }

    /// One fast-lane sweep. The temperature is widened from an f32, as the
    /// backend source writes it, so its low bits vary.
    fn sweep(&mut self, chip: EntityKey, temp: f64, clock: u64, power: f64) {
        let temp = (temp + self.wobble() * 0.4) as f32 as f64;
        let power = (power + self.wobble() * 2.0) as f32 as f64;
        self.push(
            "backend",
            Payload::Sweep(Sweep {
                entity: chip,
                signals: vec![
                    "tt_tdp_w".to_string(),
                    "tt_asic_temperature_celsius".to_string(),
                    "tt_ai_clock_mhz".to_string(),
                ],
                values: vec![Value::F64(power), Value::F64(temp), Value::U64(clock)],
                prov: Provenance::Estimated,
            }),
        );
    }
}

/// The temperature of a chip at `tick`: 45 C idle, climbing to 75 C between
/// ticks 100 and 400 while the model serves, and falling back afterwards.
fn temperature(tick: u64, offset: f64) -> f64 {
    let base = 45.0 + offset;
    if (100..400).contains(&tick) {
        base + 30.0 * (tick - 100) as f64 / 300.0
    } else if (400..500).contains(&tick) {
        base + 30.0 * (500 - tick) as f64 / 100.0
    } else {
        base
    }
}

/// Every record of the busy log, in order.
pub fn records() -> Vec<Record> {
    let a = chip_a();
    let b = chip_b();
    let sage = EntityKey::model("demo-sage");
    let scribe = EntityKey::model("demo-scribe");
    let run = EntityKey::training_run("demo-run");
    let dram3 = EntityKey::dram_channel(&a, 3);
    let mut w = Writer {
        out: Vec::new(),
        tick: 0,
        within: 0,
        lcg: LCG_START,
    };
    let mut heartbeat_a = 100u64;
    let mut heartbeat_b = 200u64;
    let mut ecc = 0u64;

    for tick in 1..=TICKS {
        w.tick = tick;
        w.within = 0;

        // Fast lane: A first, so A is the pacemaker.
        w.sweep(a.clone(), temperature(tick, 0.0), 1350, 60.0);
        let b_clock = if (300..380).contains(&tick) {
            900
        } else {
            1350
        };
        w.sweep(b.clone(), temperature(tick, 2.0), b_clock, 55.0);

        // Slow lane: topology once, heartbeats when they change.
        if tick == 1 {
            for (chip, tensix, eth) in [
                (a.clone(), 0x3edf_u64, 0x0fff_u64),
                (b.clone(), 0x3fff, 0x00f0),
            ] {
                w.sample(
                    "backend",
                    chip.clone(),
                    "toplike.harvesting_state",
                    Value::U64(0),
                );
                w.sample(
                    "backend",
                    chip.clone(),
                    "toplike.enabled_tensix_col",
                    Value::U64(tensix),
                );
                w.sample("backend", chip, "toplike.eth.live_mask", Value::U64(eth));
            }
        }
        if tick % 5 == 0 {
            heartbeat_a += 1;
            w.sample(
                "backend",
                a.clone(),
                "toplike.heartbeat",
                Value::U64(heartbeat_a),
            );
            if !(550..700).contains(&tick) {
                heartbeat_b += 1;
                w.sample(
                    "backend",
                    b.clone(),
                    "toplike.heartbeat",
                    Value::U64(heartbeat_b),
                );
            }
        }
        if tick % 4 == 0 {
            for ch in [0u8, 3] {
                let t = (40.0 + ch as f64 + w.wobble() * 0.3) as f32 as f64;
                let e = EntityKey::dram_channel(&a, ch);
                w.sample("backend", e, "toplike.dram.temp_top_c", Value::F64(t));
            }
        }

        // The model's life.
        if tick == 30 {
            w.event(
                "monitor",
                Event::ModelLoading {
                    model: sage.clone(),
                    chips: vec![a.clone(), b.clone()],
                    progress: Some(0.0),
                },
            );
        }
        if [40, 55, 70].contains(&tick) {
            w.event(
                "monitor",
                Event::KernelCompiled {
                    chip: Some(a.clone()),
                    source: "jit".to_string(),
                },
            );
        }
        if tick == 80 {
            w.event(
                "monitor",
                Event::ModelReady {
                    model: sage.clone(),
                    chips: vec![a.clone(), b.clone()],
                },
            );
        }
        if (90..=440).contains(&tick) && tick % 10 == 0 {
            let n = 3 + (w.wobble() * 5.0) as u32;
            w.event(
                "monitor",
                Event::RequestsCompleted {
                    model: sage.clone(),
                    n,
                },
            );
        }
        if [200, 260, 320].contains(&tick) {
            ecc += 1;
            w.sample(
                "backend",
                dram3.clone(),
                "toplike.dram.ecc_corr_rd",
                Value::U64(ecc),
            );
            w.event(
                "backend",
                Event::EccTick {
                    chip: a.clone(),
                    channel: 3,
                },
            );
        }
        if tick == 450 {
            w.event(
                "tt-smi",
                Event::Reset {
                    chips: vec![b.clone()],
                },
            );
        }
        if tick == 455 {
            w.event(
                "monitor",
                Event::ModelGone {
                    model: sage.clone(),
                },
            );
        }
        if tick == 500 {
            w.event(
                "tui",
                Event::ViewChanged {
                    view: "die3d".to_string(),
                },
            );
        }
        if tick == 600 {
            w.event(
                "monitor",
                Event::ModelReady {
                    model: scribe.clone(),
                    chips: vec![a.clone()],
                },
            );
        }
        if (750..=850).contains(&tick) && tick % 2 == 0 {
            w.sample(
                "trainer",
                run.clone(),
                "toplike.train.step",
                Value::U64((tick - 750) / 2),
            );
            let loss = (3.0 - (tick - 750) as f64 * 0.01 + w.wobble() * 0.05) as f32 as f64;
            w.sample(
                "trainer",
                run.clone(),
                "toplike.train.loss",
                Value::F64(loss),
            );
        }
        if tick == 900 {
            w.sample(
                "backend",
                b.clone(),
                "toplike.eth.live_mask",
                Value::U64(0x0030),
            );
            w.event(
                "backend",
                Event::LinkChanged {
                    chip: b.clone(),
                    mask: 0x0030,
                },
            );
        }
    }
    w.out
}

/// The whole fixture file: the header line, then one line per record, each
/// ending in a newline.
pub fn file_text() -> String {
    let mut text = String::from(HEADER);
    text.push('\n');
    for r in records() {
        text.push_str(&r.to_line());
        text.push('\n');
    }
    text
}

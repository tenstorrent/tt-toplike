//! Seeded record streams for the fuzz and bounds tests (Task 1.12).
//!
//! Two generators live here:
//!
//! * [`random_stream`] draws records at random from a seed: random entities
//!   of every kind, random signals (the ones the saga reads and made-up
//!   ones), finite and non-finite values of every [`Value`] variant, events
//!   of every kind, sweeps from many entities, records from source `saga`,
//!   raw frames and gaps, in random order. The same seed always gives the
//!   same stream, so a failing seed can be replayed.
//! * [`flood_stream`] is a fixed adversarial script for the bounds tests: an
//!   event flood, more chips than the saga keeps, more signal names per chip
//!   than the trail book keeps, more ready models than the ready list keeps
//!   (none of them ever goes away), and a model ready on far more chips than
//!   a world keeps. The tests never drain events while they feed it.
//!
//! The only random source is the crate's own [`Pcg32`]. Nothing here reads a
//! clock or the operating system's random source.
//!
//! This file is shared: the integration test `tests/fuzz.rs` reaches it
//! through `tests/support/mod.rs`, and the crate's unit tests in
//! `src/fuzz_tests.rs` include it with a `#[path]` attribute (they need the
//! saga's private collections and the test-only work counter). It names the
//! crate as `tt_toplike_saga` in both places; `src/lib.rs` gives the crate
//! that name for its own unit tests.

use tt_toplike_bus::{
    EntityKey, Event, Gap, Payload, Provenance, RawFrame, Record, Sample, Sweep, Value,
};
use tt_toplike_saga::rng::Pcg32;

/// Records in one fuzz stream (the brief's 20,000).
pub const FUZZ_RECORDS: usize = 20_000;

/// Distinct chip ids the fuzz draws from. More than the saga's chip cap
/// (64), so the cap is reached during a fuzz run as well.
const FUZZ_CHIPS: u32 = 80;

/// Distinct made-up signal names the fuzz draws from.
const FUZZ_SIGNALS: u32 = 300;

/// Distinct model ids the fuzz draws from.
const FUZZ_MODELS: u32 = 90;

/// Signals the saga reads by name, plus names chosen to look like command
/// lines, flags, empty text and non-ASCII text. The fuzz draws from this list
/// more than half the time, so the saga's own code paths are reached often.
const NAMED_SIGNALS: [&str; 16] = [
    "toplike.harvesting_state",
    "toplike.enabled_tensix_col",
    "toplike.eth.live_mask",
    "toplike.heartbeat",
    "tt_asic_temperature_celsius",
    "tt_ai_clock_mhz",
    "tt_tdp_w",
    "toplike.train.step",
    "toplike.train.loss",
    "saga.heartbeat_alive",
    "saga.temperature_baseline",
    "",
    "python3 -m vllm serve --model x",
    "--flag=1",
    "temp\u{2603}\u{200b}",
    "toplike.train.",
];

/// Sources a fuzz record may come from. `saga` records are skipped by the
/// feed, and the fuzz checks that they are.
const SOURCES: [&str; 5] = ["backend", "inference", "saga", "", "gozer"];

/// Builds random records from one seed.
pub struct Fuzz {
    rng: Pcg32,
    /// Records made so far; used for `seq` and the times, which the saga
    /// must never read.
    made: u64,
}

impl Fuzz {
    /// A generator for `seed`. The stream selector is fixed, so the seed
    /// alone decides the stream.
    pub fn new(seed: u64) -> Fuzz {
        Fuzz {
            rng: Pcg32::new(seed, 0xf022),
            made: 0,
        }
    }

    /// A number in `0..n`.
    fn below(&mut self, n: u32) -> u32 {
        self.rng.range(0, n)
    }

    /// True `num` times in `den`.
    fn chance(&mut self, num: u32, den: u32) -> bool {
        self.rng.chance(num, den)
    }

    /// A full 64-bit draw.
    fn u64(&mut self) -> u64 {
        (u64::from(self.rng.next_u32()) << 32) | u64::from(self.rng.next_u32())
    }

    /// A chip key. Ids 0 to 7 contain `blackhole`, so the saga guesses
    /// Blackhole floors for them; a few ids are empty or odd text.
    fn chip(&mut self) -> EntityKey {
        let i = self.below(FUZZ_CHIPS);
        match i {
            0..=7 => EntityKey::chip(&format!("blackhole-{i}")),
            8 => EntityKey::chip(""),
            9 => EntityKey::chip(" spaced id "),
            10 => EntityKey::chip("--rm -rf /"),
            _ => EntityKey::chip(&format!("0000:{i:02x}:00.0")),
        }
    }

    /// A model key, sometimes with an id that a real name must reject.
    fn model(&mut self) -> EntityKey {
        let i = self.below(FUZZ_MODELS);
        match i {
            0 => EntityKey::model(""),
            1 => EntityKey::model("python3 serve.py --port 8000"),
            2 => EntityKey::model("-x"),
            3 => EntityKey::model("meta-llama/Llama-3.1-8B-Instruct"),
            _ => EntityKey::model(&format!("fuzz-model-{i}")),
        }
    }

    /// An entity of any kind. Tiles, DRAM channels and Ethernet ports belong
    /// to a chip; every other kind has no chip.
    fn entity(&mut self) -> EntityKey {
        match self.below(12) {
            0..=3 => self.chip(),
            4 => {
                let chip = self.chip();
                let (x, y) = (self.below(256) as u8, self.below(256) as u8);
                EntityKey::tile(&chip, x, y)
            }
            5 => {
                let chip = self.chip();
                let ch = self.below(256) as u8;
                EntityKey::dram_channel(&chip, ch)
            }
            6 => {
                let chip = self.chip();
                let port = self.below(256) as u8;
                EntityKey::eth_port(&chip, port)
            }
            7 => self.model(),
            8 => EntityKey::process(self.rng.next_u32(), self.u64()),
            9 => EntityKey::lease(&format!("lease-{}", self.below(10))),
            10 => EntityKey::host(&format!("host-{}", self.below(4))),
            _ => match self.below(2) {
                0 => EntityKey::board(&format!("board-{}", self.below(4))),
                _ => EntityKey::training_run(&format!("run-{}", self.below(4))),
            },
        }
    }

    /// A signal name: one the saga reads (or an odd one) most of the time,
    /// otherwise one of [`FUZZ_SIGNALS`] made-up names.
    fn signal(&mut self) -> String {
        if self.chance(3, 5) {
            NAMED_SIGNALS[self.below(NAMED_SIGNALS.len() as u32) as usize].to_string()
        } else {
            format!("fuzz.sig.{}", self.below(FUZZ_SIGNALS))
        }
    }

    /// A value of any variant: ordinary floats, floats from random bits
    /// (which include NaN, the infinities, subnormals and huge values), the
    /// named non-finite floats, integers at their extremes, and text.
    fn value(&mut self) -> Value {
        match self.below(10) {
            0..=2 => Value::F64(f64::from(self.rng.next_u32()) / 1000.0 - 1000.0),
            3 => Value::F64(f64::from_bits(self.u64())),
            4 => Value::F64(match self.below(3) {
                0 => f64::NAN,
                1 => f64::INFINITY,
                _ => f64::NEG_INFINITY,
            }),
            5 => Value::U64(match self.below(4) {
                0 => 0,
                1 => u64::MAX,
                _ => self.u64(),
            }),
            6 => Value::U64(u64::from(self.below(20))),
            7 => Value::I64(match self.below(3) {
                0 => i64::MIN,
                1 => -1,
                _ => self.u64() as i64,
            }),
            8 => Value::Text(format!("text-{}", self.below(100))),
            _ => Value::F64(f64::from(self.below(100)) + 0.25),
        }
    }

    /// Up to three chip keys, sometimes none, now and then a long list (some
    /// of them repeated).
    fn chips(&mut self) -> Vec<EntityKey> {
        let n = if self.chance(1, 50) {
            100
        } else {
            self.below(4)
        };
        (0..n).map(|_| self.chip()).collect()
    }

    /// An event of any of the twelve kinds.
    fn event(&mut self) -> Event {
        match self.below(12) {
            0 => Event::ModelLoading {
                model: self.model(),
                chips: self.chips(),
                progress: if self.chance(1, 2) {
                    Some(f32::from_bits(self.rng.next_u32()))
                } else {
                    None
                },
            },
            1 => Event::ModelReady {
                model: self.model(),
                chips: self.chips(),
            },
            2 => Event::ModelGone {
                model: self.model(),
            },
            3 => Event::RequestsCompleted {
                model: self.model(),
                n: self.rng.next_u32(),
            },
            4 => Event::KernelCompiled {
                chip: if self.chance(1, 2) {
                    Some(self.chip())
                } else {
                    None
                },
                source: "kernel.cpp".to_string(),
            },
            5 => Event::Reset {
                chips: self.chips(),
            },
            6 => Event::LinkChanged {
                chip: self.chip(),
                mask: self.below(1 << 16) as u16,
            },
            7 => Event::LeaseGranted {
                lease: EntityKey::lease("fuzz"),
                chips: self.chips(),
            },
            8 => Event::LeaseReleased {
                lease: EntityKey::lease("fuzz"),
                chips: self.chips(),
            },
            9 => Event::EccTick {
                chip: self.chip(),
                channel: self.below(256) as u8,
            },
            10 => Event::ViewChanged {
                view: "saga".to_string(),
            },
            _ => Event::Spark {
                chip: if self.chance(1, 2) {
                    Some(self.chip())
                } else {
                    None
                },
                kind: "fuzz".to_string(),
            },
        }
    }

    /// A sweep. Half the sweeps come from the first fuzz chip, so the clock
    /// ticks often; the rest come from any entity. One sweep in ten has
    /// signal and value lists of different lengths.
    fn sweep(&mut self) -> Sweep {
        let entity = if self.chance(1, 2) {
            EntityKey::chip("blackhole-0")
        } else {
            self.entity()
        };
        let n = self.below(7) as usize;
        let signals: Vec<String> = (0..n).map(|_| self.signal()).collect();
        let m = if self.chance(1, 10) {
            self.below(7) as usize
        } else {
            n
        };
        let values = (0..m).map(|_| self.value()).collect();
        Sweep {
            entity,
            signals,
            values,
            prov: Provenance::Estimated,
        }
    }

    /// One record: a sample, sweep or event most of the time, now and then
    /// a raw frame or a gap.
    pub fn record(&mut self) -> Record {
        // The first record is a pacemaker sweep, so every stream has a clock.
        let payload = if self.made == 0 {
            Payload::Sweep(Sweep {
                entity: EntityKey::chip("blackhole-0"),
                signals: vec!["tt_tdp_w".to_string()],
                values: vec![Value::F64(50.0)],
                prov: Provenance::Measured,
            })
        } else {
            match self.below(20) {
                0..=6 => Payload::Sample(Sample {
                    entity: self.entity(),
                    signal: self.signal(),
                    value: self.value(),
                    prov: Provenance::Measured,
                }),
                7..=13 => Payload::Sweep(self.sweep()),
                14..=17 => Payload::Event(self.event()),
                18 => Payload::Raw(RawFrame {
                    source: "tt-smi".to_string(),
                    body: "{}".to_string(),
                }),
                _ => Payload::Gap(Gap {
                    reason: "fuzz".to_string(),
                }),
            }
        };
        let source = if self.made == 0 {
            "backend"
        } else if self.chance(1, 20) {
            "saga"
        } else {
            SOURCES[self.below(SOURCES.len() as u32) as usize]
        };
        self.made += 1;
        Record {
            // Sequence numbers and times are random too: the saga must not
            // read them.
            seq: self.u64(),
            t_mono_ns: self.u64(),
            t_wall_ms: self.u64(),
            source: source.to_string(),
            payload,
        }
    }
}

/// `n` random records from `seed`.
pub fn random_stream(seed: u64, n: usize) -> Vec<Record> {
    let mut f = Fuzz::new(seed);
    (0..n).map(|_| f.record()).collect()
}

// ---------------------------------------------------------------------------
// The flood: a fixed adversarial script.
// ---------------------------------------------------------------------------

/// Chips the flood reports. Far more than the saga's cap of 64.
pub const FLOOD_CHIPS: usize = 200;

/// Distinct signal names the flood reports on each chip. More than the trail
/// book's cap of 128.
pub const FLOOD_SIGNALS_PER_CHIP: usize = 300;

/// Models the flood makes ready. None of them ever goes away. More than the
/// ready list's cap of 64.
pub const FLOOD_MODELS: usize = 200;

/// Resets naming no chips in the flood's first phase. Each ends the world and
/// births the next, so each raises eight events or more.
pub const FLOOD_RESETS: usize = 700;

/// Chips named by the flood's one huge `ModelReady`. Far more than a world's
/// 32 floors, and enough that a world with a floor for each would cost more
/// than a million work units.
pub const FLOOD_READY_CHIPS: usize = 1000;

/// Records in the flood that carry a 1 MB key, signal name or event string
/// (ledger ruling R21). The saga drops each one whole.
pub const FLOOD_OVERSIZED: u64 = 5;

/// Bytes in each of the flood's oversized items.
pub const FLOOD_ITEM_BYTES: usize = 1 << 20;

/// The flood's pacemaker chip (it sweeps first).
pub fn flood_pacemaker() -> EntityKey {
    EntityKey::chip("flood-pacemaker")
}

/// The flood's chip `i` (the pacemaker is extra).
pub fn flood_chip(i: usize) -> EntityKey {
    EntityKey::chip(&format!("flood-{i:04}"))
}

/// Builds the flood's records.
struct Flood {
    out: Vec<Record>,
    /// Drives the temperature wobble.
    rng: Pcg32,
}

impl Flood {
    fn push(&mut self, payload: Payload) {
        let seq = self.out.len() as u64 + 1;
        self.out.push(Record {
            seq,
            t_mono_ns: seq * 1_000,
            t_wall_ms: seq,
            source: "backend".to_string(),
            payload,
        });
    }

    fn sample(&mut self, entity: EntityKey, signal: &str, value: f64) {
        self.push(Payload::Sample(Sample {
            entity,
            signal: signal.to_string(),
            value: Value::F64(value),
            prov: Provenance::Measured,
        }));
    }

    fn event(&mut self, e: Event) {
        self.push(Payload::Event(e));
    }

    /// One pacemaker sweep: one tick. The temperature wobbles in its low
    /// bits so the seed pool fills with varied bytes.
    fn tick(&mut self) {
        let temp = 50.0 + f64::from(self.rng.range(0, 1000)) / 997.0;
        self.push(Payload::Sweep(Sweep {
            entity: flood_pacemaker(),
            signals: vec![
                "tt_asic_temperature_celsius".to_string(),
                "tt_ai_clock_mhz".to_string(),
                "tt_tdp_w".to_string(),
            ],
            values: vec![Value::F64(temp), Value::U64(1350), Value::F64(60.0)],
            prov: Provenance::Measured,
        }));
    }
}

/// The flood, in four phases. Nothing in it is drained by the generator; the
/// tests decide.
///
/// 1. **Oversized items, then an event flood on one chip.** 70 ticks to open
///    the seed gate and settle. Then [`FLOOD_OVERSIZED`] records each carry
///    one 1 MB item: a chip key, a signal name on the pacemaker, a ready
///    model's key, a sweep signal, and a kernel compile's source. At this
///    point the chip table, trail book and ready list all have room, so only
///    the size limit can keep them out. Then [`FLOOD_RESETS`] resets naming
///    no chips, one tick after each. Each reset ends the world and births
///    another on the pacemaker's floor.
/// 2. **Many chips, many names.** For each of [`FLOOD_CHIPS`] chips: a
///    topology reading, a heartbeat, a temperature, and
///    [`FLOOD_SIGNALS_PER_CHIP`] distinct signal names; a DRAM channel sample
///    for the chip too. A tick after every chip.
/// 3. **Ready models that never go.** [`FLOOD_MODELS`] models become ready on
///    the pacemaker, one tick after each.
/// 4. **The widest births.** A model ready on [`FLOOD_READY_CHIPS`] chips,
///    then a reset naming no chips (a world on the first 32 of the 64 chips
///    the saga keeps), then 200 ticks with every chip's heartbeat advancing.
pub fn flood_stream() -> Vec<Record> {
    let mut f = Flood {
        out: Vec::new(),
        rng: Pcg32::new(0xf100d, 7),
    };
    let pacemaker = flood_pacemaker();

    // Phase 1.
    for _ in 0..70 {
        f.tick();
    }
    let long = "x".repeat(FLOOD_ITEM_BYTES);
    f.sample(EntityKey::chip(&long), "tt_asic_temperature_celsius", 50.0);
    f.sample(pacemaker.clone(), &long, 1.0);
    f.event(Event::ModelReady {
        model: EntityKey::model(&long),
        chips: vec![pacemaker.clone()],
    });
    f.push(Payload::Sweep(Sweep {
        entity: pacemaker.clone(),
        signals: vec![long.clone()],
        values: vec![Value::F64(1.0)],
        prov: Provenance::Measured,
    }));
    f.event(Event::KernelCompiled {
        chip: Some(pacemaker.clone()),
        source: long,
    });
    f.tick();
    for _ in 0..FLOOD_RESETS {
        f.event(Event::Reset { chips: Vec::new() });
        f.tick();
    }

    // Phase 2.
    for i in 0..FLOOD_CHIPS {
        let chip = flood_chip(i);
        f.sample(chip.clone(), "toplike.enabled_tensix_col", 0x3fff as f64);
        f.sample(chip.clone(), "toplike.heartbeat", 1.0);
        f.sample(chip.clone(), "tt_asic_temperature_celsius", 50.0);
        for k in 0..FLOOD_SIGNALS_PER_CHIP {
            f.sample(chip.clone(), &format!("flood.sig.{k}"), k as f64);
        }
        f.sample(EntityKey::dram_channel(&chip, 0), "dram.temp", 40.0);
        f.tick();
    }

    // Phase 3.
    for i in 0..FLOOD_MODELS {
        f.event(Event::ModelReady {
            model: EntityKey::model(&format!("flood-model-{i:03}")),
            chips: vec![pacemaker.clone()],
        });
        f.tick();
    }

    // Phase 4.
    f.event(Event::ModelReady {
        model: EntityKey::model("flood-everywhere"),
        chips: (0..FLOOD_READY_CHIPS).map(flood_chip).collect(),
    });
    f.tick();
    f.event(Event::Reset { chips: Vec::new() });
    for beat in 0..200u32 {
        for i in 0..FLOOD_CHIPS {
            f.sample(flood_chip(i), "toplike.heartbeat", f64::from(beat + 2));
        }
        f.tick();
    }
    f.out
}

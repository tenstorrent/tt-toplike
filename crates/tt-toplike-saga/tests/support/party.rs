//! The generator behind `tests/fixtures/solo.jsonl`,
//! `tests/fixtures/party4.jsonl` and `tests/fixtures/cross4.jsonl`, and the source of every larger party the
//! party tests need (2, 4 and 32 chips, in memory only).
//!
//! Ledger ruling R6 says a committed fixture must not carry this machine's
//! process list, so the logs come from a script. Every name in them is made
//! up (`demo-sage`, `demo-scribe`) and every chip key is a made-up PCI
//! address (`chip:0000:{i+1:02x}:00.0`, see [`chip`]).
//!
//! # What the script plays, for `n` chips and `ticks` ticks
//!
//! Record time is 100 ms per tick (10 Hz). Chip 0 sweeps first on every
//! tick, so it is the pacemaker. Nothing in the script resets a chip: the
//! reset rule is an open question for the party, and `busy.jsonl` keeps
//! covering the reset of one chip.
//!
//! | Tick | What happens |
//! |---|---|
//! | every tick | one sweep per chip, chip 0 first: power, temperature, AI clock; the low bits of power and temperature wobble from the script's own LCG |
//! | 1 | slow-lane topology for every chip: harvesting state, Tensix column mask (chips alternate between two masks), Ethernet live mask (the popcount differs from chip to chip, so no two neighbours share a mask) |
//! | every 5 | each chip's ARC heartbeat counter advances; chip `n-1` stalls from tick 250 to tick 400 (150 ticks) |
//! | 30 | `demo-sage` starts loading on chips `0..=n/2` |
//! | 80 | `demo-sage` is ready on chips `0..=n/2` |
//! | 90 to `ticks - 200`, every 10 | `demo-sage` completes a few requests |
//! | 100 to 400 | the temperature climbs and falls, as in the busy log |
//! | 300 | `demo-scribe` is ready on chip `n-1` |
//! | 300 to 380 | chip 1 throttles: its clock falls to 900 MHz (only when `n > 1`) |
//! | `ticks - 200` | chip 0's Ethernet mask changes (`LinkChanged`) |
//! | after that | nothing but sweeps and heartbeats: a quiet stretch of 200 ticks |
//!
//! The wobble comes from a small linear congruential generator written out
//! here, so the fixtures do not depend on the crate's own generator.
//!
//! # The links variant ([`records_with_links`])
//!
//! The same script plus four extra things, for a ring of `n` chips (at
//! least 2). These are what `cross4.jsonl` adds:
//!
//! | Tick | What happens |
//! |---|---|
//! | 1 | one `LinkPeer` per chip: chip `i`'s highest live port (the top bit of the chip's Ethernet mask) to chip `(i + 1) mod n`'s lowest live port (always port 0, because every mask starts at bit 0). The mock backend reports the same shape, with both ends among the low 12 ports |
//! | 80 | `demo-pair` is ready on chips 0 and 1, so the pair's two floors can send a hero across the link between them |
//! | 400 | chip 1's ring port (its highest live port) goes down: its mask loses that bit, and a `LinkChanged` follows |
//! | 430 | the same port comes up again: the mask is back, and a `LinkChanged` follows |
//!
//! Chip 0's own mask change at `ticks - 200` (see the first table) still
//! happens, and it takes chip 0's ring port down for good. With
//! [`CROSS4_TICKS`] that is tick 600, after the crossing scenes.
//!
//! The committed files are [`SOLO_TICKS`] ticks of one chip,
//! [`PARTY4_TICKS`] ticks of four and [`CROSS4_TICKS`] ticks of four with
//! links. `n` chips cost about `n * ticks` sweeps,
//! so the 32-chip party is only ever built in memory.

use tt_toplike_bus::{EntityKey, Event, Payload, Provenance, Record, Sample, Sweep, Value};

/// Ticks in `solo.jsonl`.
pub const SOLO_TICKS: u64 = 1200;

/// Ticks in `party4.jsonl`.
pub const PARTY4_TICKS: u64 = 600;

/// Ticks in `cross4.jsonl`. The party's own link change falls at tick 600,
/// after the scripted link down (400) and up (430).
pub const CROSS4_TICKS: u64 = 800;

/// The header line of all three party logs. It names bus 0.2.0, the
/// version that added `LinkPeer`, because `cross4.jsonl` holds that event.
/// (The busy log's header still names 0.1.0: it holds no 0.2.0 event.)
/// Readers ignore the header, so it moves no digest.
pub const HEADER: &str = r#"{"schema":1,"kind":"Live","tool_versions":{"backend":"generator","mode":"mock","tt-toplike-bus":"0.2.0"}}"#;

/// Nanoseconds of record time per tick (10 Hz).
const TICK_NS: u64 = 100_000_000;

/// The LCG's starting state: a fixed number that differs from the busy log's.
const LCG_START: u64 = 0x9a27_1e55_0b0e_51ed;

/// A made-up wall clock start, in milliseconds.
const WALL_START_MS: u64 = 1_790_000_000_000;

/// Chip `i`: `chip:0000:{i+1:02x}:00.0`. Chip 0 is the pacemaker.
pub fn chip(i: u8) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i as u32 + 1))
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

    fn sample(&mut self, entity: EntityKey, signal: &str, value: Value) {
        self.push(
            "backend",
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

    /// One fast-lane sweep. Temperature and power are widened from an f32,
    /// as the backend source writes them, so their low bits vary.
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

/// The temperature of a chip at `tick`: 45 C idle (plus `offset`), climbing
/// by 30 C between ticks 100 and 400 while the model serves, and falling
/// back by tick 500.
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

/// Every record of a party of `chips` chips over `ticks` ticks, in order.
pub fn records(chips: u8, ticks: u64) -> Vec<Record> {
    script(chips, chips, ticks, false)
}

/// The party log of [`records`] plus a ring of `LinkPeer` events at tick 1,
/// a model on chips 0 and 1 at tick 80, and a link down on chip 1's ring
/// port at tick 400 and up again at 430. See the module comment. Needs at
/// least 2 chips and at least 430 ticks.
pub fn records_with_links(chips: u8, ticks: u64) -> Vec<Record> {
    assert!(chips >= 2, "a ring has at least two chips");
    assert!(ticks >= 430, "the link comes back up at tick 430");
    script(chips, chips, ticks, true)
}

/// The Ethernet live mask the script gives chip `i` at tick 1: the low
/// `2 + i % 12` bits.
fn eth_mask(i: usize) -> u64 {
    (1u64 << (2 + i % 12)) - 1
}

/// The highest live port of a mask: the port a ring link leaves from.
fn top_port(mask: u64) -> u8 {
    (63 - mask.leading_zeros()) as u8
}

/// The script of [`records`] for 32 chips, played on `chips` chips (at
/// least 32) over `ticks` ticks. The first 32 chips play the 32-chip script
/// as the table above says: `demo-sage` on chips 0 to 16, `demo-scribe` and
/// the heartbeat stall on chip 31, chip 1 throttling. Every chip after
/// chip 31 sweeps on every tick, reports its topology at tick 1 and its
/// heartbeat every 5 ticks, and plays no other part.
///
/// Chip `i` is `chip:0000:{i+1:02x}:00.0`, so the keys of chips 32 and up
/// sort after the first 32, and a world that keeps 32 floors in key order
/// keeps the first 32 chips. With more than 64 chips the saga keeps the
/// first 64 it sees, which are chips 0 to 63, because chip 0 sweeps first on
/// every tick and the others follow in order.
pub fn records_with_extra_chips(chips: u8, ticks: u64) -> Vec<Record> {
    assert!(chips >= 32, "the extra chips come after the first 32");
    script(chips, 32, ticks, false)
}

/// The script behind [`records`] and [`records_with_extra_chips`]: `chips`
/// chips sweep, of which the first `story` play the models, the stall, the
/// throttle and the link change. `story` is at most `chips`. With `links`
/// the script adds the ring, the pair's model and the link down and up that
/// [`records_with_links`] describes; without it the output is exactly what
/// it was before the links existed, so `solo.jsonl` and `party4.jsonl` stay
/// as they are.
fn script(chips: u8, story: u8, ticks: u64, links: bool) -> Vec<Record> {
    assert!(story >= 1, "a party has at least one chip");
    assert!(story <= chips, "the story's chips are among the chips");
    let n = story as u64;
    let keys: Vec<EntityKey> = (0..chips).map(chip).collect();
    // The last chip of the story, which the scribe serves and whose
    // heartbeat stalls.
    let last = story as usize - 1;
    let sage = EntityKey::model("demo-sage");
    let scribe = EntityKey::model("demo-scribe");
    // The chips that serve demo-sage: 0 through n/2.
    let sage_chips: Vec<EntityKey> = keys[..=(n / 2) as usize].to_vec();
    let link_tick = ticks.saturating_sub(200);
    let mut w = Writer {
        out: Vec::new(),
        tick: 0,
        within: 0,
        lcg: LCG_START,
    };
    let mut heartbeats: Vec<u64> = (0..chips as u64).map(|i| 100 + 100 * i).collect();

    for tick in 1..=ticks {
        w.tick = tick;
        w.within = 0;

        // Fast lane: one sweep per chip, chip 0 first, so chip 0 is the
        // pacemaker.
        for (i, key) in keys.iter().enumerate() {
            let throttled = i == 1 && (300..380).contains(&tick);
            let clock = if throttled { 900 } else { 1350 };
            let power = 55.0 + (i % 3) as f64 * 2.5;
            w.sweep(
                key.clone(),
                temperature(tick, (i % 4) as f64 * 0.5),
                clock,
                power,
            );
        }

        // Slow lane: topology once, heartbeats when they change.
        if tick == 1 {
            for (i, key) in keys.iter().enumerate() {
                let tensix = if i % 2 == 0 { 0x3edf_u64 } else { 0x3fff };
                // A mask of `2 + i % 12` low bits: neighbours differ.
                let eth = eth_mask(i);
                w.sample(key.clone(), "toplike.harvesting_state", Value::U64(0));
                w.sample(
                    key.clone(),
                    "toplike.enabled_tensix_col",
                    Value::U64(tensix),
                );
                w.sample(key.clone(), "toplike.eth.live_mask", Value::U64(eth));
            }
        }
        if tick % 5 == 0 {
            for (i, key) in keys.iter().enumerate() {
                // The last chip's ARC stops answering for 150 ticks.
                if i == last && (250..400).contains(&tick) {
                    continue;
                }
                heartbeats[i] += 1;
                w.sample(key.clone(), "toplike.heartbeat", Value::U64(heartbeats[i]));
            }
        }

        // The links variant: the ring, the pair's model, one port down and up.
        if links {
            if tick == 1 {
                for i in 0..chips as usize {
                    let next = (i + 1) % chips as usize;
                    w.event(
                        "backend",
                        Event::LinkPeer {
                            port: EntityKey::eth_port(&keys[i], top_port(eth_mask(i))),
                            // Every mask starts at bit 0, so port 0 is the
                            // next chip's lowest live port.
                            peer: EntityKey::eth_port(&keys[next], 0),
                        },
                    );
                }
            }
            if tick == 80 {
                w.event(
                    "monitor",
                    Event::ModelReady {
                        model: EntityKey::model("demo-pair"),
                        chips: vec![keys[0].clone(), keys[1].clone()],
                    },
                );
            }
            if tick == 400 || tick == 430 {
                let full = eth_mask(1);
                let mask = if tick == 400 {
                    full & !(1u64 << top_port(full))
                } else {
                    full
                };
                w.sample(keys[1].clone(), "toplike.eth.live_mask", Value::U64(mask));
                w.event(
                    "backend",
                    Event::LinkChanged {
                        chip: keys[1].clone(),
                        mask: mask as u16,
                    },
                );
            }
        }

        // The models' lives.
        if tick == 30 {
            w.event(
                "monitor",
                Event::ModelLoading {
                    model: sage.clone(),
                    chips: sage_chips.clone(),
                    progress: Some(0.0),
                },
            );
        }
        if tick == 80 {
            w.event(
                "monitor",
                Event::ModelReady {
                    model: sage.clone(),
                    chips: sage_chips.clone(),
                },
            );
        }
        if (90..=link_tick).contains(&tick) && tick % 10 == 0 {
            let count = 3 + (w.wobble() * 5.0) as u32;
            w.event(
                "monitor",
                Event::RequestsCompleted {
                    model: sage.clone(),
                    n: count,
                },
            );
        }
        if tick == 300 {
            w.event(
                "monitor",
                Event::ModelReady {
                    model: scribe.clone(),
                    chips: vec![keys[last].clone()],
                },
            );
        }
        if tick == link_tick {
            w.sample(keys[0].clone(), "toplike.eth.live_mask", Value::U64(0x0030));
            w.event(
                "backend",
                Event::LinkChanged {
                    chip: keys[0].clone(),
                    mask: 0x0030,
                },
            );
        }
    }
    w.out
}

/// The whole fixture file for `chips` chips and `ticks` ticks: the header
/// line, then one line per record, each ending in a newline.
pub fn file_text(chips: u8, ticks: u64) -> String {
    text_of(&records(chips, ticks))
}

/// The whole fixture file for [`records_with_links`].
pub fn file_text_with_links(chips: u8, ticks: u64) -> String {
    text_of(&records_with_links(chips, ticks))
}

/// The header line, then one line per record.
fn text_of(records: &[Record]) -> String {
    let mut text = String::from(HEADER);
    text.push('\n');
    for r in records {
        text.push_str(&r.to_line());
        text.push('\n');
    }
    text
}

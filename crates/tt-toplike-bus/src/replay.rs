//! Replay: read a recorded log back and republish it on a bus.
//!
//! [`Replay::open`] takes a plain segment (`bus-1.jsonl`), a compressed
//! segment (`bus-1.jsonl.gz`) or a directory of segments. A directory is read
//! in sequence order (segments are named after their first sequence number).
//! A line that does not decode is skipped and counted
//! ([`Replay::bad_lines`]), the same rule [`crate::segment::read_segment`]
//! uses. Segments are read one at a time, so a long log is never held in
//! memory whole.
//!
//! [`Replay::play`] republishes the records through [`Bus::publish`], so the
//! replayed bus assigns its own sequence numbers and stamps its own
//! timestamps. The pause between two records is the difference of their
//! recorded `t_mono_ns` divided by the speed.
//!
//! # Holes
//!
//! A jump in sequence numbers between two records that were read (a deleted
//! or unreadable segment) is a [`Hole`], listed by [`Replay::holes`]. `play`
//! publishes a [`crate::Gap`] from source `replay` at each one, so a world
//! built from a damaged log shows the gap.
//!
//! # The play clock
//!
//! [`PlayClock`] is what `play` waits on. It is separate from the bus
//! [`crate::Clock`], which stamps records. [`RealClock`] sleeps for real.
//! [`ManualPlayClock`] only moves when `play` sleeps, and can drive a
//! [`ManualClock`] so the replayed bus stamps the same spacing the log had
//! (divided by speed). Tests use it so no test sleeps.
//!
//! # Marking the bus as a replay
//!
//! Open the replay bus with [`Replay::configure`], which sets
//! [`BusConfig::header_kind`] to [`HeaderKind::Replay`]. [`Bus::header`] then
//! reports `Replay`, and any segments the replay bus writes say so too.

use crate::config::{BusConfig, ManualClock};
use crate::log::Bus;
use crate::record::{Gap, Header, HeaderKind, Payload, Record};
use crate::segment;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What [`Replay::play`] waits on between records.
pub trait PlayClock {
    /// Nanoseconds on a monotonic scale. Only differences matter.
    fn now_ns(&self) -> u64;
    /// Wait for `ns` nanoseconds.
    fn sleep_ns(&self, ns: u64);
}

/// The real clock: `sleep_ns` sleeps the thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealClock;

impl PlayClock for RealClock {
    fn now_ns(&self) -> u64 {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn sleep_ns(&self, ns: u64) {
        std::thread::sleep(Duration::from_nanos(ns));
    }
}

/// A play clock for tests: `sleep_ns` moves time forward and returns at once.
#[derive(Debug, Default)]
pub struct ManualPlayClock {
    now: AtomicU64,
    drives: Option<Arc<ManualClock>>,
}

impl ManualPlayClock {
    /// A clock standing at 0.
    pub fn new() -> ManualPlayClock {
        ManualPlayClock::default()
    }

    /// A clock whose sleeps also advance `bus_clock`, so records published
    /// by the replay get the replayed spacing as their own `t_mono_ns`.
    pub fn driving(bus_clock: Arc<ManualClock>) -> ManualPlayClock {
        ManualPlayClock {
            now: AtomicU64::new(0),
            drives: Some(bus_clock),
        }
    }
}

impl PlayClock for ManualPlayClock {
    fn now_ns(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn sleep_ns(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::SeqCst);
        if let Some(c) = &self.drives {
            c.advance(Duration::from_nanos(ns));
        }
    }
}

/// A recorded log opened for reading.
#[derive(Debug)]
pub struct Replay {
    /// Segment files in sequence order.
    files: Vec<PathBuf>,
    header: Option<Header>,
    bad_lines: u64,
    holes: Vec<Hole>,
}

impl Replay {
    /// Open a segment file (plain or `.gz`) or a directory of segments.
    ///
    /// Fails when the path does not exist or a directory holds no segment.
    /// A directory is never modified. When a segment exists both plain and
    /// compressed (a crash while compressing), the compressed copy is read.
    pub fn open(path: &Path) -> io::Result<Replay> {
        let meta = fs::metadata(path)?;
        let files = if meta.is_dir() {
            list_dir(path)?
        } else {
            vec![path.to_path_buf()]
        };
        let header = match files.first() {
            Some(f) => segment::read_segment_from(f, 0, 1)?.header,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no log segments (bus-<n>.jsonl[.gz]) in {}", path.display()),
                ))
            }
        };
        Ok(Replay {
            files,
            header,
            bad_lines: 0,
            holes: Vec::new(),
        })
    }

    /// The header line of the first segment, if it had a valid one.
    pub fn header(&self) -> Option<&Header> {
        self.header.as_ref()
    }

    /// Lines skipped so far because they did not decode. It grows as
    /// [`Replay::records`] reads each segment.
    pub fn bad_lines(&self) -> u64 {
        self.bad_lines
    }

    /// All records, in order. Each call starts from the beginning and
    /// resets [`Replay::bad_lines`].
    pub fn records(&mut self) -> impl Iterator<Item = Record> + '_ {
        self.items().filter_map(|i| match i {
            Item::Rec(r) => Some(r),
            Item::Hole(_) => None,
        })
    }

    /// Holes found so far, in order. Like [`Replay::bad_lines`] it fills as
    /// [`Replay::records`] is consumed, so read it after the iteration.
    pub fn holes(&self) -> &[Hole] {
        &self.holes
    }

    /// How many holes have been found so far.
    pub fn hole_count(&self) -> usize {
        self.holes.len()
    }

    fn items(&mut self) -> Records<'_> {
        self.bad_lines = 0;
        self.holes.clear();
        Records {
            replay: self,
            next_file: 0,
            current: Vec::new().into_iter(),
            last: None,
            stash: None,
            failure: None,
        }
    }

    /// `cfg` with the header kind set to [`HeaderKind::Replay`]. Open the
    /// bus you replay into with this.
    pub fn configure(mut cfg: BusConfig) -> BusConfig {
        cfg.header_kind = HeaderKind::Replay;
        cfg
    }

    /// Republish every record on `bus` through [`Bus::publish`].
    ///
    /// The pause before a record is its `t_mono_ns` minus the first
    /// record's, divided by `speed`, measured from when play began, so time
    /// spent publishing does not add up as drift. Returns how many records
    /// were published. A `speed` that is not finite and above zero is an
    /// error and nothing is published.
    pub fn play(&mut self, bus: &Bus, speed: f64, clock: &dyn PlayClock) -> io::Result<u64> {
        check_speed(speed)?;
        let start = clock.now_ns();
        let mut first_t = None;
        let mut n = 0;
        for item in self.items() {
            let rec = match item {
                Item::Rec(r) => r,
                Item::Hole(h) => {
                    // Say so on the bus. The pacing rule is unchanged: the
                    // next record waits by its own recorded offset, so the
                    // hole adds no extra sleep.
                    bus.publish(
                        "replay",
                        Payload::Gap(Gap {
                            reason: h.describe(),
                        }),
                    );
                    continue;
                }
            };
            let t0 = *first_t.get_or_insert(rec.t_mono_ns);
            let offset = rec.t_mono_ns.saturating_sub(t0);
            let target = start.saturating_add((offset as f64 / speed) as u64);
            let wait = target.saturating_sub(clock.now_ns());
            if wait > 0 {
                clock.sleep_ns(wait);
            }
            bus.publish(&rec.source, rec.payload);
            n += 1;
        }
        Ok(n)
    }
}

/// Reject a speed that cannot scale time.
pub fn check_speed(speed: f64) -> io::Result<()> {
    if speed.is_finite() && speed > 0.0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("replay speed must be a number above zero, got {speed}"),
        ))
    }
}

/// Segments in `dir`, in sequence order. Compressed wins over plain.
fn list_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found: std::collections::BTreeMap<u64, (bool, PathBuf)> = Default::default();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name();
        let Some((seq, gz)) = name.to_str().and_then(segment::parse_name) else {
            continue;
        };
        match found.get(&seq) {
            Some((true, _)) => {}
            _ => {
                found.insert(seq, (gz, e.path()));
            }
        }
    }
    Ok(found.into_values().map(|(_, p)| p).collect())
}

/// A discontinuity in the records actually read: the log is missing some.
///
/// Only a jump between two records that were read counts. The first record
/// is never a hole, so a log that starts at sequence 500 (older segments
/// removed by retention) has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hole {
    /// The last record read before the hole.
    pub after_seq: u64,
    /// The first record read after it, or `None` when the log ends here
    /// because the last segment could not be read.
    pub next_seq: Option<u64>,
    /// Why: no segment file, or what went wrong reading one.
    pub reason: String,
}

impl Hole {
    /// The text [`Replay::play`] publishes as a [`crate::Gap`] at this hole.
    pub fn describe(&self) -> String {
        match self.next_seq {
            Some(n) => format!(
                "missing records {}..={} ({})",
                self.after_seq + 1,
                n - 1,
                self.reason
            ),
            None => format!(
                "log ends early after record {} ({})",
                self.after_seq, self.reason
            ),
        }
    }
}

/// What the internal iterator yields: a record, or a hole just before the
/// record that follows it.
enum Item {
    Rec(Record),
    Hole(Hole),
}

/// Iterator over the records of a [`Replay`], one segment in memory at a time.
/// It notes every hole in `replay.holes` as it goes.
struct Records<'a> {
    replay: &'a mut Replay,
    next_file: usize,
    current: std::vec::IntoIter<Record>,
    last: Option<u64>,
    /// A record read after a hole, held back until the hole is yielded.
    stash: Option<Record>,
    /// Why the most recent segment gave nothing, to explain the next hole.
    failure: Option<String>,
}

impl Records<'_> {
    fn next_record(&mut self) -> Option<Record> {
        loop {
            if let Some(r) = self.current.next() {
                return Some(r);
            }
            let path = self.replay.files.get(self.next_file)?;
            self.next_file += 1;
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            match segment::read_segment(path) {
                Ok(read) => {
                    self.replay.bad_lines += read.bad_lines;
                    // A file with no header and no record is empty or cut
                    // to nothing. A header-only file is a segment that
                    // simply has no records yet, which is not damage.
                    if read.header.is_none() && read.records.is_empty() && read.bad_lines == 0 {
                        self.failure = Some(format!("segment {name} is empty"));
                    }
                    self.current = read.records.into_iter();
                }
                Err(e) => self.failure = Some(format!("cannot read segment {name}: {e}")),
            }
        }
    }

    fn note(&mut self, hole: Hole) -> Item {
        self.replay.holes.push(hole.clone());
        Item::Hole(hole)
    }
}

impl Iterator for Records<'_> {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        if let Some(r) = self.stash.take() {
            return Some(Item::Rec(r));
        }
        let Some(r) = self.next_record() else {
            // The log ended. A segment that failed with nothing after it
            // is still a hole.
            let reason = self.failure.take()?;
            let after_seq = self.last?;
            return Some(self.note(Hole {
                after_seq,
                next_seq: None,
                reason,
            }));
        };
        let reason = self.failure.take();
        let Some(last) = self.last.replace(r.seq) else {
            return Some(Item::Rec(r));
        };
        if r.seq > last + 1 {
            let hole = Hole {
                after_seq: last,
                next_seq: Some(r.seq),
                reason: reason.unwrap_or_else(|| "no segment file holds them".to_string()),
            };
            self.stash = Some(r);
            return Some(self.note(hole));
        }
        Some(Item::Rec(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Event, Gap, Payload, Provenance, Sample, Value};
    use crate::signals;
    use crate::testutil::TempDir;
    use crate::{EntityKey, World};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bringup.jsonl");
    const WALL0: u64 = 1_790_000_000_000;

    // ---- how tests/fixtures/bringup.jsonl was made ------------------------
    //
    // The fixture is a hand-made bring-up, written by `fixture_text()` below
    // and checked in. Regenerate it with
    //
    //     cargo test -p tt-toplike-bus --lib replay::tests::regenerate_fixture -- --ignored
    //
    // `fixture_matches_generator` fails when the file and the generator
    // disagree, so neither can drift alone. The story, on a monotonic clock
    // that starts at 0 and runs 20 s:
    //
    // * board `BQB-0001` carries two chips, 0000:01:00.0 and 0000:02:00.0.
    //   Every 2 s each chip reports temperature and power, the board its fan.
    // * t = 0.5 s model `llama-8b` is announced as loading on both chips.
    // * t = 3 s both chips are reset (a `tt-smi -r` before the load).
    // * t = 1 s to 6 s the model is `compiling`, progress 0.0 to 1.0.
    // * t = 6 s to 12 s it is `loading`, progress 0.0 to 1.0.
    // * t = 12 s `ModelReady`, phase `ready`.
    // * t = 12 s to 20 s every second: tokens/s, running, waiting, KV cache.
    // * t = 14 s, 16 s, 19 s `RequestsCompleted` of 3, 5 and 2.
    //
    // Every spacing is a multiple of 100 ms, so dividing by 4 stays exact.
    // The first record is at t = 0 and sequence numbers run 1..N with no gap,
    // so a replay at speed 1 reproduces the recorded timestamps and the
    // world built from the replay equals the world built from the file.

    fn chip(n: u8) -> EntityKey {
        EntityKey::chip(&format!("0000:0{n}:00.0"))
    }

    fn model() -> EntityKey {
        EntityKey::model("llama-8b")
    }

    fn sample(e: &EntityKey, signal: &str, value: Value, prov: Provenance) -> Payload {
        Payload::Sample(Sample {
            entity: e.clone(),
            signal: signal.into(),
            value,
            prov,
        })
    }

    /// All fixture records, time-ordered with sequence numbers assigned.
    fn fixture_records() -> Vec<Record> {
        let ms = 1_000_000u64;
        let mut at: Vec<(u64, &str, Payload)> = Vec::new();
        let chips = vec![chip(1), chip(2)];
        let board = EntityKey::board("BQB-0001");
        let f = Value::F64;

        for (k, t) in (0..=10u64).map(|k| (k, k * 2_000)) {
            for (i, c) in chips.iter().enumerate() {
                let load = if t >= 12_000 { 18.0 } else { 0.0 };
                at.push((
                    t * ms,
                    "sysfs",
                    sample(
                        c,
                        "chip.temp_c",
                        f(42.0 + i as f64 + load + k as f64 * 0.5),
                        Provenance::Measured,
                    ),
                ));
                at.push((
                    t * ms,
                    "sysfs",
                    sample(
                        c,
                        "chip.power_w",
                        f(14.0 + 2.0 * i as f64 + load * 2.5),
                        Provenance::Measured,
                    ),
                ));
            }
            at.push((
                t * ms,
                "sysfs",
                sample(
                    &board,
                    "board.fan_rpm",
                    Value::U64(3200),
                    Provenance::Measured,
                ),
            ));
        }

        let m = model();
        at.push((
            500 * ms,
            "monitor",
            Payload::Event(Event::ModelLoading {
                model: m.clone(),
                chips: chips.clone(),
                progress: Some(0.0),
            }),
        ));
        at.push((
            3_000 * ms,
            "tt-smi",
            Payload::Event(Event::Reset {
                chips: chips.clone(),
            }),
        ));
        let label = Value::Text("Llama 3.1 8B".into());
        at.push((
            500 * ms,
            "monitor",
            sample(&m, signals::MODEL_LABEL, label, Provenance::Measured),
        ));
        let phase = |t: u64, p: &str| {
            (
                t * ms,
                "monitor",
                sample(
                    &m,
                    signals::MODEL_PHASE,
                    Value::Text(p.into()),
                    Provenance::Measured,
                ),
            )
        };
        at.push(phase(1_000, "compiling"));
        at.push(phase(6_000, "loading"));
        at.push(phase(12_000, "ready"));
        // Compile progress 1 s..6 s, load progress 6 s..12 s, each in 1 s steps
        // (one decimal place so the file stays readable).
        for s in 0..=5u64 {
            at.push((
                (1_000 + s * 1_000) * ms,
                "monitor",
                sample(
                    &m,
                    signals::MODEL_PROGRESS,
                    f(s as f64 / 5.0),
                    Provenance::Measured,
                ),
            ));
        }
        for s in 0..=6u64 {
            at.push((
                (6_000 + s * 1_000) * ms,
                "monitor",
                sample(
                    &m,
                    signals::MODEL_PROGRESS,
                    f((s as f64 / 6.0 * 100.0).round() / 100.0),
                    Provenance::Measured,
                ),
            ));
        }
        at.push((
            12_000 * ms,
            "monitor",
            Payload::Event(Event::ModelReady {
                model: m.clone(),
                chips: chips.clone(),
            }),
        ));
        for s in 0..=8u64 {
            let t = (12_000 + s * 1_000) * ms;
            let tokens = 38.0 + (s % 3) as f64 * 1.5;
            at.push((
                t,
                "vllm",
                sample(
                    &m,
                    signals::MODEL_TOKENS_PER_S,
                    f(tokens),
                    Provenance::Measured,
                ),
            ));
            at.push((
                t,
                "vllm",
                sample(
                    &m,
                    signals::MODEL_RUNNING,
                    Value::U64(1 + s % 4),
                    Provenance::Measured,
                ),
            ));
            at.push((
                t,
                "vllm",
                sample(
                    &m,
                    signals::MODEL_WAITING,
                    Value::U64(s / 4),
                    Provenance::Measured,
                ),
            ));
            at.push((
                t,
                "vllm",
                sample(
                    &m,
                    signals::MODEL_KV_CACHE,
                    f((5 + s * 4) as f64 / 100.0),
                    Provenance::Measured,
                ),
            ));
        }
        for (t, n) in [(14_000u64, 3u32), (16_000, 5), (19_000, 2)] {
            at.push((
                t * ms,
                "vllm",
                Payload::Event(Event::RequestsCompleted {
                    model: m.clone(),
                    n,
                }),
            ));
        }
        // Stable sort keeps the order above for records at the same instant.
        at.sort_by_key(|(t, _, _)| *t);
        at.into_iter()
            .enumerate()
            .map(|(i, (t, source, payload))| Record {
                seq: i as u64 + 1,
                t_mono_ns: t,
                t_wall_ms: WALL0 + t / ms,
                source: source.into(),
                payload,
            })
            .collect()
    }

    /// The whole fixture file: header line, then one record per line.
    fn fixture_text() -> String {
        let mut s = segment::header_line(HeaderKind::Live);
        s.push('\n');
        for r in fixture_records() {
            s.push_str(&r.to_line());
            s.push('\n');
        }
        s
    }

    #[test]
    #[ignore = "writes tests/fixtures/bringup.jsonl; run on purpose"]
    fn regenerate_fixture() {
        fs::write(FIXTURE, fixture_text()).unwrap();
    }

    #[test]
    fn fixture_matches_generator() {
        let on_disk = fs::read_to_string(FIXTURE).unwrap();
        assert_eq!(on_disk, fixture_text(), "run regenerate_fixture");
        assert!(on_disk.len() < 100_000, "fixture must stay under 100 KB");
    }

    fn fixture_world() -> World {
        let mut w = World::new();
        for r in Replay::open(Path::new(FIXTURE)).unwrap().records() {
            w.apply(&r);
        }
        w
    }

    /// A world bus whose clock the play clock drives.
    fn replay_bus() -> (Bus, ManualPlayClock) {
        let clock = ManualClock::new(WALL0);
        let bus = Bus::open_with_world(Replay::configure(BusConfig {
            clock: clock.clone(),
            ..BusConfig::default()
        }))
        .unwrap();
        (bus, ManualPlayClock::driving(clock))
    }

    fn gaps(bus: &Bus) -> Vec<u64> {
        let recs = bus.read_from(1, usize::MAX).unwrap();
        recs.windows(2)
            .map(|w| w[1].t_mono_ns - w[0].t_mono_ns)
            .collect()
    }

    fn original_gaps() -> Vec<u64> {
        let recs = fixture_records();
        recs.windows(2)
            .map(|w| w[1].t_mono_ns - w[0].t_mono_ns)
            .collect()
    }

    #[test]
    fn speed_one_keeps_the_original_spacing() {
        let (bus, clock) = replay_bus();
        let n = Replay::open(Path::new(FIXTURE))
            .unwrap()
            .play(&bus, 1.0, &clock)
            .unwrap();
        assert_eq!(n, fixture_records().len() as u64);
        assert_eq!(gaps(&bus), original_gaps());
        assert_eq!(clock.now_ns(), 20_000 * 1_000_000);
    }

    #[test]
    fn speed_four_quarters_the_spacing() {
        let (bus, clock) = replay_bus();
        Replay::open(Path::new(FIXTURE))
            .unwrap()
            .play(&bus, 4.0, &clock)
            .unwrap();
        let want: Vec<u64> = original_gaps().iter().map(|g| g / 4).collect();
        assert_eq!(gaps(&bus), want);
        assert_eq!(clock.now_ns(), 5_000 * 1_000_000);
    }

    #[test]
    fn bad_speeds_are_rejected_and_publish_nothing() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let (bus, clock) = replay_bus();
            let err = Replay::open(Path::new(FIXTURE))
                .unwrap()
                .play(&bus, bad, &clock)
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "speed {bad}");
            assert!(err.to_string().contains("above zero"), "{err}");
            assert_eq!(bus.head(), 0);
        }
    }

    #[test]
    fn replayed_world_equals_the_world_built_directly() {
        let (bus, clock) = replay_bus();
        Replay::open(Path::new(FIXTURE))
            .unwrap()
            .play(&bus, 1.0, &clock)
            .unwrap();
        let replayed = bus.snapshot().unwrap().world;
        let direct = fixture_world();
        assert_eq!(replayed, direct);
        // Guard against an empty-equals-empty pass.
        assert!(direct.seq() > 50);
        assert_eq!(direct.models().len(), 1);
    }

    #[test]
    fn a_replay_bus_reports_the_replay_header() {
        let (bus, _) = replay_bus();
        assert_eq!(bus.header().kind, HeaderKind::Replay);
        let live = Bus::open(BusConfig::default()).unwrap();
        assert_eq!(live.header().kind, HeaderKind::Live);
    }

    #[test]
    fn a_replay_bus_writes_the_replay_header_to_disk() {
        let dir = TempDir::new("replay-hdr");
        let bus = Bus::open(Replay::configure(BusConfig {
            dir: Some(dir.path().to_path_buf()),
            ..BusConfig::default()
        }))
        .unwrap();
        bus.publish("t", Payload::Gap(Gap { reason: "x".into() }));
        assert!(bus.flush(Duration::from_secs(5)));
        let replay = Replay::open(dir.path()).unwrap();
        assert_eq!(replay.header().unwrap().kind, HeaderKind::Replay);
    }

    #[test]
    fn the_fixture_header_and_records_read_back() {
        let mut r = Replay::open(Path::new(FIXTURE)).unwrap();
        assert_eq!(r.header().unwrap().kind, HeaderKind::Live);
        let got: Vec<Record> = r.records().collect();
        assert_eq!(got, fixture_records());
        assert_eq!(r.bad_lines(), 0);
    }

    #[test]
    fn a_truncated_last_line_is_skipped_and_counted() {
        let dir = TempDir::new("replay-trunc");
        fs::create_dir_all(dir.path()).unwrap();
        let mut text = fixture_text();
        // Cut the last record in half and drop its newline.
        text.pop();
        let cut = text.rfind('\n').unwrap() + 1;
        let last_len = text.len() - cut;
        text.truncate(cut + last_len / 2);
        let p = dir.path().join("bus-1.jsonl");
        fs::write(&p, text).unwrap();
        let mut r = Replay::open(&p).unwrap();
        let got: Vec<Record> = r.records().collect();
        let all = fixture_records();
        assert_eq!(got, all[..all.len() - 1]);
        assert_eq!(r.bad_lines(), 1);
    }

    #[test]
    fn a_gzip_segment_reads_the_same_as_the_plain_one() {
        let dir = TempDir::new("replay-gz");
        fs::create_dir_all(dir.path()).unwrap();
        let p = dir.path().join("bus-1.jsonl.gz");
        let mut enc = GzEncoder::new(fs::File::create(&p).unwrap(), Compression::default());
        enc.write_all(fixture_text().as_bytes()).unwrap();
        enc.finish().unwrap();
        let mut r = Replay::open(&p).unwrap();
        assert_eq!(r.records().collect::<Vec<_>>(), fixture_records());
        assert_eq!(r.header().unwrap().kind, HeaderKind::Live);
    }

    #[test]
    fn a_directory_replays_in_sequence_order_and_prefers_gzip() {
        let dir = TempDir::new("replay-dir");
        fs::create_dir_all(dir.path()).unwrap();
        let all = fixture_records();
        let (a, b) = all.split_at(10);
        let body = |recs: &[Record]| {
            let mut s = segment::header_line(HeaderKind::Live);
            s.push('\n');
            for r in recs {
                s.push_str(&r.to_line());
                s.push('\n');
            }
            s
        };
        // The later segment is plain and named first on disk listing order is
        // irrelevant: numbers decide. 100 > 11 would sort wrong as text.
        fs::write(dir.path().join("bus-11.jsonl"), body(b)).unwrap();
        let mut enc = GzEncoder::new(
            fs::File::create(dir.path().join("bus-1.jsonl.gz")).unwrap(),
            Compression::default(),
        );
        enc.write_all(body(a).as_bytes()).unwrap();
        enc.finish().unwrap();
        // A stale plain copy of segment 1 with a record missing must lose.
        fs::write(dir.path().join("bus-1.jsonl"), body(&a[..3])).unwrap();
        fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().collect::<Vec<_>>(), all);
    }

    #[test]
    fn a_missing_path_or_empty_directory_is_an_error() {
        let dir = TempDir::new("replay-none");
        assert!(Replay::open(&dir.path().join("nope")).is_err());
        fs::create_dir_all(dir.path()).unwrap();
        let e = Replay::open(dir.path()).unwrap_err();
        assert!(e.to_string().contains("no log segments"), "{e}");
    }

    // ---- holes ------------------------------------------------------------

    /// Fixture records 1..=114 in three plain segments: bus-1 holds 1..=40,
    /// bus-41 holds 41..=80, bus-81 holds 81..=114.
    fn three_segments(tag: &str) -> TempDir {
        let dir = TempDir::new(tag);
        fs::create_dir_all(dir.path()).unwrap();
        let all = fixture_records();
        for (first, lo, hi) in [(1, 0, 40), (41, 40, 80), (81, 80, all.len())] {
            let mut s = segment::header_line(HeaderKind::Live);
            s.push('\n');
            for r in &all[lo..hi] {
                s.push_str(&r.to_line());
                s.push('\n');
            }
            fs::write(dir.path().join(format!("bus-{first}.jsonl")), s).unwrap();
        }
        dir
    }

    #[test]
    fn an_intact_directory_has_no_holes() {
        let dir = three_segments("holes-none");
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 114);
        assert!(r.holes().is_empty());
        assert_eq!(r.hole_count(), 0);
    }

    #[test]
    fn a_log_that_starts_late_has_no_hole() {
        let dir = TempDir::new("holes-late");
        fs::create_dir_all(dir.path()).unwrap();
        let mut s = segment::header_line(HeaderKind::Live);
        s.push('\n');
        for r in &fixture_records()[..10] {
            let mut r = r.clone();
            r.seq += 499;
            s.push_str(&r.to_line());
            s.push('\n');
        }
        fs::write(dir.path().join("bus-500.jsonl"), s).unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 10);
        assert_eq!(r.hole_count(), 0);
    }

    #[test]
    fn a_missing_middle_segment_is_one_hole_with_the_right_bounds() {
        let dir = three_segments("holes-mid");
        fs::remove_file(dir.path().join("bus-41.jsonl")).unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 74);
        assert_eq!(r.hole_count(), 1);
        let h = &r.holes()[0];
        assert_eq!((h.after_seq, h.next_seq), (40, Some(81)));
        assert!(h.reason.contains("no segment"), "{}", h.reason);
    }

    #[test]
    fn play_publishes_a_gap_at_the_hole() {
        let dir = three_segments("holes-play");
        fs::remove_file(dir.path().join("bus-41.jsonl")).unwrap();
        let (bus, clock) = replay_bus();
        let n = Replay::open(dir.path())
            .unwrap()
            .play(&bus, 1.0, &clock)
            .unwrap();
        assert_eq!(n, 74);
        let recs = bus.read_from(1, usize::MAX).unwrap();
        // 74 records plus the one gap, in the place of the missing ones.
        assert_eq!(recs.len(), 75);
        let at = recs
            .iter()
            .position(|r| matches!(r.payload, Payload::Gap(_)))
            .unwrap();
        assert_eq!(at, 40);
        assert_eq!(recs[at].source, "replay");
        let Payload::Gap(g) = &recs[at].payload else {
            unreachable!()
        };
        assert!(g.reason.contains("missing records 41..=80"), "{}", g.reason);
        assert_eq!(
            recs.iter()
                .filter(|r| matches!(r.payload, Payload::Gap(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_segment_truncated_to_empty_is_a_hole_with_a_reason() {
        let dir = three_segments("holes-empty");
        fs::write(dir.path().join("bus-41.jsonl"), "").unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 74);
        assert_eq!(r.hole_count(), 1);
        let h = &r.holes()[0];
        assert_eq!((h.after_seq, h.next_seq), (40, Some(81)));
        assert!(h.reason.contains("bus-41.jsonl"), "{}", h.reason);
        assert!(h.reason.contains("empty"), "{}", h.reason);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_segment_is_a_hole_with_the_os_reason() {
        use std::os::unix::fs::PermissionsExt;
        let dir = three_segments("holes-perm");
        let p = dir.path().join("bus-41.jsonl");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&p).is_ok() {
            return; // running as root: permissions do not apply
        }
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 74);
        assert_eq!(r.hole_count(), 1);
        let h = &r.holes()[0];
        assert_eq!((h.after_seq, h.next_seq), (40, Some(81)));
        assert!(h.reason.contains("bus-41.jsonl"), "{}", h.reason);
        assert!(
            h.reason.to_lowercase().contains("permission"),
            "{}",
            h.reason
        );
        assert_eq!(
            r.bad_lines(),
            0,
            "an unreadable segment is a hole, not a bad line"
        );
    }

    #[test]
    fn an_unreadable_last_segment_is_a_hole_with_no_next_record() {
        let dir = three_segments("holes-tail");
        fs::write(dir.path().join("bus-81.jsonl"), "").unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        assert_eq!(r.records().count(), 80);
        let h = &r.holes()[0];
        assert_eq!((h.after_seq, h.next_seq), (80, None));
    }

    #[test]
    fn holes_reset_with_each_records_call() {
        let dir = three_segments("holes-reset");
        fs::remove_file(dir.path().join("bus-41.jsonl")).unwrap();
        let mut r = Replay::open(dir.path()).unwrap();
        r.records().count();
        r.records().count();
        assert_eq!(r.hole_count(), 1);
    }
}

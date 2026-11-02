//! Tests for the gozer source and governor. Recorded JSON only: gozer is
//! never run and no chip is touched.

use super::*;
use crate::backend::mock::MockBackend;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A real `gozer status --json` (board serials, who, reason and container
/// ids replaced by placeholders). Four chips, all `HELD-FOREIGN` when
/// recorded.
const RECORDED: &str = include_str!("fixtures/gozer_status_recorded.json");

const A: &str = "0000:01:00.0";
const B: &str = "0000:02:00.0";
const OWNER: u64 = 1017106;

/// The recorded fixture with each chip's state, who and pid set. Starts from
/// the recorded JSON so the shape stays the real one.
fn status_json(edits: &[(&str, &str, Option<&str>, Option<u64>)]) -> String {
    let mut v: serde_json::Value = serde_json::from_str(RECORDED).unwrap();
    for c in v["chips"].as_array_mut().unwrap() {
        let bdf = c["bdf"].as_str().unwrap().to_string();
        // Default: every chip FREE with no lease.
        c["state"] = "FREE".into();
        c["who"] = serde_json::Value::Null;
        c["pid"] = serde_json::Value::Null;
        for (b, state, who, pid) in edits {
            if *b == bdf {
                c["state"] = (*state).into();
                c["who"] = who.map_or(serde_json::Value::Null, |w| w.into());
                c["pid"] = pid.map_or(serde_json::Value::Null, |p| p.into());
            }
        }
    }
    v.to_string()
}

fn status(edits: &[(&str, &str, Option<&str>, Option<u64>)]) -> GozerStatus {
    parse_status(&status_json(edits)).unwrap()
}

fn ctx_poll(src: &mut GozerSource, now: Instant) -> Vec<Payload> {
    let be = MockBackend::new(1);
    let ctx = PollCtx { backend: &be };
    let mut v = Vec::new();
    src.poll(&ctx, now, &mut |p| v.push(p));
    v
}

fn events(v: &[Payload]) -> Vec<&Event> {
    v.iter()
        .filter_map(|p| match p {
            Payload::Event(e) => Some(e),
            _ => None,
        })
        .collect()
}

fn texts<'a>(v: &'a [Payload], signal: &str) -> Vec<(&'a EntityKey, &'a str)> {
    v.iter()
        .filter_map(|p| match p {
            Payload::Sample(s) if s.signal == signal => match &s.value {
                Value::Text(t) => Some((&s.entity, t.as_str())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn held(who: &str) -> Vec<(&'static str, &'static str, Option<&str>, Option<u64>)> {
    vec![
        (A, "HELD", Some(who), Some(OWNER)),
        (B, "HELD", Some(who), Some(OWNER)),
    ]
}

// ---- parsing ----

#[test]
fn recorded_fixture_parses() {
    let s = parse_status(RECORDED).unwrap();
    assert_eq!(s.chips.len(), 4);
    assert_eq!(s.chips[0].bdf, A);
    assert_eq!(s.chips[0].state, "HELD-FOREIGN");
    assert_eq!(s.chips[0].dev_index, Some(0));
    assert_eq!(s.chips[0].pid, Some(OWNER));
    assert!(!RECORDED.contains("0000046131924"), "serials replaced");
}

#[test]
fn unparsable_status_is_none() {
    assert!(parse_status("not json").is_none());
    assert!(parse_status("{}").is_none());
    assert!(parse_status(r#"{"chips":[{"bdf":"x"}]}"#).is_none());
}

// ---- source ----

#[test]
fn first_snapshot_emits_state_and_no_grant() {
    let mut src = GozerSource::fed();
    src.feed(status(&held("alice:job")));
    let out = ctx_poll(&mut src, Instant::now());
    assert!(events(&out).is_empty(), "no grant on first sight: {out:?}");
    let st = texts(&out, sig::GOZER_STATE);
    assert_eq!(st.len(), 4);
    assert!(st
        .iter()
        .any(|(e, t)| **e == EntityKey::chip(A) && *t == "HELD"));
    assert!(st.iter().any(|(_, t)| *t == "FREE"));
}

#[test]
fn free_then_held_grants_once() {
    let mut src = GozerSource::fed();
    src.feed(status(&[]));
    ctx_poll(&mut src, Instant::now());
    src.feed(status(&held("alice:job")));
    let out = ctx_poll(&mut src, Instant::now());
    let ev = events(&out);
    assert_eq!(ev.len(), 1, "{ev:?}");
    match ev[0] {
        Event::LeaseGranted { lease, chips } => {
            assert_eq!(*lease, EntityKey::lease(&format!("pid-{OWNER}")));
            assert_eq!(chips, &vec![EntityKey::chip(A), EntityKey::chip(B)]);
        }
        e => panic!("{e:?}"),
    }
    // The same snapshot again changes nothing.
    src.feed(status(&held("alice:job")));
    assert!(events(&ctx_poll(&mut src, Instant::now())).is_empty());
}

#[test]
fn held_then_free_releases_resets_and_gaps() {
    let mut src = GozerSource::fed();
    src.feed(status(&held("alice:job")));
    ctx_poll(&mut src, Instant::now());
    src.feed(status(&[]));
    let out = ctx_poll(&mut src, Instant::now());
    let ev = events(&out);
    assert_eq!(ev.len(), 2, "{ev:?}");
    let chips = vec![EntityKey::chip(A), EntityKey::chip(B)];
    assert!(matches!(ev[0], Event::LeaseReleased { chips: c, .. } if *c == chips));
    assert!(matches!(ev[1], Event::Reset { chips: c } if *c == chips));
    let gaps = out.iter().filter(|p| matches!(p, Payload::Gap(_))).count();
    assert_eq!(gaps, 1);
    // Release comes before the gap marker.
    let ri = out
        .iter()
        .position(|p| matches!(p, Payload::Event(Event::Reset { .. })))
        .unwrap();
    let gi = out
        .iter()
        .position(|p| matches!(p, Payload::Gap(_)))
        .unwrap();
    assert!(ri < gi);
}

#[test]
fn state_change_emits_one_state_sample() {
    let mut src = GozerSource::fed();
    src.feed(status(&[(A, "CLAIMED", Some("w"), Some(OWNER))]));
    ctx_poll(&mut src, Instant::now());
    src.feed(status(&[(A, "HELD", Some("w"), Some(OWNER))]));
    let out = ctx_poll(&mut src, Instant::now());
    let st = texts(&out, sig::GOZER_STATE);
    assert_eq!(st.len(), 1, "{st:?}");
    assert_eq!((st[0].0, st[0].1), (&EntityKey::chip(A), "HELD"));
    assert!(texts(&out, sig::GOZER_WHO).is_empty(), "who did not change");
    assert!(events(&out).is_empty(), "same holder, no lease event");
}

#[test]
fn who_is_redacted_clipped_and_cleared() {
    let mut src = GozerSource::fed();
    let long = format!("HF_TOKEN=hf_secret123 {}", "x".repeat(300));
    src.feed(status(&[(A, "HELD", Some(&long), Some(OWNER))]));
    let out = ctx_poll(&mut src, Instant::now());
    let who = texts(&out, sig::GOZER_WHO);
    assert_eq!(who.len(), 1);
    assert!(!who[0].1.contains("hf_secret123"), "{}", who[0].1);
    assert!(who[0].1.contains("<redacted>"));
    assert_eq!(who[0].1.chars().count(), MAX_WHO);
    src.feed(status(&[]));
    let out = ctx_poll(&mut src, Instant::now());
    let who = texts(&out, sig::GOZER_WHO);
    assert_eq!(who.len(), 1);
    assert_eq!(who[0].1, "");
}

/// Runner whose answer and delay the test controls, and that counts calls.
struct Stub {
    answer: Option<String>,
    delay: Duration,
    calls: Arc<AtomicUsize>,
}

impl StatusRunner for Stub {
    fn status_json(&self, _t: Duration) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.answer.clone()
    }
}

fn stub(answer: Option<String>, delay_ms: u64) -> (Arc<Stub>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Stub {
            answer,
            delay: Duration::from_millis(delay_ms),
            calls: calls.clone(),
        }),
        calls,
    )
}

/// Poll until the worker's answer is processed (or give up).
fn drain(src: &mut GozerSource, start: Instant) -> Vec<Payload> {
    let mut all = Vec::new();
    for i in 0..200 {
        all.extend(ctx_poll(src, start + Duration::from_millis(i)));
        if !all.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    all
}

#[test]
fn absent_gozer_emits_nothing_and_does_not_block() {
    let (r, calls) = stub(None, 0);
    let mut src = GozerSource::new(r);
    let t = Instant::now();
    let a = ctx_poll(&mut src, t);
    std::thread::sleep(Duration::from_millis(50));
    let b = ctx_poll(&mut src, t + Duration::from_secs(3));
    assert!(a.is_empty() && b.is_empty());
    assert!(calls.load(Ordering::SeqCst) >= 1);
}

#[test]
fn runner_that_sleeps_never_blocks_poll() {
    let (r, _) = stub(Some(status_json(&[])), 600);
    let mut src = GozerSource::new(r);
    let t0 = Instant::now();
    for i in 0..5 {
        ctx_poll(&mut src, t0 + Duration::from_millis(i));
    }
    assert!(
        t0.elapsed() < Duration::from_millis(200),
        "poll blocked: {:?}",
        t0.elapsed()
    );
}

#[test]
fn worker_answer_is_turned_into_samples() {
    let (r, _) = stub(Some(status_json(&held("alice:job"))), 20);
    let mut src = GozerSource::new(r);
    let out = drain(&mut src, Instant::now());
    assert_eq!(texts(&out, sig::GOZER_STATE).len(), 4);
}

#[test]
fn runner_is_started_at_most_once_per_interval() {
    let (r, calls) = stub(Some(status_json(&[])), 0);
    let mut src = GozerSource::new(r);
    let t = Instant::now();
    for i in 0..10 {
        ctx_poll(&mut src, t + Duration::from_millis(100 * i));
        std::thread::sleep(Duration::from_millis(5));
    }
    // 1 s of fake time: one start. Then a poll 2 s later starts another.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx_poll(&mut src, t + POLL_INTERVAL + Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_poll_keeps_the_last_snapshot() {
    let mut src = GozerSource::fed();
    src.feed(status(&held("w")));
    ctx_poll(&mut src, Instant::now());
    // Nothing fed: nothing emitted, and no release is invented.
    assert!(ctx_poll(&mut src, Instant::now()).is_empty());
    src.feed(status(&held("w")));
    assert!(events(&ctx_poll(&mut src, Instant::now())).is_empty());
}

// ---- governor ----

struct Counting {
    answer: Arc<Mutex<Option<String>>>,
    calls: Arc<AtomicUsize>,
}

impl StatusRunner for Counting {
    fn status_json(&self, _t: Duration) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.lock().unwrap().clone()
    }
}

fn gov(
    lease: Option<&str>,
    visible: Option<&str>,
    json: Option<String>,
) -> (GozerGovernor, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let g = GozerGovernor::new(
        lease.map(str::to_string),
        visible.map(str::to_string),
        Box::new(Counting {
            answer: Arc::new(Mutex::new(json)),
            calls: calls.clone(),
        }),
    )
    .with_owner_lookup(Box::new(|id| (id == "L1").then_some(OWNER)));
    (g, calls)
}

fn denied(p: &Permit) -> bool {
    matches!(p, Permit::Deny(_))
}

fn one(state: &str, pid: Option<u64>) -> Option<String> {
    Some(status_json(&[(A, state, Some("w"), pid)]))
}

#[test]
fn governor_permits_only_our_held_chip() {
    let (g, _) = gov(Some("L1"), Some("0,1"), one("HELD", Some(OWNER)));
    assert_eq!(g.permit(&EntityKey::chip(A)), Permit::Go);
}

#[test]
fn governor_denies_without_lease_variable() {
    let (g, _) = gov(None, Some("0,1"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    let (g, _) = gov(Some(""), Some("0,1"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_denies_without_visible_devices() {
    let (g, _) = gov(Some("L1"), None, one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    let (g, _) = gov(Some("L1"), Some("zero,one"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    let (g, _) = gov(Some("L1"), Some(""), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_denies_every_state_but_held() {
    for state in [
        "HELD-FOREIGN",
        "BUSY-UNTRACKED",
        "FREE",
        "CLAIMED",
        "STALE",
        "DEGRADED",
        "UNREADABLE",
    ] {
        let (g, _) = gov(Some("L1"), Some("0,1"), one(state, Some(OWNER)));
        assert!(denied(&g.permit(&EntityKey::chip(A))), "{state}");
    }
}

#[test]
fn governor_denies_foreign_held_state_by_name() {
    let (g, _) = gov(Some("L1"), Some("0,1"), one("HELD-FOREIGN", Some(OWNER)));
    assert_eq!(
        g.permit(&EntityKey::chip(A)),
        Permit::Deny("chip state is HELD-FOREIGN, not HELD".into())
    );
}

#[test]
fn governor_denies_held_by_another_lease() {
    let (g, _) = gov(Some("L1"), Some("0,1"), one("HELD", Some(OWNER + 1)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_denies_when_our_lease_record_is_unreadable() {
    let (g, _) = gov(Some("L2"), Some("0,1"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_denies_chip_not_in_visible_devices() {
    // Chip A has dev_index 0.
    let (g, _) = gov(Some("L1"), Some("1,2"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_denies_unknown_chip_and_bad_status() {
    let (g, _) = gov(Some("L1"), Some("0"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip("0000:99:00.0"))));
    let (g, _) = gov(Some("L1"), Some("0"), Some("garbage".into()));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    let (g, _) = gov(Some("L1"), Some("0"), None);
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_prefers_a_lease_id_in_status() {
    let mut v: serde_json::Value =
        serde_json::from_str(&status_json(&[(A, "HELD", Some("w"), Some(1))])).unwrap();
    v["chips"][0]["lease_id"] = "L9".into();
    let (g, _) = gov(Some("L9"), Some("0"), Some(v.to_string()));
    assert_eq!(g.permit(&EntityKey::chip(A)), Permit::Go);
    let (g, _) = gov(Some("L1"), Some("0"), Some(v.to_string()));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_caches_for_30s_and_denies_when_stale() {
    let (g, calls) = gov(Some("L1"), Some("0"), one("HELD", Some(OWNER)));
    let chip = EntityKey::chip(A);
    let t = Instant::now();
    assert_eq!(g.permit_at(&chip, t), Permit::Go);
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(29)), Permit::Go);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cached within 30 s");
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(30)), Permit::Go);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "refreshed after 30 s");
}

#[test]
fn governor_failed_refresh_uses_cache_until_60s_then_denies() {
    let calls = Arc::new(AtomicUsize::new(0));
    let answer = Arc::new(Mutex::new(one("HELD", Some(OWNER))));
    let runner = Counting {
        answer: answer.clone(),
        calls: calls.clone(),
    };
    let g = GozerGovernor::new(Some("L1".into()), Some("0".into()), Box::new(runner))
        .with_owner_lookup(Box::new(|_| Some(OWNER)));
    let chip = EntityKey::chip(A);
    let t = Instant::now();
    assert_eq!(g.permit_at(&chip, t), Permit::Go);
    *answer.lock().unwrap() = None; // gozer goes away
                                    // 45 s: refresh fails, 45 s old cache still fine.
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(45)), Permit::Go);
    // 76 s: the refresh fails again (31 s since the 45 s attempt) and the
    // cache is 76 s old, past 60 s.
    assert!(denied(&g.permit_at(&chip, t + Duration::from_secs(76))));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

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
    // Scrub check by pattern: every board, who, reason and container value
    // is a placeholder, so nothing real is in the checked-in fixture.
    let v: serde_json::Value = serde_json::from_str(RECORDED).unwrap();
    for c in v["chips"].as_array().unwrap() {
        let board = c["board"].as_str().unwrap();
        assert!(
            board.starts_with("BOARD-SERIAL-") && board[13..].chars().all(|d| d.is_ascii_digit()),
            "board {board}"
        );
        assert_eq!(c["who"], "WHO-PLACEHOLDER");
        assert_eq!(c["reason"], "REASON-PLACEHOLDER");
        for (_, name) in c["holder_containers"].as_object().unwrap() {
            assert_eq!(name, "CONTAINER-PLACEHOLDER");
        }
    }
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
    fn run(&self, _args: &[&str], _t: Duration) -> Option<String> {
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
    status: Arc<Mutex<Option<String>>>,
    env: Arc<Mutex<Option<String>>>,
    /// Calls of `gozer status`.
    calls: Arc<AtomicUsize>,
    /// Calls of `gozer env`.
    env_calls: Arc<AtomicUsize>,
}

impl StatusRunner for Counting {
    fn run(&self, args: &[&str], _t: Duration) -> Option<String> {
        match args {
            ["status", "--json"] => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.status.lock().unwrap().clone()
            }
            ["env", _lease, "--json"] => {
                self.env_calls.fetch_add(1, Ordering::SeqCst);
                self.env.lock().unwrap().clone()
            }
            other => panic!("unexpected gozer command {other:?}"),
        }
    }
}

/// `gozer env <lease> --json` output for the given chips.
fn env_json(bdfs: &[&str]) -> Option<String> {
    Some(format!(r#"{{"TT_VISIBLE_DEVICES": "{}"}}"#, bdfs.join(",")))
}

struct Handles {
    status: Arc<Mutex<Option<String>>>,
    env: Arc<Mutex<Option<String>>>,
    calls: Arc<AtomicUsize>,
    env_calls: Arc<AtomicUsize>,
}

fn gov_full(
    lease: Option<&str>,
    visible: Option<&str>,
    json: Option<String>,
) -> (GozerGovernor, Handles) {
    let h = Handles {
        status: Arc::new(Mutex::new(json)),
        env: Arc::new(Mutex::new(env_json(&[A, B]))),
        calls: Arc::new(AtomicUsize::new(0)),
        env_calls: Arc::new(AtomicUsize::new(0)),
    };
    let g = GozerGovernor::new(
        lease.map(str::to_string),
        visible.map(str::to_string),
        Box::new(Counting {
            status: h.status.clone(),
            env: h.env.clone(),
            calls: h.calls.clone(),
            env_calls: h.env_calls.clone(),
        }),
    );
    (g, h)
}

/// Our lease (`L1`) owns chips A and B; visible devices default to numbers.
fn gov(
    lease: Option<&str>,
    visible: Option<&str>,
    json: Option<String>,
) -> (GozerGovernor, Arc<AtomicUsize>) {
    let (g, h) = gov_full(lease, visible, json);
    (g, h.calls)
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
    let (g, _) = gov(Some("L1"), Some("0,,1"), one("HELD", Some(OWNER)));
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
fn governor_denies_chip_not_in_our_lease() {
    // Chip A is HELD, but `gozer env` for our lease lists only chip B.
    let (g, h) = gov_full(Some("L1"), Some("0,1"), one("HELD", Some(OWNER)));
    *h.env.lock().unwrap() = env_json(&[B]);
    assert_eq!(
        g.permit(&EntityKey::chip(A)),
        Permit::Deny("chip is not in our lease".into())
    );
}

#[test]
fn governor_denies_when_gozer_env_fails() {
    // A lease gozer does not know exits nonzero: the runner gives None.
    let (g, h) = gov_full(Some("L2"), Some("0,1"), one("HELD", Some(OWNER)));
    *h.env.lock().unwrap() = None;
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    // Unparsable env output also denies.
    *h.env.lock().unwrap() = Some("garbage".into());
    let g2 = gov_full(Some("L2"), Some("0,1"), one("HELD", Some(OWNER)));
    *g2.1.env.lock().unwrap() = Some("{}".into());
    assert!(denied(&g2.0.permit(&EntityKey::chip(A))));
}

#[test]
fn governor_does_not_depend_on_owner_pid() {
    // Detached lease: pid null, HELD because something has the chip open.
    let (g, _) = gov(Some("L1"), Some("0,1"), one("HELD", None));
    assert_eq!(g.permit(&EntityKey::chip(A)), Permit::Go);
    // Same pid on a chip our lease does not list is still denied.
    let (g, h) = gov_full(Some("L1"), Some("0,1"), one("HELD", Some(OWNER)));
    *h.env.lock().unwrap() = env_json(&[]);
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
fn governor_caches_for_30s_and_denies_when_stale() {
    let (g, h) = gov_full(Some("L1"), Some("0"), one("HELD", Some(OWNER)));
    let chip = EntityKey::chip(A);
    let t = Instant::now();
    assert_eq!(g.permit_at(&chip, t), Permit::Go);
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(29)), Permit::Go);
    assert_eq!(h.calls.load(Ordering::SeqCst), 1, "cached within 30 s");
    assert_eq!(h.env_calls.load(Ordering::SeqCst), 1, "env cached too");
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(30)), Permit::Go);
    assert_eq!(h.calls.load(Ordering::SeqCst), 2, "refreshed after 30 s");
    assert_eq!(h.env_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn governor_failed_refresh_uses_cache_until_60s_then_denies() {
    let (g, h) = gov_full(Some("L1"), Some("0"), one("HELD", Some(OWNER)));
    let chip = EntityKey::chip(A);
    let t = Instant::now();
    assert_eq!(g.permit_at(&chip, t), Permit::Go);
    *h.status.lock().unwrap() = None; // gozer goes away
                                      // 45 s: refresh fails, the 45 s old snapshot still counts.
    assert_eq!(g.permit_at(&chip, t + Duration::from_secs(45)), Permit::Go);
    // 76 s: refresh fails again, snapshot is 76 s old, past 60 s.
    assert!(denied(&g.permit_at(&chip, t + Duration::from_secs(76))));
    assert_eq!(h.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn governor_does_not_hold_the_cache_lock_while_running_a_command() {
    // A runner that calls back into the governor's cache would deadlock if
    // the lock were held. Here: a slow runner on one thread must not stop a
    // second permit (served from cache or by its own attempt) from returning.
    struct Slow(Arc<AtomicUsize>);
    impl StatusRunner for Slow {
        fn run(&self, _a: &[&str], _t: Duration) -> Option<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
            None
        }
    }
    let n = Arc::new(AtomicUsize::new(0));
    let g = Arc::new(GozerGovernor::new(
        Some("L1".into()),
        Some("0".into()),
        Box::new(Slow(n.clone())),
    ));
    let g2 = g.clone();
    let t = Instant::now();
    let h = std::thread::spawn(move || g2.permit_at(&EntityKey::chip(A), t));
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    assert!(denied(&g.permit_at(&EntityKey::chip(A), t)));
    assert!(
        t0.elapsed() < Duration::from_millis(300),
        "{:?}",
        t0.elapsed()
    );
    assert!(denied(&h.join().unwrap()));
}

// ---- fix round 1 ----

#[test]
fn governor_accepts_gozer_format_bdf_visible_devices() {
    let (g, _) = gov(
        Some("L1"),
        Some("0000:01:00.0,0000:02:00.0"),
        one("HELD", Some(OWNER)),
    );
    assert_eq!(g.permit(&EntityKey::chip(A)), Permit::Go);
    // Whitespace and a trailing comma are tolerated.
    let (g, _) = gov(
        Some("L1"),
        Some(" 0000:01:00.0 , 0000:02:00.0, "),
        one("HELD", Some(OWNER)),
    );
    assert_eq!(g.permit(&EntityKey::chip(A)), Permit::Go);
    // A chip whose BDF is not listed is denied.
    let (g, _) = gov(Some("L1"), Some("0000:02:00.0"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    // An entry that is neither a BDF nor a number denies the whole list.
    let (g, _) = gov(
        Some("L1"),
        Some("0000:01:00.0,GPU0"),
        one("HELD", Some(OWNER)),
    );
    assert!(denied(&g.permit(&EntityKey::chip(A))));
    // Numbers still work as dev_index (A is index 0), but a range does not.
    let (g, _) = gov(Some("L1"), Some("0-1"), one("HELD", Some(OWNER)));
    assert!(denied(&g.permit(&EntityKey::chip(A))));
}

#[test]
fn detached_lease_grants_and_releases() {
    // A bare `gozer acquire` records pid null; the who string identifies it.
    let detached = |state: &'static str| {
        vec![
            (A, state, Some("alice:job"), None),
            (B, state, Some("alice:job"), None),
        ]
    };
    let mut src = GozerSource::fed();
    src.feed(status(&[]));
    ctx_poll(&mut src, Instant::now());
    src.feed(status(&detached("CLAIMED")));
    let out = ctx_poll(&mut src, Instant::now());
    assert!(
        matches!(events(&out)[..], [Event::LeaseGranted { .. }]),
        "{out:?}"
    );
    src.feed(status(&[]));
    let out = ctx_poll(&mut src, Instant::now());
    let ev = events(&out);
    assert!(matches!(
        ev[..],
        [Event::LeaseReleased { .. }, Event::Reset { .. }]
    ));
    assert_eq!(
        out.iter().filter(|p| matches!(p, Payload::Gap(_))).count(),
        1
    );
}

#[test]
fn unreadable_flapping_emits_no_release() {
    let mut src = GozerSource::fed();
    src.feed(status(&held("w")));
    ctx_poll(&mut src, Instant::now());
    for _ in 0..3 {
        // gozer reports pid null for an UNREADABLE chip.
        src.feed(status(&[
            (A, "UNREADABLE", None, None),
            (B, "HELD", Some("w"), Some(OWNER)),
        ]));
        let out = ctx_poll(&mut src, Instant::now());
        assert!(events(&out).is_empty() && !out.iter().any(|p| matches!(p, Payload::Gap(_))));
        src.feed(status(&held("w")));
        let out = ctx_poll(&mut src, Instant::now());
        assert!(events(&out).is_empty(), "no grant spam either: {out:?}");
    }
    // A real release still works afterwards.
    src.feed(status(&[]));
    assert_eq!(events(&ctx_poll(&mut src, Instant::now())).len(), 2);
}

#[test]
fn deny_reason_does_not_echo_unknown_state() {
    let (g, _) = gov(Some("L1"), Some("0"), one("sk-secret-state", Some(OWNER)));
    assert_eq!(
        g.permit(&EntityKey::chip(A)),
        Permit::Deny("chip state is other, not HELD".into())
    );
}

// ---- the real runner, against scripts ----

mod command_runner {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable script into its own directory and return the
    /// runner for it. The TempDir must outlive the run.
    fn script(body: &str) -> (tempfile::TempDir, CommandRunner) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-gozer");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = --probe ] && exit 0\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test thread can fork while the file was open for writing,
        // and exec then fails with "text file busy". Probe until it does not.
        for _ in 0..200 {
            match std::process::Command::new(&path).arg("--probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
        let r = CommandRunner::with_program(&path);
        (dir, r)
    }

    const DEADLINE: Duration = Duration::from_millis(600);

    #[test]
    fn prints_json_and_exits() {
        let (_d, r) = script(r#"echo '{"chips": []}'"#);
        let out = r.status_json(Duration::from_secs(5)).unwrap();
        assert!(parse_status(&out).is_some());
    }

    #[test]
    fn passes_the_arguments_through() {
        let (_d, r) = script(r#"echo "$@""#);
        assert_eq!(
            r.status_json(Duration::from_secs(5)).unwrap().trim(),
            "status --json"
        );
        assert_eq!(
            r.env_json("L1", Duration::from_secs(5)).unwrap().trim(),
            "env L1 --json"
        );
    }

    #[test]
    fn refuses_other_subcommands_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("ran");
        let (_d, r) = script(&format!("touch {}", mark.display()));
        for args in [&["release", "x"][..], &["reset", "x"], &["acquire"], &[]] {
            assert!(r.run(args, Duration::from_secs(1)).is_none());
        }
        assert!(!mark.exists(), "a forbidden subcommand was spawned");
    }

    /// The pid in `file` must be gone (reaped) once the runner returned.
    fn assert_reaped(file: &Path) {
        let pid = std::fs::read_to_string(file).unwrap();
        let pid = pid.trim();
        assert!(!pid.is_empty());
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "child {pid} is still there (running or zombie)"
        );
    }

    #[test]
    fn sleeping_child_is_killed_at_the_deadline_and_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let (_d, r) = script(&format!("echo $$ > {}\nexec sleep 30", pidfile.display()));
        let t = Instant::now();
        assert!(r.status_json(DEADLINE).is_none());
        let took = t.elapsed();
        assert!(
            took >= DEADLINE && took < DEADLINE + Duration::from_millis(500),
            "{took:?}"
        );
        assert_reaped(&pidfile);
    }

    #[test]
    fn child_that_closes_stdout_then_hangs_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let (_d, r) = script(&format!(
            "echo $$ > {}\nexec 1>&-\nexec sleep 30",
            pidfile.display()
        ));
        let t = Instant::now();
        assert!(r.status_json(DEADLINE).is_none());
        let took = t.elapsed();
        assert!(
            took >= DEADLINE && took < DEADLINE + Duration::from_millis(500),
            "{took:?}"
        );
        assert_reaped(&pidfile);
    }

    #[test]
    fn nonzero_exit_is_none() {
        let (_d, r) = script(r#"echo '{"error": "not found"}'; exit 13"#);
        assert!(r.env_json("nope", Duration::from_secs(5)).is_none());
    }

    #[test]
    fn missing_program_is_none() {
        let r = CommandRunner::with_program("/nonexistent/dir/gozer");
        assert!(r.status_json(Duration::from_secs(1)).is_none());
    }

    #[test]
    fn source_keeps_polling_after_a_hung_command() {
        // The command hangs past the deadline. The worker must finish (None)
        // so `inflight` clears and the next interval starts a new command.
        let (d, _r) = script("exec sleep 30");
        let mut src = GozerSource::new(Arc::new(CommandRunner::with_program(
            d.path().join("fake-gozer"),
        )));
        let t = Instant::now();
        ctx_poll(&mut src, t);
        assert!(src.inflight.is_some());
        std::thread::sleep(COMMAND_TIMEOUT + Duration::from_millis(500));
        ctx_poll(&mut src, t + POLL_INTERVAL); // collects the None, starts again
        assert!(src.inflight.is_some(), "a new command was started");
        assert!(src.pending.is_none());
    }
}

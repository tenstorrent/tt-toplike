//! Gozer: the lease source and the governor.
//!
//! `gozer` (tt-gozer) is the box's chip lease manager. This module runs two
//! read-only commands: `gozer status --json` and `gozer env <lease> --json`.
//! It never runs `acquire`, `run`, `release`, `reset` or anything that touches
//! a chip, and it never reads gozer's own files. The real runner refuses any
//! other subcommand. Neither command opens `/dev/tenstorrent/*`.
//!
//! # What `gozer status --json` exposes
//!
//! One object, `{"chips": [...], "grain": "board", "queue": [...]}`. Each chip
//! has `bdf` (PCI address), `dev_index`, `board`, `card`, `state`, `who`,
//! `reason`, `pid` (the lease's owner pid), `pids_holding`, `owner_dead` and
//! `overstayed`. It does **not** expose the lease id. This module therefore
//! names a lease by the owner pid (`pid-<pid>`) unless a future gozer adds a
//! `lease_id` field to the chip, which it then prefers. A detached lease (a
//! bare `gozer acquire`) has `pid: null`; it is named `who-<who>` instead
//! (redacted and clipped). A chip with neither is treated as unheld.
//!
//! # [`GozerSource`]
//!
//! Emits chip and lease records from successive status snapshots.
//!
//! * The first snapshot after start only emits `toplike.gozer.state` (and
//!   `toplike.gozer.who`) samples. It emits no `LeaseGranted`, because the
//!   lease was taken before we started watching and we do not know when
//!   (same first-sight rule as the inference source).
//! * Later, a holder that appears on a chip gives one `LeaseGranted`. A holder
//!   that goes away gives one `LeaseReleased`, an `Event::Reset` and a
//!   `Payload::Gap`: gozer resets the chips on release, so readings taken just
//!   after may be garbage. A chip that turns `UNREADABLE` or `DEGRADED`
//!   (gozer reports `pid: null` for those) keeps its previous holder, so a
//!   flapping chip gives no release and no second grant.
//! * A changed state gives one `toplike.gozer.state` sample. A changed `who`
//!   gives one `toplike.gozer.who` sample (empty text when it clears).
//! * When gozer is absent or fails, nothing is emitted and the last snapshot
//!   is kept, so a failed poll does not fake a release.
//!
//! The command runs on a worker thread. `poll` only starts it (at most once
//! every [`POLL_INTERVAL`]) and picks up a finished answer, so it never
//! blocks. The real runner gives up after [`COMMAND_TIMEOUT`].
//!
//! # [`Governor`]
//!
//! A chip-opening source (the later tt-umd/exalens sidecar) asks
//! `Governor::permit(chip)` before it reads. No first-release source does.
//! [`GozerGovernor`] permits a chip only when gozer shows it `HELD` under
//! our own lease. See [`GozerGovernor`] for the matching rules and limits.
//!
//! The sidecar launcher sets `TT_TOPLIKE_LEASE_ID` (the lease id it was
//! given). Gozer sets no variable for it: `gozer env <lease>` exports only
//! `TT_VISIBLE_DEVICES`, a comma list of PCI addresses.

use crate::bus::redact::redact_cmdline;
use crate::bus::signals as sig;
use crate::bus::source::{PollCtx, Source};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tt_toplike_bus::{EntityKey, Event, Gap, Payload, Provenance, Sample, Value};

/// The real runner is started at most once every this long.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// The real runner kills `gozer status --json` after this long.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// Most characters kept of a `who` string, counted after redaction.
pub const MAX_WHO: usize = 128;
/// The governor re-reads status at most this often.
pub const GOVERNOR_REFRESH: Duration = Duration::from_secs(30);
/// The governor denies when its last good status is older than this.
pub const GOVERNOR_STALE: Duration = Duration::from_secs(60);

/// Environment variable that holds our lease id. Set by the sidecar launcher.
pub const LEASE_ENV: &str = "TT_TOPLIKE_LEASE_ID";
/// Environment variable gozer exports for a lease: the visible chip indices.
pub const VISIBLE_ENV: &str = "TT_VISIBLE_DEVICES";

// ---------------------------------------------------------------------------
// Status parsing
// ---------------------------------------------------------------------------

/// One chip as `gozer status --json` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipStatus {
    /// PCI address, the chip's identity.
    pub bdf: String,
    /// `/dev/tenstorrent/N` index.
    pub dev_index: Option<u32>,
    /// `FREE`, `HELD`, `HELD-FOREIGN`, `CLAIMED`, `STALE`, `BUSY-UNTRACKED`,
    /// `DEGRADED` or `UNREADABLE` (or a name a newer gozer adds).
    pub state: String,
    /// The lease's `--who`, raw (user text).
    pub who: Option<String>,
    /// The lease's owner pid.
    pub pid: Option<u64>,
    /// The lease id, if a future gozer exposes it.
    pub lease_id: Option<String>,
}

impl ChipStatus {
    /// Name of whoever holds the chip: the lease id when exposed, else
    /// `pid-<owner pid>`, else `who-<who>` for a detached lease. `None` when
    /// no lease covers the chip.
    fn holder(&self) -> Option<String> {
        match (&self.lease_id, self.pid, self.who.as_deref()) {
            (Some(id), _, _) if !id.is_empty() => Some(id.clone()),
            (_, Some(pid), _) => Some(format!("pid-{pid}")),
            // Detached lease: no owner pid. The who string is all there is.
            (_, None, Some(who)) if !who.is_empty() => Some(format!("who-{}", clean_who(who))),
            _ => None,
        }
    }
}

/// A `who` string safe to store: redacted, then clipped to [`MAX_WHO`].
fn clean_who(who: &str) -> String {
    redact_cmdline(who).chars().take(MAX_WHO).collect()
}

/// A parsed `gozer status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GozerStatus {
    pub chips: Vec<ChipStatus>,
}

/// Parse `gozer status --json`. `None` when the text is not that shape (no
/// `chips` array, or a chip without `bdf` and `state`).
pub fn parse_status(json: &str) -> Option<GozerStatus> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let arr = v.get("chips")?.as_array()?;
    let mut chips = Vec::with_capacity(arr.len());
    for c in arr {
        chips.push(ChipStatus {
            bdf: c.get("bdf")?.as_str()?.to_string(),
            dev_index: c
                .get("dev_index")
                .and_then(|x| x.as_u64())
                .and_then(|x| u32::try_from(x).ok()),
            state: c.get("state")?.as_str()?.to_string(),
            who: c.get("who").and_then(|x| x.as_str()).map(str::to_string),
            pid: c.get("pid").and_then(|x| x.as_u64()),
            lease_id: c
                .get("lease_id")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        });
    }
    Some(GozerStatus { chips })
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Runs a read-only gozer command. Injectable so tests supply JSON.
///
/// One generic method, `run`, serves both `status --json` and
/// `env <lease> --json`.
pub trait StatusRunner: Send + Sync {
    /// The command's stdout, or `None` when gozer is not installed, exits
    /// nonzero or does not finish within `timeout`.
    fn run(&self, args: &[&str], timeout: Duration) -> Option<String>;

    /// `gozer status --json`.
    fn status_json(&self, timeout: Duration) -> Option<String> {
        self.run(&["status", "--json"], timeout)
    }

    /// `gozer env <lease> --json`.
    fn env_json(&self, lease: &str, timeout: Duration) -> Option<String> {
        self.run(&["env", lease, "--json"], timeout)
    }

    /// `gozer env <lease> --json`, telling "gozer says there is no such lease"
    /// (exit 13, `{"error": "not found"}`) from every other failure. The
    /// default cannot tell them apart and reports `Failed`.
    fn env_answer(&self, lease: &str, timeout: Duration) -> EnvAnswer {
        match self.env_json(lease, timeout) {
            Some(s) => EnvAnswer::Chips(s),
            None => EnvAnswer::Failed,
        }
    }
}

/// The outcome of `gozer env <lease> --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvAnswer {
    /// Exit 0 with this stdout.
    Chips(String),
    /// gozer answered that the lease does not exist (released or reaped).
    NotFound,
    /// Not installed, timed out, other nonzero exit.
    Failed,
}

/// Spawns the real `gozer`.
pub struct CommandRunner {
    program: PathBuf,
}

impl CommandRunner {
    /// Runs `gozer` found on PATH.
    pub fn new() -> Self {
        Self::with_program("gozer")
    }

    /// Runs the given program instead of `gozer` (tests use a script).
    pub fn with_program(program: impl AsRef<Path>) -> Self {
        Self {
            program: program.as_ref().to_path_buf(),
        }
    }
}

impl Default for CommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRunner {
    /// Run `args` and return the exit code and stdout of a child that exited
    /// before the deadline. `timeout` covers the whole child: spawn, output
    /// and exit. On expiry the child is killed and reaped. Only `status` and
    /// `env` are run; any other subcommand returns `None` without spawning.
    fn exec(&self, args: &[&str], timeout: Duration) -> Option<(i32, String)> {
        if !matches!(args.first(), Some(&"status") | Some(&"env")) {
            return None;
        }
        crate::startup_log::phase("gozer exec");
        let deadline = Instant::now() + timeout;
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // Read stdout on its own thread so a full pipe cannot stall the child.
        let mut out = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            let ok = out.read_to_string(&mut s).is_ok();
            let _ = tx.send(ok.then_some(s));
        });
        // Wait for the child to exit, up to the deadline.
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        // The child is gone; its output is complete unless a grandchild holds
        // the pipe, so wait only for what is left of the deadline.
        let text = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
            .flatten()?;
        Some((status.code()?, text))
    }
}

impl StatusRunner for CommandRunner {
    fn run(&self, args: &[&str], timeout: Duration) -> Option<String> {
        match self.exec(args, timeout)? {
            (0, text) => Some(text),
            _ => None,
        }
    }

    fn env_answer(&self, lease: &str, timeout: Duration) -> EnvAnswer {
        match self.exec(&["env", lease, "--json"], timeout) {
            Some((0, text)) => EnvAnswer::Chips(text),
            Some((13, text)) if is_not_found(&text) => EnvAnswer::NotFound,
            _ => EnvAnswer::Failed,
        }
    }
}

/// True for gozer's `{"error": "not found"}`.
fn is_not_found(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(|e| e == "not found"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// GozerSource
// ---------------------------------------------------------------------------

/// What the source remembers of a chip between snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
struct View {
    state: String,
    /// Redacted and clipped `who`; empty when none.
    who: String,
    holder: Option<String>,
}

/// Emits lease events and per-chip gozer samples. See the module docs.
pub struct GozerSource {
    runner: Option<Arc<dyn StatusRunner>>,
    /// Answer of the worker thread that is running now.
    inflight: Option<Receiver<Option<String>>>,
    last_start: Option<Instant>,
    /// Status fed (or fetched) and not yet turned into payloads.
    pending: Option<GozerStatus>,
    /// The previous snapshot, by PCI address. `None` before the first.
    prev: Option<BTreeMap<String, View>>,
    /// Set by `resend_all`: the next snapshot sends every chip's state and
    /// `who` samples, changed or not. Lease events are unaffected.
    resend: bool,
}

impl GozerSource {
    /// A source that fetches status itself with `runner`.
    pub fn new(runner: Arc<dyn StatusRunner>) -> Self {
        Self {
            runner: Some(runner),
            inflight: None,
            last_start: None,
            pending: None,
            prev: None,
            resend: false,
        }
    }

    /// A source with the real `gozer status --json` runner.
    pub fn with_command() -> Self {
        Self::new(Arc::new(CommandRunner::new()))
    }

    /// A source that only reads what [`feed`](Self::feed) gives it.
    pub fn fed() -> Self {
        Self {
            runner: None,
            inflight: None,
            last_start: None,
            pending: None,
            prev: None,
            resend: false,
        }
    }

    /// Hand over the latest parsed status. A later feed replaces one not yet
    /// polled.
    pub fn feed(&mut self, status: GozerStatus) {
        self.pending = Some(status);
    }

    /// Collect a finished worker answer and start a new worker when due.
    fn service_worker(&mut self, now: Instant) {
        if let Some(rx) = &self.inflight {
            match rx.try_recv() {
                Ok(answer) => {
                    self.inflight = None;
                    if let Some(st) = answer.as_deref().and_then(parse_status) {
                        self.pending = Some(st);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.inflight = None,
            }
        }
        let due = self
            .last_start
            .map_or(true, |t| now.saturating_duration_since(t) >= POLL_INTERVAL);
        if let (true, true, Some(runner)) = (due, self.inflight.is_none(), &self.runner) {
            let runner = Arc::clone(runner);
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("gozer-status".into())
                .spawn(move || {
                    crate::workload::background::lower_priority();
                    let _ = tx.send(runner.status_json(COMMAND_TIMEOUT));
                });
            self.last_start = Some(now);
            if spawned.is_ok() {
                self.inflight = Some(rx);
            }
        }
    }

    /// Compare `status` with the previous snapshot and emit.
    fn process(&mut self, status: &GozerStatus, out: &mut dyn FnMut(Payload)) {
        let prev = self.prev.take();
        let cur: BTreeMap<String, View> = status
            .chips
            .iter()
            .map(|c| {
                let who = c.who.as_deref().map(clean_who).unwrap_or_default();
                let mut holder = c.holder();
                // gozer reports pid null for these, whatever the lease: keep
                // the holder we saw, so a flapping chip is not a release.
                if holder.is_none() && matches!(c.state.as_str(), "UNREADABLE" | "DEGRADED") {
                    holder = prev
                        .as_ref()
                        .and_then(|p| p.get(&c.bdf))
                        .and_then(|o| o.holder.clone());
                }
                (
                    c.bdf.clone(),
                    View {
                        state: c.state.clone(),
                        who,
                        holder,
                    },
                )
            })
            .collect();

        // State and who samples. After `resend_all` every chip counts as
        // new here (but not for lease events below).
        let resend = std::mem::take(&mut self.resend);
        for (bdf, v) in &cur {
            let old = prev.as_ref().and_then(|p| p.get(bdf)).filter(|_| !resend);
            let chip = EntityKey::chip(bdf);
            if old.map(|o| &o.state) != Some(&v.state) {
                text_sample(out, &chip, sig::GOZER_STATE, &v.state);
            }
            let old_who = old.map(|o| o.who.as_str()).unwrap_or("");
            if old_who != v.who {
                text_sample(out, &chip, sig::GOZER_WHO, &v.who);
            }
        }

        // Lease events: not on the first snapshot.
        if let Some(prev) = &prev {
            let mut released: BTreeMap<String, Vec<EntityKey>> = BTreeMap::new();
            let mut granted: BTreeMap<String, Vec<EntityKey>> = BTreeMap::new();
            for (bdf, v) in &cur {
                let old_holder = prev.get(bdf).and_then(|o| o.holder.clone());
                if old_holder == v.holder {
                    continue;
                }
                if let Some(h) = old_holder {
                    released.entry(h).or_default().push(EntityKey::chip(bdf));
                }
                if let Some(h) = &v.holder {
                    granted
                        .entry(h.clone())
                        .or_default()
                        .push(EntityKey::chip(bdf));
                }
            }
            for (h, chips) in released {
                out(Payload::Event(Event::LeaseReleased {
                    lease: EntityKey::lease(&h),
                    chips: chips.clone(),
                }));
                out(Payload::Event(Event::Reset { chips }));
                out(Payload::Gap(Gap {
                    reason: "gozer released a lease and reset its chips".into(),
                }));
            }
            for (h, chips) in granted {
                out(Payload::Event(Event::LeaseGranted {
                    lease: EntityKey::lease(&h),
                    chips,
                }));
            }
        }
        self.prev = Some(cur);
    }
}

fn text_sample(out: &mut dyn FnMut(Payload), e: &EntityKey, signal: &str, text: &str) {
    out(Payload::Sample(Sample {
        entity: e.clone(),
        signal: signal.to_string(),
        value: Value::Text(text.to_string()),
        prov: Provenance::Measured,
    }));
}

impl Source for GozerSource {
    fn name(&self) -> &'static str {
        "gozer"
    }

    fn poll(&mut self, _ctx: &PollCtx<'_>, now: Instant, out: &mut dyn FnMut(Payload)) {
        self.service_worker(now);
        if let Some(st) = self.pending.take() {
            self.process(&st, out);
        }
    }

    fn resend_all(&mut self) {
        self.resend = true;
    }
}

// ---------------------------------------------------------------------------
// Governor
// ---------------------------------------------------------------------------

/// The answer to "may I read this chip?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permit {
    Go,
    /// Do not read. The text is a fixed reason string.
    Deny(String),
}

/// Decides whether a source that opens a chip on its own account may do so.
pub trait Governor {
    fn permit(&self, chip: &EntityKey) -> Permit;
}

/// What the governor knows. The two halves are cached separately, each with
/// the time it was read.
struct Cache {
    last_attempt: Option<Instant>,
    /// `gozer status --json`.
    status: Option<(Instant, GozerStatus)>,
    /// `gozer env <our lease> --json`: the chips of our lease. Cleared when
    /// gozer says the lease does not exist.
    lease_chips: Option<(Instant, ChipList)>,
}

/// A parsed chip list: gozer writes PCI addresses. In
/// `TT_VISIBLE_DEVICES` (our environment) a plain number is also accepted as
/// a `dev_index`, when an entry has no `:`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ChipList {
    bdfs: Vec<String>,
    indices: Vec<u32>,
}

impl ChipList {
    /// `"0000:01:00.0,0000:02:00.0"`, plus plain numbers when `numbers` is
    /// true. Whitespace and one trailing comma are ignored. `None` for an
    /// empty list or any other entry (a name, an empty entry in the middle, a
    /// range).
    fn parse(s: &str, numbers: bool) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_suffix(',').unwrap_or(s);
        let mut out = ChipList::default();
        for part in s.split(',') {
            let p = part.trim();
            if p.is_empty() {
                return None;
            }
            if p.contains(':') {
                out.bdfs.push(p.to_ascii_lowercase());
            } else if numbers {
                out.indices.push(p.parse::<u32>().ok()?);
            } else {
                return None;
            }
        }
        Some(out)
    }

    fn lists(&self, chip: &ChipStatus) -> bool {
        self.bdfs.iter().any(|b| b.eq_ignore_ascii_case(&chip.bdf))
            || chip.dev_index.is_some_and(|i| self.indices.contains(&i))
    }
}

/// `{"TT_VISIBLE_DEVICES": "<bdf,...>"}` from `gozer env <lease> --json`.
fn parse_env(json: &str) -> Option<ChipList> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    // gozer writes BDFs here, never bare numbers.
    ChipList::parse(v.get(VISIBLE_ENV)?.as_str()?, false)
}

/// Permits a chip only when all of these hold:
///
/// 1. `TT_TOPLIKE_LEASE_ID` is set and is a plain id: 1 to 64 characters of
///    `[A-Za-z0-9_-]`, not starting with `-`. gozer builds a file path from
///    the id, so anything else (`/tmp/x`, `../x`) is refused before any
///    command runs.
/// 2. `gozer env <lease> --json` succeeds and lists the chip's PCI address
///    (the chip belongs to our lease, as gozer says).
/// 3. `TT_VISIBLE_DEVICES` in our own environment lists the chip (gozer
///    writes PCI addresses; a plain number is taken as a `dev_index`).
/// 4. `gozer status --json` shows the chip in state exactly `HELD`.
///
/// Anything else denies. No file of gozer's is read.
///
/// # What `HELD` proves
///
/// For a lease with an owner pid (`gozer run`, or `acquire --owner-pid`),
/// `HELD` means a device is open by the owner or its descendants. Rule 2
/// says the chip is in our lease, so the chip is ours and in use by our
/// lease's process tree.
///
/// For a detached lease (a bare `gozer acquire`), gozer reports `HELD` when
/// any process has the chip open, including another tenant's, since gozer
/// cannot tell whose it is. Rules 2 and 3 still say the chip is ours, but
/// `HELD` there only means "something has it open". Do not read it as "our
/// workload is running".
///
/// `CLAIMED` (lease taken, nothing open yet) denies: there is no workload to
/// observe, and a sidecar that opens a chip first would be the first opener.
///
/// # Caching, and what this is not
///
/// Both commands are re-run at most once per [`GOVERNOR_REFRESH`] (30 s),
/// counting failed attempts. The two answers are kept separately, each with
/// its own time. A half that parses replaces its own older value. When
/// `gozer env` says the lease does not exist (exit 13), our lease's chip list
/// is dropped at once, so the next `permit` denies. A failed refresh of
/// either half keeps its older value until [`GOVERNOR_STALE`] (60 s, counted
/// per half) and then denies. So this governor tolerates one failed refresh
/// (up to 60 s of staleness). It is NOT the sidecar's rule "exit on
/// the first failed 30 s re-check"; the sidecar must implement that itself.
///
/// `permit` can block for up to about 4 s on a refresh (two commands, 2 s
/// each), so call it
/// from the sidecar's own thread. The cache lock is never held while a
/// command runs.
pub struct GozerGovernor {
    lease_id: Option<String>,
    visible: Option<String>,
    runner: Box<dyn StatusRunner>,
    cache: Mutex<Cache>,
}

impl GozerGovernor {
    /// From `TT_TOPLIKE_LEASE_ID` and `TT_VISIBLE_DEVICES`, with the real
    /// `gozer` runner.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var(LEASE_ENV).ok(),
            std::env::var(VISIBLE_ENV).ok(),
            Box::new(CommandRunner::new()),
        )
    }

    /// For tests and callers that supply their own runner.
    pub fn new(
        lease_id: Option<String>,
        visible: Option<String>,
        runner: Box<dyn StatusRunner>,
    ) -> Self {
        Self {
            lease_id,
            visible,
            runner,
            cache: Mutex::new(Cache {
                last_attempt: None,
                status: None,
                lease_chips: None,
            }),
        }
    }

    /// `permit` at an explicit time, so tests can move the clock.
    pub fn permit_at(&self, chip: &EntityKey, now: Instant) -> Permit {
        let deny = |s: &str| Permit::Deny(s.to_string());
        let lease = match self.lease_id.as_deref() {
            Some(l) if !l.is_empty() => l,
            _ => return deny("no lease id in TT_TOPLIKE_LEASE_ID"),
        };
        if !valid_lease_id(lease) {
            return deny("TT_TOPLIKE_LEASE_ID is not a valid lease id");
        }
        let visible = match self.visible.as_deref() {
            Some(v) => v,
            None => return deny("TT_VISIBLE_DEVICES is not set"),
        };
        let visible = match ChipList::parse(visible, true) {
            Some(v) => v,
            None => return deny("TT_VISIBLE_DEVICES is not a list of chips"),
        };
        let (status, lease_chips) = match self.snapshot(lease, now) {
            Some(s) => s,
            None => return deny("gozer status is unavailable"),
        };
        let c = match status
            .chips
            .iter()
            .find(|c| EntityKey::chip(&c.bdf) == *chip)
        {
            Some(c) => c,
            None => return deny("chip is not in gozer status"),
        };
        if c.state != "HELD" {
            return Permit::Deny(format!("chip state is {}, not HELD", state_label(&c.state)));
        }
        if !visible.lists(c) {
            return deny("chip is not in TT_VISIBLE_DEVICES");
        }
        if !lease_chips.lists(c) {
            return deny("chip is not in our lease");
        }
        Permit::Go
    }

    /// The cached halves, refreshed when due. `None` unless both are newer
    /// than [`GOVERNOR_STALE`] (judged per half).
    fn snapshot(&self, lease: &str, now: Instant) -> Option<(GozerStatus, ChipList)> {
        let due = {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let due = c.last_attempt.map_or(true, |t| {
                now.saturating_duration_since(t) >= GOVERNOR_REFRESH
            });
            if due {
                c.last_attempt = Some(now);
            }
            due
        };
        if due {
            // No lock held while the commands run.
            let status = self
                .runner
                .status_json(COMMAND_TIMEOUT)
                .as_deref()
                .and_then(parse_status);
            let env = self.runner.env_answer(lease, COMMAND_TIMEOUT);
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            // A result older than what is stored never replaces it.
            if let Some(st) = status {
                if c.status.as_ref().map_or(true, |(at, _)| *at <= now) {
                    c.status = Some((now, st));
                }
            }
            if c.lease_chips.as_ref().map_or(true, |(at, _)| *at <= now) {
                match env {
                    EnvAnswer::Chips(text) => {
                        if let Some(list) = parse_env(&text) {
                            c.lease_chips = Some((now, list));
                        }
                    }
                    // The lease is gone: stop trusting its old chip list.
                    EnvAnswer::NotFound => c.lease_chips = None,
                    EnvAnswer::Failed => {}
                }
            }
        }
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = |at: &Instant| now.saturating_duration_since(*at) <= GOVERNOR_STALE;
        match (&c.status, &c.lease_chips) {
            (Some((a, st)), Some((b, l))) if fresh(a) && fresh(b) => Some((st.clone(), l.clone())),
            _ => None,
        }
    }
}

/// A lease id gozer can safely turn into a path: 1 to 64 characters of
/// `[A-Za-z0-9_-]`, not starting with `-`.
fn valid_lease_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Governor for GozerGovernor {
    fn permit(&self, chip: &EntityKey) -> Permit {
        self.permit_at(chip, Instant::now())
    }
}

/// Gozer's state name for a deny reason. A name this code does not know
/// becomes `other`, so a reason never carries text from outside.
fn state_label(state: &str) -> &'static str {
    const KNOWN: [&str; 8] = [
        "FREE",
        "HELD",
        "HELD-FOREIGN",
        "CLAIMED",
        "STALE",
        "BUSY-UNTRACKED",
        "DEGRADED",
        "UNREADABLE",
    ];
    KNOWN
        .iter()
        .find(|k| **k == state)
        .copied()
        .unwrap_or("other")
}

#[cfg(test)]
#[path = "gozer_tests.rs"]
mod tests;

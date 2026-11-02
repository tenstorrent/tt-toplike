//! Gozer: the lease source and the governor.
//!
//! `gozer` (tt-gozer) is the box's chip lease manager. This module reads its
//! `gozer status --json` and nothing else. It never runs `acquire`, `run`,
//! `release`, `reset` or any command that touches a chip. `gozer status` itself
//! never opens `/dev/tenstorrent/*`.
//!
//! # What `gozer status --json` exposes
//!
//! One object, `{"chips": [...], "grain": "board", "queue": [...]}`. Each chip
//! has `bdf` (PCI address), `dev_index`, `board`, `card`, `state`, `who`,
//! `reason`, `pid` (the lease's owner pid), `pids_holding`, `owner_dead` and
//! `overstayed`. It does **not** expose the lease id. This module therefore
//! names a lease by the owner pid (`pid-<pid>`) unless a future gozer adds a
//! `lease_id` field to the chip, which it then prefers.
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
//!   after may be garbage.
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
//! `TT_VISIBLE_DEVICES`.

use crate::bus::redact::redact_cmdline;
use crate::bus::signals as sig;
use crate::bus::source::{PollCtx, Source};
use std::collections::BTreeMap;
use std::io::Read;
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
    /// `pid-<owner pid>`. `None` when no lease covers the chip.
    fn holder(&self) -> Option<String> {
        match (&self.lease_id, self.pid) {
            (Some(id), _) if !id.is_empty() => Some(id.clone()),
            (_, Some(pid)) => Some(format!("pid-{pid}")),
            _ => None,
        }
    }
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

/// Runs `gozer status --json`. Injectable so tests supply JSON.
pub trait StatusRunner: Send + Sync {
    /// The command's stdout, or `None` when gozer is not installed, fails or
    /// does not finish within `timeout`.
    fn status_json(&self, timeout: Duration) -> Option<String>;
}

/// Spawns the real `gozer status --json`.
pub struct CommandRunner;

impl StatusRunner for CommandRunner {
    fn status_json(&self, timeout: Duration) -> Option<String> {
        let mut child = Command::new("gozer")
            .args(["status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // Read stdout on its own thread so a full pipe cannot stall the child
        // while we wait for it.
        let mut out = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            let ok = out.read_to_string(&mut s).is_ok();
            let _ = tx.send(ok.then_some(s));
        });
        let text = match rx.recv_timeout(timeout) {
            Ok(t) => t,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        };
        let status = child.wait().ok()?;
        if status.success() {
            text
        } else {
            None
        }
    }
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
        }
    }

    /// A source with the real `gozer status --json` runner.
    pub fn with_command() -> Self {
        Self::new(Arc::new(CommandRunner))
    }

    /// A source that only reads what [`feed`](Self::feed) gives it.
    pub fn fed() -> Self {
        Self {
            runner: None,
            inflight: None,
            last_start: None,
            pending: None,
            prev: None,
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
        let cur: BTreeMap<String, View> = status
            .chips
            .iter()
            .map(|c| {
                let who = c.who.as_deref().map(redact_cmdline).unwrap_or_default();
                let who: String = who.chars().take(MAX_WHO).collect();
                (
                    c.bdf.clone(),
                    View {
                        state: c.state.clone(),
                        who,
                        holder: c.holder(),
                    },
                )
            })
            .collect();
        let prev = self.prev.take();

        // State and who samples.
        for (bdf, v) in &cur {
            let old = prev.as_ref().and_then(|p| p.get(bdf));
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

/// Looks up the owner pid of a lease id.
type OwnerLookup = Box<dyn Fn(&str) -> Option<u64> + Send + Sync>;

/// Mutable cache state of the governor.
struct Cache {
    last_attempt: Option<Instant>,
    /// The last status that parsed, and when it was read.
    good: Option<(Instant, GozerStatus)>,
}

/// Permits a chip only when `gozer status --json` shows it `HELD` under our
/// own lease and the chip is in `TT_VISIBLE_DEVICES`.
///
/// # How our lease is matched
///
/// Status does not expose a lease id. Each chip carries `pid`, the lease's
/// owner pid. The governor reads the owner pid of our lease id from gozer's
/// own record, `<root>/leases/<id>.json` field `pid`, where `<root>` is
/// `$GOZER_ROOT` or `/tmp/tt-gozer` (a read of a plain file; no chip is
/// touched). A chip matches when its status `pid` equals that pid. If a
/// later gozer adds `lease_id` to the chip, that is compared first.
///
/// Limits: two leases with the same owner pid look the same (an `adopt` or
/// `--owner-pid` on one supervisor for several leases). A lease file that
/// cannot be read (another user's `/tmp`, a different `GOZER_ROOT`) denies.
/// A reused pid is not distinguished.
///
/// Only `HELD` permits. `CLAIMED` (lease taken, nothing open yet), `STALE`,
/// `HELD-FOREIGN`, `BUSY-UNTRACKED`, `FREE`, `DEGRADED` and `UNREADABLE` deny.
///
/// # Caching
///
/// Status is re-read at most once per [`GOVERNOR_REFRESH`] (30 s), counting
/// failed attempts. A status older than [`GOVERNOR_STALE`] (60 s) with a
/// failed refresh denies. `permit` can block up to the runner's 2 s timeout
/// when it refreshes, so call it from the sidecar's own thread.
pub struct GozerGovernor {
    lease_id: Option<String>,
    visible: Option<String>,
    runner: Box<dyn StatusRunner>,
    owner_of: OwnerLookup,
    cache: Mutex<Cache>,
}

impl GozerGovernor {
    /// From `TT_TOPLIKE_LEASE_ID` and `TT_VISIBLE_DEVICES`, with the real
    /// `gozer` runner and the lease file lookup.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var(LEASE_ENV).ok(),
            std::env::var(VISIBLE_ENV).ok(),
            Box::new(CommandRunner),
        )
    }

    /// For tests and callers that supply their own runner. The owner pid
    /// comes from the lease file.
    pub fn new(
        lease_id: Option<String>,
        visible: Option<String>,
        runner: Box<dyn StatusRunner>,
    ) -> Self {
        Self {
            lease_id,
            visible,
            runner,
            owner_of: Box::new(owner_pid_from_lease_file),
            cache: Mutex::new(Cache {
                last_attempt: None,
                good: None,
            }),
        }
    }

    /// Replace the lease owner lookup (tests).
    pub fn with_owner_lookup(mut self, f: OwnerLookup) -> Self {
        self.owner_of = f;
        self
    }

    /// `permit` at an explicit time, so tests can move the clock.
    pub fn permit_at(&self, chip: &EntityKey, now: Instant) -> Permit {
        let deny = |s: &str| Permit::Deny(s.to_string());
        let lease = match self.lease_id.as_deref() {
            Some(l) if !l.is_empty() => l,
            _ => return deny("no lease id in TT_TOPLIKE_LEASE_ID"),
        };
        let visible = match self.visible.as_deref() {
            Some(v) => v,
            None => return deny("TT_VISIBLE_DEVICES is not set"),
        };
        let visible = match parse_visible(visible) {
            Some(v) => v,
            None => return deny("TT_VISIBLE_DEVICES is not a list of chip numbers"),
        };
        let status = match self.status(now) {
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
            return Permit::Deny(format!("chip state is {}, not HELD", c.state));
        }
        match c.dev_index {
            Some(i) if visible.contains(&i) => {}
            _ => return deny("chip is not in TT_VISIBLE_DEVICES"),
        }
        let ours = match &c.lease_id {
            Some(id) => id == lease,
            None => match (c.pid, (self.owner_of)(lease)) {
                (Some(p), Some(o)) => p == o,
                (_, None) => return deny("our lease record cannot be read"),
                (None, _) => false,
            },
        };
        if ours {
            Permit::Go
        } else {
            deny("chip is held by another lease")
        }
    }

    /// The cached status, refreshed when due. `None` when there is no status
    /// newer than [`GOVERNOR_STALE`].
    fn status(&self, now: Instant) -> Option<GozerStatus> {
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let due = c.last_attempt.map_or(true, |t| {
            now.saturating_duration_since(t) >= GOVERNOR_REFRESH
        });
        if due {
            c.last_attempt = Some(now);
            let parsed = self
                .runner
                .status_json(COMMAND_TIMEOUT)
                .as_deref()
                .and_then(parse_status);
            if let Some(st) = parsed {
                c.good = Some((now, st));
            }
        }
        match &c.good {
            Some((at, st)) if now.saturating_duration_since(*at) <= GOVERNOR_STALE => {
                Some(st.clone())
            }
            _ => None,
        }
    }
}

impl Governor for GozerGovernor {
    fn permit(&self, chip: &EntityKey) -> Permit {
        self.permit_at(chip, Instant::now())
    }
}

/// `"0,1"` to `[0, 1]`. `None` for anything that is not a comma list of
/// numbers (an empty value, a stray name, a trailing comma).
fn parse_visible(s: &str) -> Option<Vec<u32>> {
    s.split(',')
        .map(|p| p.trim().parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()
        .filter(|v| !v.is_empty())
}

/// Owner pid from gozer's lease record. `None` when the id is not a plain
/// token or the file is missing or unreadable.
fn owner_pid_from_lease_file(lease_id: &str) -> Option<u64> {
    if lease_id.is_empty()
        || !lease_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let root = std::env::var("GOZER_ROOT").unwrap_or_else(|_| "/tmp/tt-gozer".into());
    let text = std::fs::read_to_string(format!("{root}/leases/{lease_id}.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("pid")?
        .as_u64()
}

#[cfg(test)]
#[path = "gozer_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure parsers for TT inference-server probe output: `docker stats`, the
//! `/tt-liveness` HTTP probe, `env`/`printenv` dumps, and `ps` snapshots.
//!
//! These functions never touch the network or the filesystem themselves —
//! they only interpret text that some other layer already captured. That
//! keeps them trivially unit-testable and panic-free on untrusted input
//! (raw docker/HTTP/process output), since a malformed line should degrade
//! to `None`/a conservative `Readiness` rather than crash the monitor.

/// `ps` invocation whose first data row is the service's top-CPU consumer.
/// Correct as a *global* `ps` when run via `docker exec` (the container's own
/// PID namespace already scopes it to that container's processes);
/// `SystemProbe` overrides `ContainerProbe::top_proc_cmd` for a host-keyed
/// service to scope this the same way `process_tree_pids` does.
pub(crate) const TOP_PROC_CMD: &str = "ps -eo pcpu,rss,comm --sort=-pcpu";

/// Readiness ladder from a liveness probe.
#[derive(Debug, Clone, PartialEq)]
pub enum Readiness {
    Down,                             // connection refused / no response
    NotReady,                         // up but model not loaded (e.g. 405 "Model is not ready")
    Ready { runner: Option<String> }, // 200; runner = runner_in_use if present
}

/// Parse `{{.CPUPerc}}|{{.MemUsage}}` → (cpu%, rss bytes). MemUsage is "USED / LIMIT".
pub fn parse_docker_stats(line: &str) -> Option<(f32, u64)> {
    let (cpu_s, mem_s) = line.split_once('|')?;
    let cpu = cpu_s.trim().trim_end_matches('%').parse::<f32>().ok()?;
    let used = mem_s.split('/').next()?.trim();
    Some((cpu, parse_size_bytes(used)?))
}

/// "39.96GiB" / "812916KiB" / "700MiB" → bytes.
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let v = num.trim().parse::<f64>().ok()?;
    let mult = match unit.trim().to_lowercase().as_str() {
        "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "tib" | "tb" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((v * mult) as u64)
}

/// Interpret an HTTP status + body from the `/tt-liveness` probe as a `Readiness`.
pub fn parse_liveness(status: u16, body: &str) -> Readiness {
    match status {
        0 => Readiness::Down,
        200 => {
            // pull runner_in_use if present (tolerant substring parse, no serde dep needed)
            let runner = body
                .split_once("\"runner_in_use\"")
                .and_then(|(_, rest)| rest.split_once(':'))
                .and_then(|(_, rest)| rest.split('"').nth(1))
                .map(|s| s.to_string());
            Readiness::Ready { runner }
        }
        _ => Readiness::NotReady, // 405 "Model is not ready", 503, etc.
    }
}

/// Extract `KEY=VALUE` from `env`/`printenv`-style output.
pub fn parse_env_var(env_output: &str, key: &str) -> Option<String> {
    env_output.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Count non-empty lines (kernel-artifact count, FD count).
pub fn count_lines(s: &str) -> usize {
    s.lines().filter(|l| !l.trim().is_empty()).count()
}

/// True if ANY process in `ps -eo pcpu,rss,comm …` output is a python server.
/// The server can sit near 0% CPU while it mmaps/loads weights (IO-bound), so
/// another process may top the CPU chart — deriving liveness from only the top
/// row (as we once did) mis-reads a loading server as Down. Scan every data row.
pub fn contains_python(ps_output: &str) -> bool {
    ps_output
        .lines()
        .skip(1) // header
        .filter_map(|l| l.split_whitespace().nth(2)) // comm column
        .any(|comm| comm.contains("python"))
}

/// First data row of `ps -eo pcpu,rss,comm --sort=-pcpu` → (comm, cpu%, rss bytes).
/// rss is KiB in ps output. Skips the header line.
pub fn top_process(ps_output: &str) -> Option<(String, f32, u64)> {
    // Skip the header row, then parse the first data row. (A plain `for` loop
    // here trips clippy::never_loop, since every path either returns or
    // short-circuits via `?` on the first iteration — there's never a second.)
    let line = ps_output.lines().nth(1)?;
    let mut it = line.split_whitespace();
    let cpu = it.next()?.parse::<f32>().ok()?;
    let rss_kib = it.next()?.parse::<u64>().ok()?;
    let comm = it.next()?.to_string();
    Some((comm, cpu, rss_kib.saturating_mul(1024)))
}

/// Parse one `/proc/<pid>/task/<pid>/children` file's contents
/// (whitespace-separated child pids) into a `Vec<i32>`. Pure — a malformed
/// or non-numeric token is silently skipped rather than failing the whole
/// parse, matching every other parser in this module.
fn parse_children(text: &str) -> Vec<i32> {
    text.split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// All pids in the process tree rooted at `root` (inclusive), found by
/// recursively reading `/proc/<pid>/task/<pid>/children`. Docker gets this
/// scoping for free from the container's own PID namespace (`docker
/// exec`/`docker stats` only ever see the container's own processes); a bare
/// host-launched vLLM has no such boundary, and it does fork real children
/// (a `g++` compile child during kernel compilation, TT device worker
/// subprocess(es)) whose CPU/RSS must count toward the same service's
/// signals. Best-effort: a pid whose `children` file is already gone (it
/// exited between reads) simply isn't descended further — same
/// degrade-gracefully contract as every other probe helper.
#[cfg(target_os = "linux")]
fn process_tree_pids(root: i32) -> Vec<i32> {
    let mut out = vec![root];
    let mut frontier = vec![root];
    // Guards against re-visiting a pid: a real /proc tree can't cycle back to
    // an ancestor, but a malformed children file (or a future test double)
    // could report the same pid as a descendant more than once, so `seen`
    // ensures every pid is pushed onto `out`/`frontier` at most once.
    let mut seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    seen.insert(root);
    while let Some(pid) = frontier.pop() {
        let path = format!("/proc/{pid}/task/{pid}/children");
        if let Ok(text) = std::fs::read_to_string(&path) {
            for child in parse_children(&text) {
                if seen.insert(child) {
                    out.push(child);
                    frontier.push(child);
                }
            }
        }
    }
    out
}

/// Non-Linux: no `/proc` children interface, so the tree is just the root.
/// The host path is Linux-only in practice (`parse_direct_vllm` is fed from
/// `/proc`), but this keeps `probe.rs` compiling on the macOS/Windows CI jobs.
#[cfg(not(target_os = "linux"))]
fn process_tree_pids(root: i32) -> Vec<i32> {
    vec![root]
}

// ── Container access abstraction ────────────────────────────────────────────
//
// The monitor's tick loop needs raw text from a running container (env dump,
// resource stats, an exec'd shell probe, an HTTP health check) — all of it
// I/O, none of it pure. `ContainerProbe` isolates that I/O behind a trait so
// `fold_tick` (the actual decision logic) can be unit-tested against a fake
// without touching a real `docker` binary or socket.

use std::process::Command;

/// One tick's raw sample for a service, already parsed from probe output into
/// the shapes `fold_tick` needs. Built by the monitor from `ContainerProbe`
/// calls; consumed purely by `fold_tick`.
pub struct TickSample {
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub kernel_count: usize,
    /// Count of loaded weight shards (`.tensorbin` under `$CACHE_ROOT`) — the
    /// weight-load-phase counterpart to `kernel_count`'s compile signal.
    pub loaded_count: usize,
    pub safetensors_fds: usize,
    pub readiness: Readiness,
    pub top_proc: Option<String>,
    pub python_alive: bool,
    pub last_log: Option<String>,
    /// Raw body text from the `/metrics` scrape (empty/unparseable → no
    /// serving stats this tick). Parsing happens in `fold_tick`, not here,
    /// so `probe.rs` stays a pure-parser + I/O-trait module.
    pub metrics_text: String,
}

/// Abstract container access so the monitor is testable with a fake. Trail: a
/// host/systemd impl for non-Docker installs would implement this same trait.
pub trait ContainerProbe: Send {
    /// `printenv`-style dump of the container's environment.
    fn env(&self, container: &str) -> String;
    /// `"cpu%|memusage"` resource snapshot (see [`parse_docker_stats`]).
    fn stats(&self, container: &str) -> String;
    /// Run `sh -c sh` inside the container, returning stdout.
    fn exec(&self, container: &str, sh: &str) -> String;
    /// GET `path` on the container's published `port`. `(status, body)`;
    /// status `0` means unreachable (down or timed out).
    fn http(&self, port: u16, path: &str) -> (u16, String);

    /// The `ps`-style command whose first data row is the service's
    /// top-CPU process (see [`top_process`]/[`contains_python`]'s expected
    /// 3-column, header-then-rows shape). Default: [`TOP_PROC_CMD`] — correct
    /// for `DockerProbe`, since `docker exec` already scopes to the
    /// container's own PID namespace. `key` is the same identity string
    /// passed to `env`/`stats`/`exec`.
    fn top_proc_cmd(&self, key: &str) -> String {
        let _ = key;
        TOP_PROC_CMD.to_string()
    }

    /// Enumerate running TT inference-server containers directly (via `docker
    /// ps` + `docker inspect`), independent of any foreground `docker run` host
    /// process — this is how **detached** (`-d`), `docker compose`, and
    /// systemd-managed containers get found. Default returns empty so test
    /// fakes opt out; only [`DockerProbe`] implements it.
    fn list_servers(&self) -> Vec<super::InferenceServer> {
        Vec::new()
    }
}

/// Real `docker`-CLI-backed [`ContainerProbe`]. All calls shell out; every
/// method degrades to an empty string / status 0 on error rather than
/// panicking, since a mid-tick docker hiccup shouldn't take down the monitor.
pub struct DockerProbe;

impl ContainerProbe for DockerProbe {
    fn env(&self, c: &str) -> String {
        docker(&["exec", c, "env"])
    }
    fn stats(&self, c: &str) -> String {
        docker(&[
            "stats",
            "--no-stream",
            "--format",
            "{{.CPUPerc}}|{{.MemUsage}}",
            c,
        ])
    }
    fn exec(&self, c: &str, sh: &str) -> String {
        docker(&["exec", c, "sh", "-c", sh])
    }
    fn http(&self, port: u16, path: &str) -> (u16, String) {
        // Reuse the crate's localhost HTTP helper (liveness_probe) for status+body.
        crate::workload::liveness_probe::http_get_status_body(port, path)
    }

    fn list_servers(&self) -> Vec<super::InferenceServer> {
        // List running containers (name + image), keep TT inference images, and
        // inspect each match into a structured record. All docker I/O — only
        // ever called on the monitor's background thread, never the render path.
        let listing = docker(&["ps", "--no-trunc", "--format", "{{.Names}}\t{{.Image}}"]);
        let mut out = Vec::new();
        for line in listing.lines() {
            let Some((name, image)) = line.split_once('\t') else {
                continue;
            };
            if !super::detect::is_tt_inference_image(image) {
                continue;
            }
            if let Some(s) = super::detect::parse_inspect(&docker(&["inspect", name.trim()])) {
                out.push(s);
            }
        }
        out
    }
}

/// Parse the pid out of a `service_key`-produced host key
/// (`"host-vllm-<pid>"`), or `None` for a Docker-shaped key (a plain
/// container name). `SystemProbe` uses this to decide, per call, which
/// backing implementation handles a given identity key.
fn pid_from_key(key: &str) -> Option<i32> {
    key.strip_prefix(super::detect::HOST_KEY_PREFIX)?
        .parse()
        .ok()
}

/// Read `/proc/<pid>/environ` (NUL-separated `KEY=VALUE` records) and
/// reformat as newline-separated `KEY=VALUE` lines — the shape
/// [`parse_env_var`] already expects (matches the Docker path's
/// `env`/`printenv` dump shape). Missing/unreadable (process exited,
/// permission denied) degrades to `""`, same as every other probe method.
fn host_environ_text(pid: i32) -> String {
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => String::new(),
    }
}

/// Build the `"{cpu}%|{rss_bytes}B / 0B"` text `host_stats_text` returns —
/// factored out so tests can pin the exact format contract without spawning
/// a real `ps`.
fn format_host_stats(cpu_pct: f32, rss_bytes: u64) -> String {
    format!("{cpu_pct:.2}%|{rss_bytes}B / 0B")
}

/// `"{cpu}%|{rss_bytes}B / 0B"` — matches [`parse_docker_stats`]'s expected
/// shape (it only reads the "used" side of the `/`, so the "0B" denominator
/// is a placeholder, not a real memory limit; documented so it isn't
/// mistaken for one). CPU% and RSS are summed over the caller-supplied pid
/// list (see [`HostProbeCache::tree_pids`]) via a single tree-scoped `ps`
/// call — a bare host process has no cgroup to aggregate by the way `docker
/// stats` does for a container.
///
/// Known limitation: `ps`'s `%CPU` column is a lifetime average
/// (cputime/elapsed since the process started), not an instantaneous
/// snapshot like `docker stats` reports. A host-probed service that
/// compiled kernels for 90 minutes and then went idle may keep reporting a
/// stale-high CPU% long after an equivalent container's `docker stats`
/// would show it drop.
fn host_stats_text(pids: &[i32]) -> String {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let out = run_local(&["ps", "-o", "pcpu=,rss=", "-p", &pid_list]);
    let (mut cpu_sum, mut rss_kib_sum) = (0.0f32, 0u64);
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let cpu = it.next().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        let rss = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        cpu_sum += cpu;
        rss_kib_sum = rss_kib_sum.saturating_add(rss);
    }
    let rss_bytes = rss_kib_sum.saturating_mul(1024);
    format_host_stats(cpu_sum, rss_bytes)
}

/// The tree-scoped `ps` command a host-keyed [`ContainerProbe::top_proc_cmd`]
/// returns — same 3-column, header-then-rows shape [`TOP_PROC_CMD`] produces
/// (so [`top_process`]/[`contains_python`] parse it unchanged), scoped to the
/// caller-supplied pid list instead of relying on a container PID namespace.
fn host_top_proc_cmd(pids: &[i32]) -> String {
    let pid_list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("ps -o pcpu,rss,comm --sort=-pcpu -p {pid_list}")
}

/// Environment variables `host_exec` forwards from the target process's own
/// `environ` — exactly what `KERNEL_FIND_CMD`/`LOADED_FIND_CMD` reference
/// (`$TT_METAL_HOME`, `$CACHE_ROOT`, `$HOME`). Deliberately an allowlist, not
/// a copy of the whole environ: only root can read another user's
/// `/proc/<pid>/environ`, so a local user could otherwise plant a process
/// whose environ carries `LD_PRELOAD` (or similar) alongside the
/// `MESH_DEVICE`/`TT_METAL_HOME` gate `parse_direct_vllm` checks, and have
/// the root-run monitor's next probe tick spawn `sh -c '<find …>'` with that
/// variable set — `sh` isn't setuid, so the dynamic linker honors it.
/// Forwarding only the handful of names the two shell commands actually need
/// closes that off. `PATH` is deliberately **not** in this list — see
/// [`host_exec`].
const HOST_EXEC_FORWARDED_VARS: &[&str] = &["TT_METAL_HOME", "CACHE_ROOT", "HOME"];

/// The fixed `PATH` given to every `host_exec` shell — never the target
/// process's own. Sufficient for the only commands `KERNEL_FIND_CMD`/
/// `LOADED_FIND_CMD` invoke by bare name (`find`, `wc`), both standard-issue
/// on any Linux box this runs on.
const HOST_EXEC_PATH: &str = "/usr/bin:/bin:/usr/local/bin";

/// Run `sh -c <sh>` locally, with an environment built from scratch (not
/// inherited) so a locally-spawned shell doesn't pick up tt-toplike's own
/// process environment the way a docker-exec'd shell would automatically be
/// scoped to the container's env. Only [`HOST_EXEC_FORWARDED_VARS`] are
/// copied from the target pid's own `environ` — **`PATH` is never one of
/// them**, and the interpreter itself is invoked by an absolute path, not a
/// bare name. `Command::new("sh")` resolves that bare name using whatever
/// `PATH` ends up set on the builder (confirmed empirically: a fake `sh`
/// placed on a PATH forwarded via `.env()` executes over the real `/bin/sh`)
/// — so forwarding the target's own `PATH` here would have let any process
/// satisfying `parse_direct_vllm`'s gate (which the process's own,
/// self-reported environment controls) redirect the root-run monitor into
/// executing an attacker-planted binary as its next probe tick. `find`/`wc`
/// inside the script body are likewise resolved via this fixed `PATH`, not
/// the target's.
///
/// Takes the target's `environ` text as a parameter rather than reading
/// `/proc/<pid>/environ` itself — the caller (`SystemProbe`, via
/// [`HostProbeCache`]) already has it cached for this probe tick, and a
/// bare-`pid` signature would invite re-reading it here on every call.
fn host_exec(sh: &str, env_text: &str) -> String {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(sh);
    cmd.env_clear();
    for key in HOST_EXEC_FORWARDED_VARS {
        if let Some(v) = parse_env_var(env_text, key) {
            cmd.env(key, v);
        }
    }
    cmd.env("PATH", HOST_EXEC_PATH);
    run_bounded(cmd)
}

/// How long a per-pid `environ` read / process-tree walk may be reused
/// before being considered stale — long enough to cover every call
/// `build_sample` makes for one service within one probe tick (all
/// synchronous, occurring within milliseconds of each other: `env`, two
/// `exec`s for the kernel/weight-shard `find` probes, `stats`, and
/// `top_proc_cmd` + its own `exec`, all keyed by the same pid), short
/// enough to be naturally stale well before the next tick (services are
/// probed on a several-second cadence, see `TICK_INTERVAL` in `monitor.rs`).
/// Never serves stale *data* across ticks — CPU/RSS genuinely change every
/// tick — only the pid list and environ text, which don't.
const HOST_PROBE_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(500);

/// Per-pid memoization for `SystemProbe`'s host-keyed path. Without this,
/// one `build_sample` call for a single host-keyed service independently
/// re-read `/proc/<pid>/environ` up to four times and re-walked
/// `process_tree_pids` twice — pure repeated I/O for data that cannot have
/// changed between those calls. Single-threaded only (the monitor's probe
/// runs on one dedicated background thread — see `spawn_with_probe` — so
/// plain `RefCell`, not `Mutex`, is sufficient and cheaper).
struct HostProbeCache {
    environ: std::cell::RefCell<Option<(i32, std::time::Instant, String)>>,
    tree: std::cell::RefCell<Option<(i32, std::time::Instant, Vec<i32>)>>,
}

impl HostProbeCache {
    fn new() -> Self {
        Self {
            environ: std::cell::RefCell::new(None),
            tree: std::cell::RefCell::new(None),
        }
    }

    fn environ_text(&self, pid: i32) -> String {
        if let Some((cached_pid, at, text)) = self.environ.borrow().as_ref() {
            if *cached_pid == pid && at.elapsed() < HOST_PROBE_CACHE_TTL {
                return text.clone();
            }
        }
        let text = host_environ_text(pid);
        *self.environ.borrow_mut() = Some((pid, std::time::Instant::now(), text.clone()));
        text
    }

    fn tree_pids(&self, pid: i32) -> Vec<i32> {
        if let Some((cached_pid, at, pids)) = self.tree.borrow().as_ref() {
            if *cached_pid == pid && at.elapsed() < HOST_PROBE_CACHE_TTL {
                return pids.clone();
            }
        }
        let pids = process_tree_pids(pid);
        *self.tree.borrow_mut() = Some((pid, std::time::Instant::now(), pids.clone()));
        pids
    }
}

/// Wraps a [`DockerProbe`] and dispatches each `ContainerProbe` call by the
/// identity key's shape: a `"host-vllm-<pid>"` key (see
/// [`crate::workload::inference_server::detect::service_key`]) is handled by
/// reading `/proc`/running local `ps`/`sh` scoped to that pid's whole process
/// tree; any other key (a Docker container name) is forwarded to the wrapped
/// `DockerProbe` unchanged. This is the probe `InferenceServerMonitor::spawn`
/// constructs, so both deployment shapes are monitored simultaneously.
pub(crate) struct SystemProbe {
    docker: DockerProbe,
    cache: HostProbeCache,
}

impl SystemProbe {
    pub(crate) fn new(docker: DockerProbe) -> Self {
        Self {
            docker,
            cache: HostProbeCache::new(),
        }
    }
}

impl ContainerProbe for SystemProbe {
    fn env(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => self.cache.environ_text(pid),
            None => self.docker.env(key),
        }
    }
    fn stats(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_stats_text(&self.cache.tree_pids(pid)),
            None => self.docker.stats(key),
        }
    }
    fn exec(&self, key: &str, sh: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_exec(sh, &self.cache.environ_text(pid)),
            None => self.docker.exec(key, sh),
        }
    }
    fn http(&self, port: u16, path: &str) -> (u16, String) {
        // Container-agnostic already — plain localhost HTTP either way.
        self.docker.http(port, path)
    }
    fn top_proc_cmd(&self, key: &str) -> String {
        match pid_from_key(key) {
            Some(pid) => host_top_proc_cmd(&self.cache.tree_pids(pid)),
            None => self.docker.top_proc_cmd(key),
        }
    }
    fn list_servers(&self) -> Vec<super::InferenceServer> {
        // Detached host processes are already found by the periodic
        // `HostProcesses::detected_inference_servers()` re-scan (there's no
        // "detached vLLM process" concept analogous to a detached container
        // needing separate enumeration) — only forward Docker's.
        self.docker.list_servers()
    }
}

/// Hard cap on any single `docker` invocation. The monitor probes on one
/// background thread, so without this a wedged docker daemon (or a `docker
/// exec` into a hung container) would block *all* services' probing
/// indefinitely. Generous — a healthy `docker stats`/`exec`/`inspect` returns
/// well under a second.
const DOCKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Hard cap on bytes read from one `docker` invocation's stdout. `KERNEL_FIND_CMD`
/// / `LOADED_FIND_CMD` run `find` inside the container, so a pathologically large
/// cache directory could otherwise buffer unboundedly; the counts we derive (line
/// counts, short JSON) never approach this. Mirrors the 1 MiB cap in
/// `liveness_probe::http_get_localhost`, but larger since a big kernel cache is
/// a legitimate many-thousand-line listing.
const MAX_DOCKER_OUTPUT: u64 = 8 * 1024 * 1024;

/// Run a pre-built `Command`, bounded by [`DOCKER_TIMEOUT`] (renamed in
/// spirit only — the bound applies to any local subprocess this module
/// spawns, not just `docker`) and [`MAX_DOCKER_OUTPUT`]. Any spawn/exec
/// failure, or exceeding the timeout, yields `""` rather than panicking or
/// blocking the monitor thread indefinitely; on timeout the child is killed
/// so the reader thread's pipe hits EOF and the thread exits (no orphan).
fn run_bounded(mut cmd: Command) -> String {
    use std::io::Read;
    use std::process::Stdio;

    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut so) = stdout {
            let _ = so.by_ref().take(MAX_DOCKER_OUTPUT).read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    match rx.recv_timeout(DOCKER_TIMEOUT) {
        Ok(s) => {
            let _ = child.wait();
            s
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            String::new()
        }
    }
}

/// Run `docker <args>`, returning stdout as a lossy UTF-8 string — see
/// [`run_bounded`] for the timeout/output-cap contract.
pub(crate) fn docker(args: &[&str]) -> String {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    run_bounded(cmd)
}

/// Run `args[0] args[1..]` locally (no `docker` wrapper) — used by
/// `SystemProbe`'s host-process probing (`ps`, `sh -c <find/ps command>`)
/// where `docker exec`'s namespace scoping doesn't apply. Same bounds as
/// [`docker`]. Empty `args` yields `""` without spawning anything.
fn run_local(args: &[&str]) -> String {
    let Some((program, rest)) = args.split_first() else {
        return String::new();
    };
    let mut cmd = Command::new(program);
    cmd.args(rest);
    run_bounded(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll `/proc/<pid>/environ` (bypassing any cache) until it contains
    /// `marker`, or panic after a generous bounded wait.
    ///
    /// `Command::spawn()` returns as soon as `fork()` succeeds — it makes no
    /// promise that the child has reached `exec()` yet. Until it has,
    /// `/proc/<pid>/environ` still reflects the *pre-exec* image (typically
    /// the test harness's own environment, inherited across the fork), not
    /// `/bin/sleep`'s post-exec one carrying our injected `MARKER`. Under
    /// load (e.g. the full suite running many tests in parallel) that
    /// fork-to-exec gap can outlast an immediate read, so a test that reads
    /// `environ` right after `spawn()` races the child's own startup. This
    /// helper removes the race by waiting for the observable post-exec state
    /// instead of assuming a fixed delay is long enough.
    #[cfg(target_os = "linux")]
    fn wait_for_environ_marker(pid: i32, marker: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let text = host_environ_text(pid);
            if text.contains(marker) {
                return text;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "timed out after 5s waiting for pid {pid}'s /proc/{pid}/environ \
                     to contain {marker:?} (child never appeared to exec); last \
                     read: {text:?}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// `HostProbeCache` memoizes per-pid, keyed by pid — the most likely way
    /// a hand-rolled cache like this goes wrong is serving pid A's data for
    /// pid B. Spawns two real distinct processes with genuinely different
    /// environments and confirms each pid's cached environ matches only its
    /// own process, never the other's.
    #[test]
    #[cfg(target_os = "linux")]
    fn host_probe_cache_keys_environ_by_pid_not_a_single_shared_slot() {
        let mut child_a = Command::new("/bin/sleep")
            .arg("5")
            .env_clear()
            .env("MARKER", "PROCESS_A")
            .spawn()
            .expect("failed to spawn child A");
        let mut child_b = Command::new("/bin/sleep")
            .arg("5")
            .env_clear()
            .env("MARKER", "PROCESS_B")
            .spawn()
            .expect("failed to spawn child B");
        let pid_a = child_a.id() as i32;
        let pid_b = child_b.id() as i32;

        // Wait for each child to actually reach exec() before touching the
        // cache under test — see `wait_for_environ_marker`'s doc comment.
        // These reads go straight through `host_environ_text`, not the
        // cache, so they can't influence what the cache under test observes.
        wait_for_environ_marker(pid_a, "MARKER=PROCESS_A");
        wait_for_environ_marker(pid_b, "MARKER=PROCESS_B");

        let cache = HostProbeCache::new();
        // Interleaved on purpose: A, then B, then A again — a single-slot
        // cache bug would have B's read clobber A's, so re-reading A must
        // still show A's marker, not B's.
        let a1 = cache.environ_text(pid_a);
        let b1 = cache.environ_text(pid_b);
        let a2 = cache.environ_text(pid_a);

        child_a.kill().ok();
        child_b.kill().ok();
        child_a.wait().ok();
        child_b.wait().ok();

        assert!(a1.contains("MARKER=PROCESS_A"));
        assert!(b1.contains("MARKER=PROCESS_B"));
        assert!(
            a2.contains("MARKER=PROCESS_A"),
            "re-reading pid_a after caching pid_b must still return pid_a's \
             own environ, not pid_b's; got: {a2:?}"
        );
    }

    /// Regression test for a PATH-injection bug: `host_exec` used to forward
    /// the *target process's own* `PATH` (read from `/proc/<pid>/environ`)
    /// onto the `sh` it spawned via `Command::new("sh")` — a bare-name
    /// lookup that resolves using whatever `PATH` ends up on the builder, so
    /// an attacker-controlled process (one that merely satisfies
    /// `parse_direct_vllm`'s detection gate, which is itself controlled by
    /// that process's own self-reported environment) could redirect the
    /// root-run monitor into executing a planted `sh` — arbitrary code
    /// execution, not just a hijacked sub-command.
    ///
    /// Spawns a real child process whose environment sets `PATH` to a
    /// directory containing a fake `sh` that would betray itself if
    /// resolved and executed in its place, then runs `host_exec` against
    /// that real pid. The fake binary must never run (note: it must fake
    /// `sh` itself, not merely a command the script body invokes — an
    /// earlier draft of this test faked `find` instead, and passed
    /// vacuously against the pre-fix code too, because with no fake `sh`
    /// present, `Command::new("sh")`'s bare-name lookup failed outright
    /// before ever reaching the script body).
    #[test]
    #[cfg(target_os = "linux")]
    fn host_exec_never_resolves_the_shell_via_the_targets_own_path() {
        let tmp = std::env::temp_dir().join(format!(
            "host_exec_path_injection_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_sh = tmp.join("sh");
        std::fs::write(&fake_sh, "#!/bin/sh\necho HIJACKED-BY-TARGET-PATH\n").unwrap();
        let mut perms = std::fs::metadata(&fake_sh).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake_sh, perms).unwrap();

        // A real process whose *own* environ carries the malicious PATH —
        // exactly what a locally-planted attacker process would look like
        // from the root-run monitor's point of view.
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .env_clear()
            .env("PATH", tmp.to_str().unwrap())
            .spawn()
            .expect("failed to spawn test child");
        let pid = child.id() as i32;
        let env_text = host_environ_text(pid);

        let output = host_exec("echo real-sh-ran", &env_text);

        child.kill().ok();
        child.wait().ok();
        std::fs::remove_dir_all(&tmp).ok();

        assert!(
            !output.contains("HIJACKED-BY-TARGET-PATH"),
            "host_exec must never resolve the `sh` interpreter itself via \
             the target process's own PATH; got: {output:?}"
        );
    }

    #[test]
    fn contains_python_scans_all_rows_not_just_top() {
        // A loading server: some other process tops CPU, python idles lower down.
        let ps = "PCPU  RSS COMMAND\n \
                  93.0 1000 tt-metal-build\n \
                  0.2 90000000 python3\n \
                  0.0 500 sh\n";
        assert!(contains_python(ps), "python below the top row must count");
        // No python anywhere → false.
        let no_py = "PCPU RSS COMMAND\n50.0 100 node\n0.1 200 bash\n";
        assert!(!contains_python(no_py));
        // Header only / empty → false, no panic.
        assert!(!contains_python("PCPU RSS COMMAND\n"));
        assert!(!contains_python(""));
    }
    #[test]
    fn parses_docker_stats_line() {
        // `docker stats --no-stream --format '{{.CPUPerc}}|{{.MemUsage}}'`
        let (cpu, rss) = parse_docker_stats("102.17%|39.96GiB / 249.3GiB").unwrap();
        assert!((cpu - 102.17).abs() < 0.01);
        assert_eq!(rss, (39.96 * 1024.0 * 1024.0 * 1024.0) as u64);
    }
    #[test]
    fn liveness_ladder() {
        assert!(matches!(parse_liveness(0, ""), Readiness::Down));
        assert!(matches!(
            parse_liveness(405, r#"{"detail":"Model is not ready"}"#),
            Readiness::NotReady
        ));
        match parse_liveness(200, r#"{"runner_in_use":"tt-z-image-turbo"}"#) {
            Readiness::Ready { runner } => assert_eq!(runner.as_deref(), Some("tt-z-image-turbo")),
            _ => panic!("expected Ready"),
        }
    }
    #[test]
    fn parses_env_and_counts() {
        assert_eq!(
            parse_env_var("HOME=/root\nTT_METAL_HOME=/x/tt-metal\n", "TT_METAL_HOME").as_deref(),
            Some("/x/tt-metal")
        );
        assert_eq!(count_lines("a\nb\nc\n"), 3);
        assert_eq!(count_lines(""), 0);
    }
    #[test]
    fn top_process_from_ps() {
        // `ps -eo pcpu,rss,comm --sort=-pcpu` (rss in KiB)
        let out = "%CPU   RSS COMMAND\n33.7 9043136 python3\n 5.2 815612 python3\n";
        let (name, cpu, rss) = top_process(out).unwrap();
        assert_eq!(name, "python3");
        assert!((cpu - 33.7).abs() < 0.01);
        assert_eq!(rss, 9043136 * 1024);
    }
    #[test]
    fn top_process_rss_saturates_and_does_not_panic() {
        let out = "%CPU   RSS COMMAND\n1.0 18446744073709551615 python3\n";
        let (name, _cpu, rss) = top_process(out).unwrap();
        assert_eq!(name, "python3");
        assert_eq!(rss, u64::MAX); // saturated, no overflow panic
    }
    #[test]
    fn parse_children_splits_whitespace_separated_pids() {
        assert_eq!(parse_children("123 456 789\n"), vec![123, 456, 789]);
        assert_eq!(parse_children(""), Vec::<i32>::new());
        assert_eq!(parse_children("  42  "), vec![42]);
        // A non-numeric token (shouldn't happen in a real /proc file, but the
        // parser must degrade rather than panic) is silently skipped.
        assert_eq!(parse_children("1 abc 3"), vec![1, 3]);
    }
    #[test]
    fn default_top_proc_cmd_is_the_docker_global_ps() {
        struct Dummy;
        impl ContainerProbe for Dummy {
            fn env(&self, _c: &str) -> String {
                String::new()
            }
            fn stats(&self, _c: &str) -> String {
                String::new()
            }
            fn exec(&self, _c: &str, _sh: &str) -> String {
                String::new()
            }
            fn http(&self, _port: u16, _path: &str) -> (u16, String) {
                (0, String::new())
            }
        }
        assert_eq!(Dummy.top_proc_cmd("anything"), TOP_PROC_CMD);
    }
    #[test]
    fn system_probe_dispatches_docker_keys_to_the_inner_docker_probe() {
        // Exercise the real dispatch in SystemProbe::top_proc_cmd: DockerProbe
        // doesn't override ContainerProbe::top_proc_cmd, so it inherits the
        // pure, no-subprocess default (TOP_PROC_CMD) — meaning we can assert
        // on both the docker and host arms without touching a real docker
        // daemon or /proc.
        let p = SystemProbe::new(DockerProbe);
        assert_eq!(p.top_proc_cmd("tt-inference-server-abc"), TOP_PROC_CMD); // docker arm
        assert!(p
            .top_proc_cmd("host-vllm-1")
            .starts_with("ps -o pcpu,rss,comm --sort=-pcpu -p ")); // host arm
        assert_ne!(p.top_proc_cmd("host-vllm-1"), TOP_PROC_CMD);

        // pid_from_key itself, still worth pinning directly.
        assert!(pid_from_key("tt-inference-server-abc123").is_none());
        assert_eq!(pid_from_key("host-vllm-4242"), Some(4242));
        assert!(pid_from_key("host-vllm-not-a-pid").is_none());
    }
    #[test]
    fn host_stats_text_matches_parse_docker_stats_shape() {
        // Calls the actual formatting helper host_stats_text uses internally
        // (not a re-typed literal), so a change to its format string would be
        // caught here, then round-trips the result through parse_docker_stats
        // the same way a real docker stats line is consumed.
        let formatted = format_host_stats(12.5, 1024 * 1024);
        let (cpu, rss) = parse_docker_stats(&formatted).expect("must parse");
        assert!((cpu - 12.5).abs() < 0.01);
        assert_eq!(rss, 1024 * 1024);
    }
}

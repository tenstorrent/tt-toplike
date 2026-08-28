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
    text.split_whitespace().filter_map(|t| t.parse().ok()).collect()
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
#[allow(dead_code)]
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
#[allow(dead_code)] // consumed starting Task 6
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
}

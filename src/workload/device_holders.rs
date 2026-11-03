// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Which process holds which Tenstorrent device, read from `/proc` at once
//! and blended with tt-smi's `processes[]` when that arrives.
//!
//! # Where the `/proc` answer comes from
//!
//! 1. `/proc/driver/tenstorrent/<N>/pids`. tt-kmd lists there every pid that
//!    holds device `N` open. The file is world readable, so it covers every
//!    user, root and processes in containers. tt-smi builds its own
//!    `processes[]` from this same file.
//! 2. `/proc/<pid>/fd/*` links that point at `/dev/tenstorrent/<N>`. Only for
//!    processes this user may read (see [`crate::workload::proc_access`]):
//!    links are read with `readlink`, nothing is opened, nothing is traced. A
//!    refused `fd/` directory is skipped and counted. This covers a driver too
//!    old to have the `pids` file.
//!
//! `N` is the driver's device number. It is mapped to the backend's device
//! index with [`DeviceMap`]: `/sys/class/tenstorrent/tenstorrent!<N>/device`
//! names the PCI address, and the backend's device with that `bus_id` gives
//! the index. That is the key the sysfs backend orders its devices by, so the
//! two agree even when the driver numbers the cards in another order. When
//! no number maps (no class directory, or a backend that knows none of the
//! addresses), each number is used as the index itself, which is what the
//! scan did before. When some numbers map, a number that does not is left
//! out, so two cards never share an index.
//!
//! # Blend rules
//!
//! [`blend`] joins the `/proc` holders with tt-smi's `processes[]`. The rules:
//!
//! * `/proc` is authoritative where it has an answer. When the scan read the
//!   driver's `pids` files ([`ProcHolders::complete`]), it saw every holder,
//!   and tt-smi, which reads the same files at another moment, adds nothing.
//! * When the scan saw only the `fd` links (no `pids` files), it could not see
//!   other users' processes. tt-smi then fills those in: a pid tt-smi lists
//!   and `/proc` did not see is added with tt-smi's device and command line.
//! * A process seen by both is a single holder. Its devices are those `/proc`
//!   saw; tt-smi only fills a name `/proc` did not have.
//! * Nothing `/proc` reported is removed because tt-smi arrived or does not
//!   list it. A holder leaves when a later `/proc` scan no longer sees it.
//! * Each holder says where its devices came from ([`HolderSource`]), so a
//!   reader can keep the provenance of the answer.
//!
//! tt-smi's own `tt-smi -s` run holds every device while it reads them. That
//! is a monitor reading the chips, so [`opened_pairs`] leaves it out of the
//! process events the Wafer Waffler turns into camera cuts.

use std::collections::BTreeMap;

use crate::models::DeviceProcess;

/// Where a holder's device list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderSource {
    /// The `/proc` scan (driver `pids` files or `fd` links).
    Proc,
    /// tt-smi's `processes[]`, for a pid `/proc` could not see.
    TtSmi,
}

/// One process holding one or more devices open.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub pid: i64,
    /// Backend device indices, sorted, without repeats.
    pub devices: Vec<usize>,
    /// Process name (`comm`), or tt-smi's command line when `/proc` had none.
    pub name: String,
    /// Command line, when known. Empty otherwise. It may be cut for display.
    pub cmdline: String,
    /// A tt-smi snapshot run, decided on the full command line before any
    /// cut ([`is_snapshot_tool`]).
    pub snapshot_tool: bool,
    pub source: HolderSource,
}

/// What one `/proc` scan found, ready to blend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcHolders {
    /// Every holder the scan saw, by pid.
    pub holders: Vec<Holder>,
    /// True when the scan read the driver's `pids` files and so saw every
    /// holder on the box, whoever owns it.
    pub complete: bool,
    /// The device number to index map the scan used.
    pub map: DeviceMap,
}

/// Maps the driver's device number (`/dev/tenstorrent/<N>`) to the
/// backend's device index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceMap {
    by_number: BTreeMap<u32, usize>,
}

impl DeviceMap {
    /// A map that uses each device number as the index.
    pub fn identity() -> Self {
        Self::default()
    }

    /// Build the map from the driver's numbers and their PCI addresses
    /// (`number_to_bus`, from [`read_number_bus`]) and the backend's devices
    /// as `(bus_id, index)` pairs. Bus ids are compared trimmed and in lower
    /// case, as the Hybrid backend joins tt-smi to sysfs.
    pub fn from_parts(number_to_bus: &BTreeMap<u32, String>, devices: &[(String, usize)]) -> Self {
        let by_bus: BTreeMap<String, usize> = devices
            .iter()
            .map(|(b, i)| (normalize_bus(b), *i))
            .filter(|(b, _)| !b.is_empty())
            .collect();
        let by_number = number_to_bus
            .iter()
            .filter_map(|(n, bus)| by_bus.get(&normalize_bus(bus)).map(|i| (*n, *i)))
            .collect();
        Self { by_number }
    }

    /// The backend index for device number `n`. An empty map (no class
    /// directory, or a backend that knows none of the addresses) uses `n`
    /// itself. A map that names some numbers gives `None` for the others: `n`
    /// could be another card's mapped index, and two cards must not share one.
    pub fn index_for(&self, n: u32) -> Option<usize> {
        if self.by_number.is_empty() {
            return Some(n as usize);
        }
        self.by_number.get(&n).copied()
    }

    /// Map a list of device numbers to sorted, unique indices, leaving out
    /// numbers [`Self::index_for`] cannot place.
    pub fn indices_for(&self, numbers: &[u32]) -> Vec<usize> {
        let mut v: Vec<usize> = numbers.iter().filter_map(|&n| self.index_for(n)).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn normalize_bus(b: &str) -> String {
    b.trim().to_lowercase()
}

/// The device number in a `/dev/tenstorrent/<N>` link target, if it is one.
pub fn number_from_link(target: &std::path::Path) -> Option<u32> {
    target
        .to_str()?
        .strip_prefix("/dev/tenstorrent/")?
        .parse()
        .ok()
}

/// True for a tt-smi snapshot run (`tt-smi -s`, `tt-smi --snapshot`): a
/// monitor that holds every device while it reads them.
pub fn is_snapshot_tool(name: &str, cmdline: &str) -> bool {
    let named = name == "tt-smi"
        || cmdline
            .split_whitespace()
            .take(3)
            .any(|w| w.rsplit('/').next() == Some("tt-smi"));
    named
        && (cmdline.split_whitespace().any(|w| w == "-s" || w == "--snapshot")
            // `comm` alone, with no command line: a tt-smi we cannot read.
            || cmdline.is_empty())
}

/// Join the `/proc` holders with tt-smi's `processes[]` (see the module doc
/// for the rules). tt-smi names devices by driver number; `proc.map` maps
/// them. The result is sorted by pid.
pub fn blend(proc: &ProcHolders, tt_smi: &[DeviceProcess]) -> Vec<Holder> {
    let mut by_pid: BTreeMap<i64, Holder> =
        proc.holders.iter().map(|h| (h.pid, h.clone())).collect();
    // tt-smi entries, grouped by pid.
    let mut smi: BTreeMap<i64, (Vec<u32>, String)> = BTreeMap::new();
    for p in tt_smi {
        let (Some(pid), Some(dev)) = (p.pid, p.device) else {
            continue;
        };
        let Ok(dev) = u32::try_from(dev) else {
            continue;
        };
        let e = smi
            .entry(pid)
            .or_insert_with(|| (Vec::new(), p.cmdline.clone().unwrap_or_default()));
        e.0.push(dev);
    }
    for (pid, (numbers, cmdline)) in smi {
        match by_pid.get_mut(&pid) {
            // Seen by both: one holder, `/proc`'s devices. tt-smi only fills
            // text `/proc` did not have.
            Some(h) => {
                h.snapshot_tool |= is_snapshot_tool(&h.name, &cmdline);
                if h.cmdline.is_empty() {
                    h.cmdline = cmdline.clone();
                }
                if h.name.is_empty() {
                    h.name = cmdline;
                }
            }
            // `/proc` saw every holder: tt-smi adds nothing.
            None if proc.complete => {}
            // `/proc` could not see this pid: tt-smi fills it in.
            None => {
                by_pid.insert(
                    pid,
                    Holder {
                        pid,
                        devices: proc.map.indices_for(&numbers),
                        name: cmdline.clone(),
                        snapshot_tool: is_snapshot_tool("", &cmdline),
                        cmdline,
                        source: HolderSource::TtSmi,
                    },
                );
            }
        }
    }
    by_pid.into_values().collect()
}

/// `(pid, device index)` pairs for the Wafer Waffler's process events,
/// leaving out tt-smi snapshot runs.
pub fn opened_pairs(holders: &[Holder]) -> Vec<(i64, usize)> {
    holders
        .iter()
        .filter(|h| !h.snapshot_tool)
        .flat_map(|h| h.devices.iter().map(move |&d| (h.pid, d)))
        .collect()
}

// ── Linux readers ────────────────────────────────────────────────────────────

/// Most pids read from one driver `pids` file.
#[cfg(target_os = "linux")]
pub const MAX_PIDS_PER_DEVICE: usize = 4096;

/// Most `fd` links read for one process. A process with more is read up to
/// the bound and counted in [`ScanCounts::fd_truncated`].
#[cfg(target_os = "linux")]
pub const MAX_FDS_PER_PROCESS: usize = 16384;

/// Most processes one scan looks at. The rest are counted in
/// [`ScanCounts::proc_truncated`] and not read.
#[cfg(target_os = "linux")]
pub const MAX_PROCESSES: usize = 32768;

/// Most bytes read from one driver `pids` file (64 KiB holds over 8,000
/// pids). A line the bound cuts is dropped.
#[cfg(target_os = "linux")]
pub const MAX_PIDS_FILE_BYTES: usize = 64 * 1024;

/// The scan's bounds. [`Limits::default`] gives the constants above; tests
/// make them small.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub processes: usize,
    pub fds_per_process: usize,
    pub pids_per_device: usize,
    pub pids_file_bytes: usize,
}

#[cfg(target_os = "linux")]
impl Default for Limits {
    fn default() -> Self {
        Limits {
            processes: MAX_PROCESSES,
            fds_per_process: MAX_FDS_PER_PROCESS,
            pids_per_device: MAX_PIDS_PER_DEVICE,
            pids_file_bytes: MAX_PIDS_FILE_BYTES,
        }
    }
}

/// Read at most `limit` bytes of `path` as text. When the file is longer,
/// the last, cut line is dropped.
#[cfg(target_os = "linux")]
fn read_bounded(path: &std::path::Path, limit: usize) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > limit {
        buf.truncate(limit);
        let keep = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(keep);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The driver's device numbers and the PCI address of each, from
/// `<sys_root>/class/tenstorrent/tenstorrent!<N>/device`. Empty when the
/// driver has no class directory.
#[cfg(target_os = "linux")]
pub fn read_number_bus(sys_root: &std::path::Path) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(sys_root.join("class/tenstorrent")) else {
        return out;
    };
    for e in dir.flatten() {
        let name = e.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|s| s.strip_prefix("tenstorrent!"))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(link) = std::fs::read_link(e.path().join("device")) else {
            continue;
        };
        // The last path component is the card's PCI address.
        if let Some(bus) = link.file_name().and_then(|s| s.to_str()) {
            out.insert(n, bus.to_string());
        }
    }
    out
}

/// The driver's holder lists: pid to the device numbers it holds, from
/// `<proc_root>/driver/tenstorrent/<N>/pids`. `None` when the driver has no
/// such directory (an older tt-kmd), so the caller knows the answer is not
/// complete. At most [`MAX_PIDS_PER_DEVICE`] pids and
/// [`MAX_PIDS_FILE_BYTES`] bytes are read per device.
#[cfg(target_os = "linux")]
pub fn read_driver_pids(proc_root: &std::path::Path) -> Option<BTreeMap<i32, Vec<u32>>> {
    read_driver_pids_with(proc_root, &Limits::default())
}

/// [`read_driver_pids`] with the given bounds.
#[cfg(target_os = "linux")]
pub fn read_driver_pids_with(
    proc_root: &std::path::Path,
    limits: &Limits,
) -> Option<BTreeMap<i32, Vec<u32>>> {
    let dir = std::fs::read_dir(proc_root.join("driver/tenstorrent")).ok()?;
    let mut out: BTreeMap<i32, Vec<u32>> = BTreeMap::new();
    let mut any = false;
    for e in dir.flatten() {
        let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(text) = read_bounded(&e.path().join("pids"), limits.pids_file_bytes) else {
            continue;
        };
        any = true;
        for pid in text
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse::<i32>().ok())
            .take(limits.pids_per_device)
        {
            let v = out.entry(pid).or_default();
            if !v.contains(&n) {
                v.push(n);
            }
        }
    }
    // `read_dir` order is arbitrary; keep each list in number order.
    for v in out.values_mut() {
        v.sort_unstable();
    }
    any.then_some(out)
}

/// What reading one process's `fd/` directory gave.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FdRead {
    /// The device numbers its links point at (possibly none).
    Numbers(Vec<u32>),
    /// The directory was refused.
    Denied,
    /// The process is gone (it exited during the scan).
    Gone,
}

/// Read the links in `fd_dir` and collect the `/dev/tenstorrent/<N>` numbers
/// they point at. Reads at most [`MAX_FDS_PER_PROCESS`] links; the second
/// value is true when there were more. A link that vanishes while it is read
/// (the process closed it) is skipped.
#[cfg(target_os = "linux")]
pub fn fd_numbers(fd_dir: &std::path::Path) -> (FdRead, bool) {
    fd_numbers_with(fd_dir, MAX_FDS_PER_PROCESS)
}

/// [`fd_numbers`], reading at most `max_fds` links.
#[cfg(target_os = "linux")]
pub fn fd_numbers_with(fd_dir: &std::path::Path, max_fds: usize) -> (FdRead, bool) {
    let dir = match std::fs::read_dir(fd_dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return (FdRead::Denied, false)
        }
        Err(_) => return (FdRead::Gone, false),
    };
    let mut numbers = Vec::new();
    let mut seen = 0usize;
    let mut truncated = false;
    for e in dir.flatten() {
        if seen >= max_fds {
            truncated = true;
            break;
        }
        seen += 1;
        if let Some(n) = std::fs::read_link(e.path())
            .ok()
            .and_then(|t| number_from_link(&t))
        {
            if !numbers.contains(&n) {
                numbers.push(n);
            }
        }
    }
    numbers.sort_unstable();
    (FdRead::Numbers(numbers), truncated)
}

/// Counters for one `/proc` scan, kept so a partial answer says why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanCounts {
    /// Processes owned by another user whose `fd/` was not attempted.
    pub foreign: usize,
    /// Processes whose `fd/` was refused although we own them.
    pub denied: usize,
    /// Processes that exited while the scan read them.
    pub vanished: usize,
    /// Processes with more `fd` links than the scan reads.
    pub fd_truncated: usize,
    /// Processes left unread because the scan reached its process bound.
    pub proc_truncated: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(pid: i64, devices: &[usize], name: &str) -> Holder {
        Holder {
            pid,
            devices: devices.to_vec(),
            name: name.into(),
            cmdline: String::new(),
            snapshot_tool: false,
            source: HolderSource::Proc,
        }
    }

    fn smi(pid: i64, device: usize, cmdline: &str) -> DeviceProcess {
        DeviceProcess {
            pid: Some(pid),
            user: Some("root".into()),
            device: Some(device),
            cmdline: Some(cmdline.into()),
        }
    }

    fn bus_map() -> DeviceMap {
        // The driver numbers the cards 0..3 in one order; the backend sorts
        // them by PCI address in another.
        let numbers: BTreeMap<u32, String> = [
            (0, "0000:04:00.0"),
            (1, "0000:01:00.0"),
            (2, "0000:03:00.0"),
            (3, "0000:02:00.0"),
        ]
        .into_iter()
        .map(|(n, b)| (n, b.to_string()))
        .collect();
        let devices = vec![
            ("0000:01:00.0".to_string(), 0),
            ("0000:02:00.0".to_string(), 1),
            ("0000:03:00.0".to_string(), 2),
            ("0000:04:00.0".to_string(), 3),
        ];
        DeviceMap::from_parts(&numbers, &devices)
    }

    #[test]
    fn device_numbers_map_through_the_pci_address() {
        let m = bus_map();
        assert_eq!(m.index_for(0), Some(3));
        assert_eq!(m.index_for(1), Some(0));
        assert_eq!(m.indices_for(&[2, 0, 0]), vec![2, 3]);
        // With no map at all a number is its own index.
        assert_eq!(DeviceMap::identity().index_for(2), Some(2));
    }

    /// A partial map never puts two cards on one index: number 0 maps to
    /// index 3, and number 3, which the class directory does not name, is
    /// left out. Mapped to itself, it would land on 3 as well.
    #[test]
    fn an_unmapped_number_never_takes_a_mapped_index() {
        let numbers: BTreeMap<u32, String> = [(0, "0000:04:00.0".to_string())].into();
        let m = DeviceMap::from_parts(&numbers, &[("0000:04:00.0".into(), 3)]);
        assert_eq!(m.index_for(0), Some(3));
        assert_eq!(m.index_for(3), None);
        assert_eq!(m.indices_for(&[0, 3]), vec![3]);
    }

    #[test]
    fn upper_case_bus_ids_still_match() {
        let numbers: BTreeMap<u32, String> = [(0, "0000:0A:00.0".to_string())].into();
        let m = DeviceMap::from_parts(&numbers, &[(" 0000:0a:00.0".into(), 5)]);
        assert_eq!(m.index_for(0), Some(5));
    }

    #[test]
    fn link_targets_parse_only_for_device_nodes() {
        use std::path::Path;
        assert_eq!(number_from_link(Path::new("/dev/tenstorrent/3")), Some(3));
        assert_eq!(
            number_from_link(Path::new("/dev/tenstorrent/by-id/x")),
            None
        );
        assert_eq!(number_from_link(Path::new("/dev/null")), None);
        assert_eq!(number_from_link(Path::new("socket:[123]")), None);
    }

    #[test]
    fn snapshot_runs_are_recognised() {
        assert!(is_snapshot_tool(
            "tt-smi",
            "/venv/bin/python /home/u/.local/bin/tt-smi -s"
        ));
        assert!(is_snapshot_tool(
            "python3",
            "python3 /usr/bin/tt-smi --snapshot"
        ));
        assert!(is_snapshot_tool("tt-smi", ""));
        // The interactive tt-smi and a reset are not snapshot runs.
        assert!(!is_snapshot_tool("tt-smi", "tt-smi"));
        assert!(!is_snapshot_tool("tt-smi", "tt-smi -r 0"));
        assert!(!is_snapshot_tool("VLLM::EngineCor", "VLLM::EngineCore"));
    }

    /// tt-smi arriving changes nothing `/proc` already showed: same holders,
    /// same devices, one row per pid.
    #[test]
    fn a_complete_proc_answer_is_not_changed_by_tt_smi() {
        let proc = ProcHolders {
            holders: vec![holder(10, &[0, 1], "vllm"), holder(11, &[2], "train")],
            complete: true,
            map: DeviceMap::identity(),
        };
        let smi_list = vec![
            smi(10, 0, "vllm serve"),
            smi(10, 1, "vllm serve"),
            // A pid that has exited since tt-smi read the file.
            smi(99, 3, "old job"),
        ];
        let out = blend(&proc, &smi_list);
        assert_eq!(out.iter().map(|h| h.pid).collect::<Vec<_>>(), vec![10, 11]);
        assert_eq!(out[0].devices, vec![0, 1]);
        assert_eq!(out[0].source, HolderSource::Proc);
        assert_eq!(out[1].devices, vec![2], "a pid tt-smi does not list stays");
    }

    /// Without the driver lists `/proc` could not see other users' processes;
    /// tt-smi fills those in, through the same device map.
    #[test]
    fn tt_smi_fills_what_an_fd_only_scan_could_not_see() {
        let proc = ProcHolders {
            holders: vec![holder(10, &[0], "mine")],
            complete: false,
            map: bus_map(),
        };
        let smi_list = vec![
            smi(10, 1, "mine --flag"),
            smi(20, 0, "python serve.py"),
            smi(20, 2, "python serve.py"),
        ];
        let out = blend(&proc, &smi_list);
        assert_eq!(out.len(), 2, "one row per pid");
        assert_eq!(out[0].devices, vec![0], "/proc's devices stand");
        assert_eq!(
            out[0].cmdline, "mine --flag",
            "tt-smi fills a missing command line"
        );
        assert_eq!(out[1].pid, 20);
        assert_eq!(out[1].devices, vec![2, 3], "numbers 0 and 2 map to 3 and 2");
        assert_eq!(out[1].source, HolderSource::TtSmi);
        assert_eq!(out[1].name, "python serve.py");
    }

    /// Before tt-smi answers, the blend is the `/proc` answer as it is.
    #[test]
    fn with_no_tt_smi_the_proc_answer_passes_through() {
        let proc = ProcHolders {
            holders: vec![holder(10, &[0], "vllm")],
            complete: false,
            map: DeviceMap::identity(),
        };
        assert_eq!(blend(&proc, &[]), proc.holders);
    }

    #[test]
    fn opened_pairs_leave_out_snapshot_runs() {
        let mut t = holder(30, &[0, 1, 2, 3], "tt-smi");
        t.cmdline = "python /usr/bin/tt-smi -s".into();
        t.snapshot_tool = true;
        let pairs = opened_pairs(&[holder(10, &[0, 1], "vllm"), t]);
        assert_eq!(pairs, vec![(10, 0), (10, 1)]);
    }

    /// tt-smi's own row for its snapshot run, with a long interpreter path,
    /// is marked as a snapshot run and makes no pair.
    #[test]
    fn a_tt_smi_row_for_its_own_long_snapshot_run_makes_no_pair() {
        let proc = ProcHolders::default();
        let cmd = "/home/ttuser/.tenstorrent-venv/bin/python /home/ttuser/.local/bin/tt-smi -s";
        let out = blend(&proc, &[smi(77, 0, cmd), smi(77, 1, cmd)]);
        assert!(out[0].snapshot_tool);
        assert!(opened_pairs(&out).is_empty());
    }

    #[test]
    fn malformed_tt_smi_rows_are_ignored() {
        let proc = ProcHolders::default();
        let rows = vec![
            DeviceProcess {
                pid: None,
                device: Some(0),
                ..Default::default()
            },
            DeviceProcess {
                pid: Some(5),
                device: None,
                ..Default::default()
            },
        ];
        assert!(blend(&proc, &rows).is_empty());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn driver_pids_are_read_per_device() {
        let td = tempfile::tempdir().unwrap();
        let base = td.path().join("driver/tenstorrent");
        for (n, pids) in [("0", "10\n11\n"), ("1", "10\n"), ("2", "")] {
            fs::create_dir_all(base.join(n)).unwrap();
            fs::write(base.join(n).join("pids"), pids).unwrap();
        }
        // Not a device directory.
        fs::create_dir_all(base.join("by-id")).unwrap();
        let got = read_driver_pids(td.path()).expect("the driver lists exist");
        assert_eq!(got.get(&10), Some(&vec![0, 1]));
        assert_eq!(got.get(&11), Some(&vec![0]));
        assert_eq!(got.len(), 2);
    }

    fn write_pids(root: &std::path::Path, n: u32, text: &str) {
        let d = root.join("driver/tenstorrent").join(n.to_string());
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("pids"), text).unwrap();
    }

    #[test]
    fn at_most_the_bound_of_pids_is_read_per_device() {
        let td = tempfile::tempdir().unwrap();
        let text: String = (100..110).map(|p| format!("{p}\n")).collect();
        write_pids(td.path(), 0, &text);
        let limits = Limits {
            pids_per_device: 4,
            ..Limits::default()
        };
        let got = read_driver_pids_with(td.path(), &limits).unwrap();
        assert_eq!(
            got.keys().copied().collect::<Vec<_>>(),
            vec![100, 101, 102, 103]
        );
    }

    /// A `pids` file longer than the byte bound is read up to the bound, and
    /// the line the bound cuts is dropped, so no pid is made from half a
    /// number.
    #[test]
    fn a_pids_file_past_the_byte_bound_is_cut_at_a_line() {
        let td = tempfile::tempdir().unwrap();
        // "123456\n" is 7 bytes; a 10-byte bound cuts the second pid.
        write_pids(td.path(), 0, "123456\n654321\n777777\n");
        let limits = Limits {
            pids_file_bytes: 10,
            ..Limits::default()
        };
        let got = read_driver_pids_with(td.path(), &limits).unwrap();
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), vec![123456]);
    }

    #[test]
    fn at_most_the_bound_of_fd_links_is_read() {
        let td = tempfile::tempdir().unwrap();
        let fd = td.path().join("fd");
        fs::create_dir_all(&fd).unwrap();
        for i in 0..5 {
            symlink(format!("/dev/tenstorrent/{i}"), fd.join(i.to_string())).unwrap();
        }
        let (got, truncated) = fd_numbers_with(&fd, 3);
        let FdRead::Numbers(n) = got else {
            panic!("{got:?}")
        };
        assert_eq!(n.len(), 3, "three links read");
        assert!(truncated);
        assert_eq!(
            fd_numbers_with(&fd, 5),
            (FdRead::Numbers(vec![0, 1, 2, 3, 4]), false)
        );
    }

    #[test]
    fn no_driver_directory_means_no_complete_answer() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(read_driver_pids(td.path()), None);
    }

    #[test]
    fn class_directory_names_each_number_s_pci_address() {
        let td = tempfile::tempdir().unwrap();
        let class = td.path().join("class/tenstorrent");
        fs::create_dir_all(&class).unwrap();
        for (n, bus) in [(0, "0000:04:00.0"), (1, "0000:01:00.0")] {
            let d = class.join(format!("tenstorrent!{n}"));
            fs::create_dir_all(&d).unwrap();
            symlink(
                format!("../../../devices/pci0000:00/0000:00:01.1/{bus}"),
                d.join("device"),
            )
            .unwrap();
        }
        let got = read_number_bus(td.path());
        assert_eq!(got.get(&0).map(String::as_str), Some("0000:04:00.0"));
        assert_eq!(got.get(&1).map(String::as_str), Some("0000:01:00.0"));
    }

    #[test]
    fn fd_links_to_device_nodes_are_collected() {
        let td = tempfile::tempdir().unwrap();
        let fd = td.path().join("fd");
        fs::create_dir_all(&fd).unwrap();
        symlink("/dev/tenstorrent/2", fd.join("3")).unwrap();
        symlink("/dev/tenstorrent/0", fd.join("4")).unwrap();
        symlink("/dev/tenstorrent/2", fd.join("5")).unwrap();
        symlink("/dev/null", fd.join("0")).unwrap();
        symlink("socket:[1234]", fd.join("1")).unwrap();
        assert_eq!(fd_numbers(&fd), (FdRead::Numbers(vec![0, 2]), false));
    }

    #[test]
    fn a_vanished_process_is_gone() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(fd_numbers(&td.path().join("no-such/fd")).0, FdRead::Gone);
    }

    #[test]
    fn a_refused_fd_directory_is_denied() {
        let td = tempfile::tempdir().unwrap();
        let fd = td.path().join("fd");
        fs::create_dir_all(&fd).unwrap();
        fs::set_permissions(&fd, fs::Permissions::from_mode(0o000)).unwrap();
        let got = fd_numbers(&fd).0;
        fs::set_permissions(&fd, fs::Permissions::from_mode(0o700)).unwrap();
        // root reads any directory; the refusal exists only for other users.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert_eq!(got, FdRead::Denied);
    }
}

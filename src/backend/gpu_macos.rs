// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Apple GPU statistics via `ioreg` (no sudo, no unsafe).
//!
//! Reads the `IOAccelerator` node's `PerformanceStatistics` dictionary plus the
//! GPU `model` and `gpu-core-count`. Used by `HostBackend` to add a GPU device
//! on macOS so Metal/MPS workloads appear in the visualizations.

use std::process::Command;

/// One sample of Apple GPU stats.
#[derive(Debug, Clone)]
pub struct GpuSample {
    pub model: String,
    pub core_count: usize,
    pub util_pct: f32,
    pub mem_in_use_bytes: u64,
    pub mem_alloc_bytes: u64,
}

/// Extract a quoted string value: `"<key>" = "value"`.
fn find_quoted(output: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = \"");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract an unquoted integer value: `"<key>" = 20`.
fn find_int_assign(output: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\" = ");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Extract a dict-style integer value inside PerformanceStatistics: `"<key>"=123`.
fn find_dict_int(output: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"=");
    let start = output.find(&needle)? + needle.len();
    let rest = &output[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Isolate the single `ioreg` node block that actually reports GPU utilization.
///
/// `ioreg -r` prints one block per matching node, each introduced by the `+-o `
/// tree marker. A Mac with more than one `IOAccelerator` (e.g. an Intel Mac with
/// integrated + discrete GPUs) would otherwise let a global first-match splice
/// `model` from one node with `Device Utilization %` from another. We scope to
/// the block containing the utilization field so every field comes from the
/// same GPU. When there's no marker (a single block, or a trimmed test fixture)
/// the whole input is used if it carries the field.
fn node_with_utilization(output: &str) -> Option<&str> {
    const MARKER: &str = "+-o ";
    const UTIL: &str = "Device Utilization %";
    if !output.contains(MARKER) {
        return output.contains(UTIL).then_some(output);
    }
    output.split(MARKER).find(|block| block.contains(UTIL))
}

/// Parse `ioreg -rc IOAccelerator` output into a [`GpuSample`].
///
/// Returns `None` if the GPU utilization field is absent (not an Apple GPU node
/// or unexpected format) so the caller can skip adding a GPU device.
pub fn parse_ioreg(output: &str) -> Option<GpuSample> {
    // Scope all field lookups to one node so a second IOAccelerator can't splice
    // its values into this sample.
    let node = node_with_utilization(output)?;
    let util_pct = find_dict_int(node, "Device Utilization %")? as f32;
    Some(GpuSample {
        model: find_quoted(node, "model").unwrap_or_else(|| "Apple GPU".to_string()),
        core_count: find_int_assign(node, "gpu-core-count").unwrap_or(0) as usize,
        util_pct,
        mem_in_use_bytes: find_dict_int(node, "In use system memory").unwrap_or(0),
        mem_alloc_bytes: find_dict_int(node, "Alloc system memory").unwrap_or(0),
    })
}

/// Spawn `ioreg -r -c IOAccelerator` and parse one GPU sample. `None` if `ioreg` is missing/errors.
pub fn sample() -> Option<GpuSample> {
    let out = Command::new("ioreg")
        .args(["-r", "-c", "IOAccelerator"])
        .output()
        .ok()?;
    parse_ioreg(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from `ioreg -rc IOAccelerator` on an Apple M4 Pro (trimmed).
    const FIXTURE: &str = r#"
  | |   "model" = "Apple M4 Pro"
  | |   "gpu-core-count" = 20
  | |   "PerformanceStatistics" = {"In use system memory (driver)"=0,"Alloc system memory"=5043453952,"Tiler Utilization %"=13,"Renderer Utilization %"=24,"Device Utilization %"=27,"In use system memory"=650788864}
"#;

    #[test]
    fn parses_gpu_sample_from_ioreg() {
        let s = parse_ioreg(FIXTURE).expect("should parse");
        assert_eq!(s.model, "Apple M4 Pro");
        assert_eq!(s.core_count, 20);
        assert_eq!(s.util_pct, 27.0);
        assert_eq!(s.mem_in_use_bytes, 650_788_864);
        assert_eq!(s.mem_alloc_bytes, 5_043_453_952);
    }

    #[test]
    fn returns_none_on_garbage() {
        assert!(parse_ioreg("no gpu here").is_none());
    }

    // Two IOAccelerator nodes: a first one WITHOUT utilization (wrong GPU) and
    // the real Apple GPU node with it. We must read every field from the node
    // that reports utilization, never splice the first node's model in.
    const MULTI_NODE: &str = r#"
  +-o IOAccelerator@0  <class IOAccelerator>
  | |   "model" = "Intel HD Graphics"
  | |   "gpu-core-count" = 8
  +-o IOAccelerator@1  <class IOAccelerator>
  | |   "model" = "Apple M4 Pro"
  | |   "gpu-core-count" = 20
  | |   "PerformanceStatistics" = {"Alloc system memory"=5043453952,"Device Utilization %"=27,"In use system memory"=650788864}
"#;

    #[test]
    fn multi_accelerator_reads_from_the_utilization_node() {
        let s = parse_ioreg(MULTI_NODE).expect("should parse the util-bearing node");
        assert_eq!(
            s.model, "Apple M4 Pro",
            "must not splice the other node's model"
        );
        assert_eq!(s.core_count, 20);
        assert_eq!(s.util_pct, 27.0);
        assert_eq!(s.mem_in_use_bytes, 650_788_864);
    }
}

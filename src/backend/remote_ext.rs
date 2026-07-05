// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! `tt_toplike` frame extension — processes + inference status riding along on
//! the `/telemetry` publisher stream.
//!
//! ## Why this exists
//!
//! `--serve` publishes `tt-smi -s` JSON frames verbatim (see
//! [`crate::backend::ws`]) so that any `tt-smi`-compatible consumer keeps
//! working unmodified. But a `tt-toplike --remote` client wants more than
//! device telemetry: it wants the box's process list and any inference-server
//! status, exactly like the local TUI shows. Rather than invent a second
//! stream/schema, this module bolts an **additive** top-level key —
//! `tt_toplike` — onto the existing JSON object. `tt-smi` and any other plain
//! `tt-smi -s` consumer ignores keys it doesn't know about, so this key is
//! free: the frame is still valid `tt-smi -s` JSON before and after injection
//! (verified in tests via [`crate::backend::json::parse_tt_smi_snapshot`]).
//!
//! ## Shape
//!
//! ```text
//! { ...tt-smi fields..., "tt_toplike": { "schema": 1, "processes": [...], "inference": [...] } }
//! ```
//!
//! `schema` lets a future incompatible revision of this extension be
//! recognized and skipped by older readers instead of misparsed:
//! [`parse_extension`] returns `None` for any schema other than the one this
//! build understands ([`TT_TOPLIKE_SCHEMA`]).
//!
//! ## Design notes
//!
//! - Pure module: no I/O, no threads, no sockets. Callers own transport.
//! - Panic-free: malformed input (bad JSON, wrong types, unknown schema)
//!   degrades to `None` / passthrough rather than panicking, since the input
//!   here is attacker-adjacent (a remote box's frame, or a stale client
//!   talking to a newer server).
//! - `RemoteServing` mirrors the *display* fields of
//!   [`crate::workload::inference_server::metrics::ServingStats`] — the
//!   numbers the local UI already renders — rather than the full internal
//!   struct (which also carries raw counters used only to compute deltas
//!   locally).

use serde::{Deserialize, Serialize};

/// Schema version for the `tt_toplike` extension key. Bump this whenever the
/// shape of [`TtToplikeExt`] (or its children) changes incompatibly; readers
/// reject any frame whose `schema` doesn't match theirs (see [`parse_extension`]).
pub const TT_TOPLIKE_SCHEMA: u32 = 1;

/// The `tt_toplike` extension payload: everything a `--remote` client needs
/// beyond raw device telemetry to render the box like a local one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TtToplikeExt {
    /// Schema version this payload was produced with. See [`TT_TOPLIKE_SCHEMA`].
    pub schema: u32,
    /// Processes on the remote box (host process list, TT-using or not).
    ///
    /// `None` means "this producer does not stream processes" → the consumer
    /// falls back to its LOCAL process list (and labels it). `Some(vec![])`
    /// means "streamed, and there are none" — an authoritative empty. This
    /// distinction is the wire contract: a producer that only knows about some
    /// sub-keys (e.g. tt-station-agentd emitting processes but not inference)
    /// omits the others entirely rather than sending an empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<RemoteProc>>,
    /// Inference workloads detected on the remote box. Same `None` (not
    /// streamed → local fallback) vs `Some(vec![])` (streamed, none) contract
    /// as [`Self::processes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<Vec<RemoteInference>>,
}

/// One process entry, as shown in the local process panel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteProc {
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    /// Whether this process is believed to be using Tenstorrent hardware.
    pub uses_tt: bool,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

/// One inference workload's status, as shown in the local inference panel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteInference {
    /// Stable identifier for this workload (e.g. container/service name).
    pub key: String,
    /// Human-readable label for display.
    pub label: String,
    /// Lifecycle phase: "down" | "compiling" | "loading" | "ready" | "alarm".
    pub phase: String,
    /// Compile/load progress in `[0.0, 1.0]`, if known (`None` outside those phases).
    pub progress: Option<f32>,
    /// Live serving metrics, present only once the workload is actually serving.
    pub serving: Option<RemoteServing>,
}

/// Serving metrics mirrored from the *display* fields of
/// `crate::workload::inference_server::metrics::ServingStats` — deliberately
/// excludes the raw counters (`VllmCounters`) that struct also carries, since
/// those exist only to compute local deltas and aren't meaningful to re-derive
/// remotely.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteServing {
    pub generation_tps: f32,
    pub prompt_tps: f32,
    pub requests_running: u32,
    pub requests_waiting: u32,
    pub kv_cache_usage: f32,
    pub ttft_avg_s: f32,
    pub queue_avg_s: f32,
    pub prefill_avg_s: f32,
    pub decode_avg_s: f32,
    pub tpot_avg_s: f32,
    pub completed_delta: u32,
    pub errored_delta: u32,
    pub prefix_hit_rate: f32,
    pub preemptions_delta: u32,
}

/// Build a [`TtToplikeExt`] payload from this box's local process list and
/// inference-server snapshot, ready to hand to [`inject_extension`].
///
/// Unlike a partial producer (e.g. tt-station-agentd, which may stream only
/// `processes`), tt-toplike itself always knows both — so both sub-keys come
/// back `Some(...)`, even when empty, per the wire contract documented on
/// [`TtToplikeExt::processes`]/[`TtToplikeExt::inference`].
pub fn build_extension(
    procs: &[crate::workload::host_processes::ProcRow],
    inference: &[crate::workload::inference_server::ServiceState],
) -> TtToplikeExt {
    TtToplikeExt {
        schema: TT_TOPLIKE_SCHEMA,
        processes: Some(procs.iter().map(remote_proc_from_row).collect()),
        inference: Some(inference.iter().map(remote_inference_from_state).collect()),
    }
}

/// Map one local [`ProcRow`](crate::workload::host_processes::ProcRow) to the
/// wire [`RemoteProc`] shape.
///
/// `ProcRow` has no separate `cmd` field (just `name`), so `cmd` is populated
/// with a clone of `name` — good enough for a remote viewer's display, which
/// is all `cmd` is used for.
fn remote_proc_from_row(row: &crate::workload::host_processes::ProcRow) -> RemoteProc {
    RemoteProc {
        // PIDs are conceptually unsigned on the wire; a negative `pid` (not
        // expected in practice, but `ProcRow::pid` is a plain `i32`) clamps to
        // 0 rather than wrapping into a huge u32 via `as` on a negative value.
        pid: row.pid.max(0) as u32,
        name: row.name.clone(),
        cmd: row.name.clone(),
        uses_tt: row.tt.is_some(),
        cpu_pct: row.cpu_pct,
        mem_bytes: row.mem_bytes,
    }
}

/// Map one local `ServiceState` to the wire [`RemoteInference`] shape.
fn remote_inference_from_state(
    state: &crate::workload::inference_server::ServiceState,
) -> RemoteInference {
    RemoteInference {
        key: state.key.clone(),
        label: state.label.clone(),
        phase: phase_str(state.phase).to_string(),
        progress: state.progress,
        serving: state.serving.as_ref().map(remote_serving_from_stats),
    }
}

/// Lowercase wire name for a [`Phase`](crate::workload::inference_server::Phase).
fn phase_str(phase: crate::workload::inference_server::Phase) -> &'static str {
    use crate::workload::inference_server::Phase;
    match phase {
        Phase::Down => "down",
        Phase::Compiling => "compiling",
        Phase::Loading => "loading",
        Phase::Ready => "ready",
        Phase::Alarm => "alarm",
    }
}

/// Map the display fields of `ServingStats` field-by-field onto the wire
/// [`RemoteServing`] shape (see the module doc for why the raw counters are
/// deliberately excluded).
fn remote_serving_from_stats(
    stats: &crate::workload::inference_server::metrics::ServingStats,
) -> RemoteServing {
    RemoteServing {
        generation_tps: stats.generation_tps,
        prompt_tps: stats.prompt_tps,
        requests_running: stats.requests_running,
        requests_waiting: stats.requests_waiting,
        kv_cache_usage: stats.kv_cache_usage,
        ttft_avg_s: stats.ttft_avg_s,
        queue_avg_s: stats.queue_avg_s,
        prefill_avg_s: stats.prefill_avg_s,
        decode_avg_s: stats.decode_avg_s,
        tpot_avg_s: stats.tpot_avg_s,
        completed_delta: stats.completed_delta,
        errored_delta: stats.errored_delta,
        prefix_hit_rate: stats.prefix_hit_rate,
        preemptions_delta: stats.preemptions_delta,
    }
}

/// The top-level JSON key the extension lives under.
const EXT_KEY: &str = "tt_toplike";

/// Insert `ext` into `tt_smi_json` under the additive `"tt_toplike"` key and
/// return the re-serialized frame.
///
/// `tt_smi_json` is expected to already be a JSON object (a `tt-smi -s`
/// snapshot); this function only adds a key to it, never touches existing
/// keys. On any parse failure (not valid JSON, or not a JSON object),
/// `tt_smi_json` is returned unchanged — this is a best-effort enrichment,
/// not a required transform, so a malformed base frame shouldn't be turned
/// into an error path here.
pub fn inject_extension(tt_smi_json: &str, ext: &TtToplikeExt) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(tt_smi_json) else {
        return tt_smi_json.to_string();
    };
    let Some(obj) = value.as_object_mut() else {
        return tt_smi_json.to_string();
    };
    let Ok(ext_value) = serde_json::to_value(ext) else {
        return tt_smi_json.to_string();
    };
    obj.insert(EXT_KEY.to_string(), ext_value);
    serde_json::to_string(&value).unwrap_or_else(|_| tt_smi_json.to_string())
}

/// Decode the `tt_toplike` key from a frame, if present and understood.
///
/// Returns `None` if:
/// - `frame` isn't valid JSON, or isn't a JSON object,
/// - the `tt_toplike` key is absent (a plain `tt-smi -s` frame),
/// - the key is present but doesn't deserialize into [`TtToplikeExt`],
/// - the key deserializes fine but `schema != TT_TOPLIKE_SCHEMA` (a
///   version this build doesn't understand — fail closed rather than
///   guess at a mismatched shape).
pub fn parse_extension(frame: &str) -> Option<TtToplikeExt> {
    let value: serde_json::Value = serde_json::from_str(frame).ok()?;
    let ext_value = value.as_object()?.get(EXT_KEY)?;
    let ext: TtToplikeExt = serde_json::from_value(ext_value.clone()).ok()?;
    if ext.schema != TT_TOPLIKE_SCHEMA {
        return None;
    }
    Some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::json::parse_tt_smi_snapshot;

    /// Minimal but real tt-smi `-s` snapshot shape (trimmed from the fixture
    /// used in `backend::json` tests) — enough for `parse_tt_smi_snapshot` to
    /// succeed on it.
    const MINIMAL_TTSMI_JSON: &str = r#"{
        "device_info": [{
            "board_info": {
                "board_type": "p300c",
                "bus_id": "0000:04:00.0",
                "coords": "N/A"
            },
            "telemetry": {
                "voltage": "0.72",
                "current": " 23.0",
                "power": " 16.0",
                "aiclk": " 800",
                "asic_temperature": "34.8",
                "fan_speed": " 38",
                "heartbeat": "11540"
            },
            "smbus_telem": {
                "BOARD_ID_HIGH": "0x461",
                "BOARD_ID_LOW": "0x31924062"
            }
        }]
    }"#;

    fn sample_ext() -> TtToplikeExt {
        TtToplikeExt {
            schema: TT_TOPLIKE_SCHEMA,
            processes: Some(vec![RemoteProc {
                pid: 4242,
                name: "tt-inference-server".to_string(),
                cmd: "/usr/bin/tt-inference-server --port 8080".to_string(),
                uses_tt: true,
                cpu_pct: 87.5,
                mem_bytes: 12_884_901_888,
            }]),
            inference: Some(vec![RemoteInference {
                key: "vllm-llama3-70b".to_string(),
                label: "Llama-3 70B (vLLM)".to_string(),
                phase: "ready".to_string(),
                progress: None,
                serving: Some(RemoteServing {
                    generation_tps: 512.3,
                    prompt_tps: 1024.7,
                    requests_running: 3,
                    requests_waiting: 1,
                    kv_cache_usage: 0.42,
                    ttft_avg_s: 0.35,
                    queue_avg_s: 0.02,
                    prefill_avg_s: 0.11,
                    decode_avg_s: 0.05,
                    tpot_avg_s: 0.03,
                    completed_delta: 27,
                    errored_delta: 0,
                    prefix_hit_rate: 0.61,
                    preemptions_delta: 2,
                }),
            }]),
        }
    }

    /// (a) inject then parse round-trips a populated `TtToplikeExt`.
    #[test]
    fn inject_then_parse_round_trips() {
        let ext = sample_ext();
        let frame = inject_extension(MINIMAL_TTSMI_JSON, &ext);
        let parsed = parse_extension(&frame).expect("extension should parse back out");
        assert_eq!(parsed, ext);
    }

    /// (b) a plain tt-smi frame (no key) → `parse_extension` == None.
    #[test]
    fn plain_frame_has_no_extension() {
        assert_eq!(parse_extension(MINIMAL_TTSMI_JSON), None);
    }

    /// (c) a frame with `tt_toplike.schema = 999` → None.
    #[test]
    fn mismatched_schema_is_rejected() {
        let ext = sample_ext();
        let frame = inject_extension(MINIMAL_TTSMI_JSON, &ext);
        let mut value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        value["tt_toplike"]["schema"] = serde_json::json!(999);
        let mutated = serde_json::to_string(&value).unwrap();
        assert_eq!(parse_extension(&mutated), None);
    }

    /// (d) `inject_extension` output still parses via
    /// `crate::backend::json::parse_tt_smi_snapshot` — the additive key must
    /// never break the base tt-smi parser plain consumers rely on.
    #[test]
    fn injected_frame_still_parses_as_tt_smi() {
        let ext = sample_ext();
        let frame = inject_extension(MINIMAL_TTSMI_JSON, &ext);
        let parsed = parse_tt_smi_snapshot(&frame).expect("tt-smi parser must still succeed");
        assert_eq!(parsed.devices.len(), 1);
    }

    /// Malformed base JSON: `inject_extension` returns it unchanged rather
    /// than panicking or fabricating a frame.
    #[test]
    fn inject_on_malformed_json_returns_unchanged() {
        let ext = sample_ext();
        let garbage = "not json at all";
        assert_eq!(inject_extension(garbage, &ext), garbage);
    }

    /// A JSON value that parses but isn't an object (e.g. a bare array) is
    /// also passed through unchanged — there's no object to insert a key into.
    #[test]
    fn inject_on_non_object_json_returns_unchanged() {
        let ext = sample_ext();
        let non_object = "[1, 2, 3]";
        assert_eq!(inject_extension(non_object, &ext), non_object);
    }

    /// `parse_extension` on non-JSON input degrades to `None`, not a panic.
    #[test]
    fn parse_on_malformed_json_returns_none() {
        assert_eq!(parse_extension("not json at all"), None);
    }

    /// tt-station contract (per TT_TOPLIKE_STREAM.md): a producer may stream a
    /// SUBSET of sub-keys. agentd emits `processes` but OMITS `inference`. A
    /// missing sub-key must decode to `None` ("not streamed → local fallback"),
    /// never a spurious empty list ("streamed, zero"). The whole extension must
    /// still decode (we must not drop the processes we DID get).
    #[test]
    fn missing_inference_subkey_is_none_not_empty() {
        // A frame whose tt_toplike carries processes but no `inference` key.
        let frame = r#"{"time":"t","device_info":[],
            "tt_toplike":{"schema":1,"processes":[
              {"pid":7,"name":"python3","cmd":"vllm","uses_tt":true,"cpu_pct":9.0,"mem_bytes":100}
            ]}}"#;
        let ext =
            parse_extension(frame).expect("extension with only `processes` must still decode");
        assert!(ext.processes.is_some(), "streamed processes present");
        assert_eq!(ext.processes.as_ref().unwrap().len(), 1);
        assert_eq!(
            ext.inference, None,
            "a missing inference sub-key is `None` (not streamed → local fallback), not Some([])"
        );
    }

    /// The inverse: a producer that streams `inference` but no `processes`
    /// decodes with `processes == None` (fall back to local processes).
    #[test]
    fn missing_processes_subkey_is_none() {
        let frame = r#"{"tt_toplike":{"schema":1,"inference":[
              {"key":"k","label":"M","phase":"ready","progress":null,"serving":null}
            ]}}"#;
        let ext = parse_extension(frame).expect("extension with only `inference` must decode");
        assert_eq!(ext.processes, None);
        assert!(ext.inference.is_some());
    }

    /// `build_extension` maps one `ProcRow` (TT-using) and one Ready
    /// `ServiceState` (with `serving` populated) into the wire shape:
    /// tt-toplike always streams both sub-keys as `Some(...)` (never omits
    /// like a partial producer would), `uses_tt` reflects `tt.is_some()`, and
    /// the phase lowercases per the `Phase` → wire-string mapping.
    #[test]
    fn build_extension_maps_proc_and_inference() {
        use crate::workload::host_processes::{ProcRow, TtProcInfo};
        use crate::workload::inference_server::metrics::ServingStats;
        use crate::workload::inference_server::{Phase, Readiness, ServiceState};

        let procs = vec![ProcRow {
            pid: 4242,
            name: "tt-inference-server".to_string(),
            cpu_pct: 87.5,
            mem_bytes: 12_884_901_888,
            inference: Some("vllm"),
            active: true,
            tt: Some(TtProcInfo {
                device_indices: vec![0],
                hugepages_1g: 4,
                hugepages_2m: 0,
            }),
        }];

        let inference = vec![ServiceState {
            key: "vllm-llama3-70b".to_string(),
            label: "Llama-3 70B (vLLM)".to_string(),
            phase: Phase::Ready,
            cpu_pct: 42.0,
            rss_bytes: 1024,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            safetensors_fds: 3,
            readiness: Readiness::Ready { runner: None },
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving: Some(ServingStats {
                generation_tps: 512.3,
                prompt_tps: 1024.7,
                completed_delta: 27,
                errored_delta: 0,
                requests_running: 3,
                requests_waiting: 1,
                kv_cache_usage: 0.42,
                ttft_avg_s: 0.35,
                queue_avg_s: 0.02,
                prefill_avg_s: 0.11,
                decode_avg_s: 0.05,
                tpot_avg_s: 0.03,
                prefix_hit_rate: 0.61,
                preemptions_delta: 2,
                counters: Default::default(),
            }),
        }];

        let ext = build_extension(&procs, &inference);

        let processes = ext.processes.expect("tt-toplike always streams processes");
        assert_eq!(processes.len(), 1);
        assert!(processes[0].uses_tt, "ProcRow.tt was Some(...)");

        let inference_out = ext.inference.expect("tt-toplike always streams inference");
        assert_eq!(inference_out.len(), 1);
        assert_eq!(inference_out[0].phase, "ready");
        assert!(inference_out[0].serving.is_some());
    }
}

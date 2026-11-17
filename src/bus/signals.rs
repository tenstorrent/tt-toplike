//! Signal names the main crate's sources write: the backend source and the
//! workload sources.
//!
//! These strings are part of the log format. Renaming one breaks replay of old
//! logs, so add new names and leave existing ones alone. Names that tt-telemetry
//! already uses keep its spelling (`tt_*`); the rest start with `toplike.`.
//!
//! Fast-lane signals go on a chip entity in one `Sweep` per poll. Slow-lane
//! signals go out as `Sample`s only when the value changed. Per-channel DRAM
//! signals go on the `dram_channel` entity of the chip.

// ---- Fast lane (chip entity, every poll) -------------------------------

/// Chip power draw in watts (tt-telemetry name).
pub const TDP_W: &str = "tt_tdp_w";
/// Core voltage in volts.
pub const VOLTAGE_V: &str = "toplike.voltage_v";
/// Core current in amps.
pub const CURRENT_A: &str = "toplike.current_a";
/// ASIC temperature in degrees Celsius (tt-telemetry name).
pub const ASIC_TEMPERATURE_C: &str = "tt_asic_temperature_celsius";
/// AI clock in MHz (tt-telemetry name).
pub const AI_CLOCK_MHZ: &str = "tt_ai_clock_mhz";
/// PCIe bytes per second flowing into the chip. Absent when the backend has no counters.
pub const PCIE_IN_BPS: &str = "toplike.pcie.in_bps";
/// PCIe bytes per second flowing out of the chip. Absent when the backend has no counters.
pub const PCIE_OUT_BPS: &str = "toplike.pcie.out_bps";

// ---- Slow lane, chip entity (only when changed) ------------------------

/// Board power in watts.
pub const BOARD_POWER_W: &str = "toplike.board_power_w";
/// ARC firmware heartbeat counter.
pub const HEARTBEAT: &str = "toplike.heartbeat";
/// Fan speed in RPM (tt-telemetry name). Firmware "no reading" values are not emitted.
pub const FAN_SPEED_RPM: &str = "tt_fan_speed_rpm";
/// AXI clock in MHz.
pub const AXI_CLOCK_MHZ: &str = "tt_axi_clock_mhz";
/// ARC clock in MHz.
pub const ARC_CLOCK_MHZ: &str = "tt_arc_clock_mhz";
/// Lifetime thermal trip count.
pub const THERM_TRIP_COUNT: &str = "toplike.therm_trip_count";
/// Harvesting state register.
pub const HARVESTING_STATE: &str = "toplike.harvesting_state";
/// Enabled Tensix column mask.
pub const ENABLED_TENSIX_COL: &str = "toplike.enabled_tensix_col";
/// Full Ethernet live-link mask (64 bits). A change also emits `Event::LinkChanged`
/// carrying the low 16 bits.
pub const ETH_LIVE_MASK: &str = "toplike.eth.live_mask";

// ---- Slow lane, dram_channel entity (only when changed) ----------------

/// GDDR temperature at the top of the channel, degrees Celsius.
pub const DRAM_TEMP_TOP_C: &str = "toplike.dram.temp_top_c";
/// GDDR temperature at the bottom of the channel, degrees Celsius.
pub const DRAM_TEMP_BOTTOM_C: &str = "toplike.dram.temp_bottom_c";
/// 1 when memory training passed on the channel, else 0.
pub const DRAM_TRAINING_PASS: &str = "toplike.dram.training_pass";
/// 1 when the built-in self test passed on the channel, else 0.
pub const DRAM_BIST_PASS: &str = "toplike.dram.bist_pass";
/// 1 when the channel is harvested (fused off), else 0.
pub const DRAM_HARVESTED: &str = "toplike.dram.harvested";
/// 1 when the channel is enabled, else 0.
pub const DRAM_ENABLED: &str = "toplike.dram.enabled";
/// Corrected ECC errors on reads.
pub const DRAM_ECC_CORR_RD: &str = "toplike.dram.ecc_corr_rd";
/// Corrected ECC errors on writes.
pub const DRAM_ECC_CORR_WR: &str = "toplike.dram.ecc_corr_wr";
/// Uncorrected ECC errors on reads.
pub const DRAM_ECC_UNCORR_RD: &str = "toplike.dram.ecc_uncorr_rd";
/// Uncorrected ECC errors on writes.
pub const DRAM_ECC_UNCORR_WR: &str = "toplike.dram.ecc_uncorr_wr";

/// Source name on `Raw` records carrying the verbatim tt-smi snapshot.
pub const RAW_SOURCE_TT_SMI: &str = "tt-smi";

// ---- Workload sources: model entity --------------------------------------
//
// The model signals the world reads (`model.label`, `model.phase`,
// `model.tokens_per_s`, `model.running`, `model.waiting`, `model.kv_cache`,
// `model.progress`) live in `tt_toplike_bus::signals`. These are the extra
// ones the inference and remote sources write. The world stores them as
// readings and does not copy them into `ModelView`.

/// Mean time to first token over the last scrape window, in seconds. Not sent while no request has finished.
pub const MODEL_TTFT_S: &str = "model.ttft_s";
/// Host CPU percent of the server's processes (100 = one core).
pub const MODEL_CPU_PCT: &str = "model.cpu_pct";
/// Host resident memory of the server's processes, in bytes.
pub const MODEL_RSS_BYTES: &str = "model.rss_bytes";
/// Compiled kernels found in the server's kernel cache.
pub const MODEL_KERNEL_COUNT: &str = "model.kernel_count";
/// Loaded weight shards (`.tensorbin` files) the server has written.
pub const MODEL_SHARD_COUNT: &str = "model.shard_count";
/// Media (image or video) generations in progress right now.
pub const MODEL_MEDIA_JOBS: &str = "model.media_jobs";
/// Media generations completed per minute over the last scrape window.
pub const MODEL_GENERATIONS_PER_MIN: &str = "model.generations_per_min";

// ---- Workload sources: training_run entity -------------------------------

/// The run's current step.
pub const TRAIN_STEP: &str = "toplike.train.step";
/// The run's step budget. Not sent while unknown (0).
pub const TRAIN_MAX_STEPS: &str = "toplike.train.max_steps";
/// The latest loss. Not sent while unknown or not finite.
pub const TRAIN_LOSS: &str = "toplike.train.loss";
/// Wall time of the latest step, in milliseconds. Measured when the trainer printed it, derived when timed from a progress bar.
pub const TRAIN_STEP_MS: &str = "toplike.train.step_ms";
/// The trainer's host CPU percent (100 = one core).
pub const TRAIN_CPU_PCT: &str = "toplike.train.cpu_pct";
/// The trainer's host resident memory, in bytes.
pub const TRAIN_RSS_BYTES: &str = "toplike.train.rss_bytes";
/// The run's chips as comma-separated chip keys (text). Not sent while none are known.
pub const TRAIN_CHIPS: &str = "toplike.train.chips";

// ---- Workload sources: process entity ------------------------------------

/// Process CPU percent (100 = one core).
pub const PROC_CPU_PCT: &str = "toplike.proc.cpu_pct";
/// Process resident memory, in bytes.
pub const PROC_RSS_BYTES: &str = "toplike.proc.rss_bytes";
/// Process name (text, passed through `redact_cmdline`).
pub const PROC_NAME: &str = "toplike.proc.name";
/// Process command line (text). Always passed through `redact_cmdline` first.
pub const PROC_CMDLINE: &str = "toplike.proc.cmdline";
/// The chips the process holds open, as comma-separated chip keys (text). Sent when it changes; an empty text means the process no longer holds a resolved chip.
pub const PROC_DEVICE: &str = "toplike.proc.device";
/// 1 when the remote host says the process uses Tenstorrent hardware, else 0 (remote source only).
pub const PROC_USES_TT: &str = "toplike.proc.uses_tt";

// ---- Gozer lease source (chip entity, only when changed) ---------------

/// The chip's gozer state as text: `FREE`, `HELD`, `HELD-FOREIGN`, `CLAIMED`,
/// `STALE`, `BUSY-UNTRACKED`, `DEGRADED` or `UNREADABLE`.
pub const GOZER_STATE: &str = "toplike.gozer.state";
/// Who holds the chip, from the lease (`--who`). Redacted and clipped to 128
/// characters. Empty text when the holder goes away.
pub const GOZER_WHO: &str = "toplike.gozer.who";

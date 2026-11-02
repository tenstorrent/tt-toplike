//! Signal names the backend source writes.
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

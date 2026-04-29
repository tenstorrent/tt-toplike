// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Board topology detection and visualization helpers
//!
//! This module groups Tenstorrent chips into boards and provides helpers that
//! drive topology-aware visualizations:
//!
//! - **Starfield**: intra-board streams are always visible (activity floor);
//!   inter-board streams only appear when both sides are active.
//! - **Memory Castle**: thick `║` separator at board boundaries vs thin `│`
//!   between chips on the same board.
//! - **Arcade header**: one-line topology diagram showing chip activity and
//!   link types (`←→` intra-board, `═══` inter-board).
//!
//! ## Board detection
//!
//! Primary method: group chips by matching `SmbusTelemetry.board_id` string.
//! Fallback (when any board_id is missing): board type-aware grouping.
//!
//! Single-chip PCIe cards (p150, n150, e75, e150) get `chips_per_board = 1` —
//! each card is its own independent unit with no on-board sibling.
//! Multi-chip carrier boards (p300, n300) get `chips_per_board = 2`.
//! Mixed or unknown fleets default to 2 (conservative).
//!
//! When every board has exactly one chip (`has_multi_chip_boards() == false`)
//! callers should suppress board-label rows and intra/inter-board link
//! decorations — the "board" concept doesn't apply to independent PCIe cards.
//!
//! ## sync_score
//!
//! A scalar 0.0–1.0 capturing how synchronised the activity of two chips is:
//! `(act_a * act_b).sqrt()` — the geometric mean.  This rewards both chips
//! being active at the same time (two quiet chips give score 0.0, two busy
//! chips give 1.0, one quiet + one busy gives ≈0.5 regardless of which way).
//!
//! Intra-board pairs receive a minimum floor of 0.2 so the structural link
//! between on-board siblings is always at least faintly visible.

use crate::models::Device;
use std::collections::HashMap;

// ─── Board hue palette ────────────────────────────────────────────────────────
//
// Each board gets a fixed base hue so it has a consistent colour identity
// across all visualisation modes.  We spread boards evenly around the wheel.
//
// Board 0 → 200° (blue-cyan)  Board 1 → 20° (orange)
// Board 2 → 290° (magenta)    Board 3 → 110° (green)

const BASE_HUES: &[f32] = &[200.0, 20.0, 290.0, 110.0];

/// Minimum sync score for intra-board chip pairs.
///
/// This floor ensures that on-board siblings always show a faint structural
/// link even when both chips are completely idle.
pub const INTRA_BOARD_FLOOR: f32 = 0.2;

// ─── Data types ───────────────────────────────────────────────────────────────

/// A group of chips that share a physical board (and DDR).
#[derive(Debug, Clone)]
pub struct Board {
    /// Human-readable label derived from board_id or index ("p300c-0", "board-1", …)
    pub label: String,

    /// Device indices belonging to this board (order matches `backend.devices()`)
    pub chips: Vec<usize>,

    /// Base hue (0–360°) used to colour-code this board's chips and links
    pub hue: f32,
}

/// Full system topology: boards and the links between them.
#[derive(Debug, Clone)]
pub struct BoardTopology {
    /// All detected boards, in stable order
    pub boards: Vec<Board>,

    /// Pairs of board indices that are networked together.
    /// On a QB2 this would be `[(0, 1)]`.
    pub inter_board_links: Vec<(usize, usize)>,
}

// ─── Construction ─────────────────────────────────────────────────────────────

impl BoardTopology {
    /// Build topology from devices and optional SMBUS board IDs.
    ///
    /// If all board IDs are `Some` and at least two distinct values exist the
    /// IDs are used for grouping.  Otherwise (any `None`, or all chips share
    /// one ID) falls back to consecutive-index pairs so every multi-chip system
    /// gets a reasonable topology.
    pub fn from_devices_with_ids(devices: &[Device], board_ids: &[Option<String>]) -> Self {
        // Attempt ID-based grouping if we have complete information.
        if board_ids.len() == devices.len() && board_ids.iter().all(|b| b.is_some()) {
            let mut id_to_chips: HashMap<String, Vec<usize>> = HashMap::new();
            for (dev, id_opt) in devices.iter().zip(board_ids.iter()) {
                if let Some(id) = id_opt {
                    id_to_chips.entry(id.clone()).or_default().push(dev.index);
                }
            }

            // Only use ID grouping when there are ≥2 distinct board IDs.
            if id_to_chips.len() >= 2 {
                let mut sorted_ids: Vec<String> = id_to_chips.keys().cloned().collect();
                sorted_ids.sort();  // stable board ordering across calls

                let boards: Vec<Board> = sorted_ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| {
                        let mut chips = id_to_chips[id].clone();
                        chips.sort();
                        Board {
                            label: id.clone(),
                            chips,
                            hue: BASE_HUES[i % BASE_HUES.len()],
                        }
                    })
                    .collect();

                let inter = inter_board_links(boards.len());
                return Self { boards, inter_board_links: inter };
            }
        }

        // Fallback: consecutive pairs
        Self::from_devices(devices)
    }

    /// Build topology using board-type-aware consecutive grouping fallback.
    ///
    /// Reads `board_type` on each device to determine how many chips share a
    /// physical carrier board:
    ///
    /// - Single-chip cards (p150, n150, e75, e150): `chips_per_board = 1`
    /// - Dual-chip carriers (p300, n300): `chips_per_board = 2`
    /// - Mixed or unknown: `chips_per_board = 2` (conservative)
    ///
    /// When all devices are single-chip cards, each device gets its own Board
    /// entry so `has_multi_chip_boards()` returns `false` and visualizations
    /// can suppress board-level decorations.
    pub fn from_devices(devices: &[Device]) -> Self {
        let all_single_chip = !devices.is_empty()
            && devices.iter().all(|d| is_single_chip_card(&d.board_type));
        let chips_per_board = if all_single_chip { 1 } else { 2 };

        let n = devices.len();
        let num_boards = (n + chips_per_board - 1) / chips_per_board;

        let boards: Vec<Board> = (0..num_boards)
            .map(|b| {
                let start = b * chips_per_board;
                let end = (start + chips_per_board).min(n);
                let chips: Vec<usize> = devices[start..end].iter().map(|d| d.index).collect();
                // For single-chip cards, use the board_type in the label; for
                // multi-chip carriers, use the generic "board-N" form.
                let label = if chips_per_board == 1 {
                    let bt = devices[start].board_type.to_lowercase();
                    if bt.is_empty() || bt == "unknown" {
                        format!("card-{}", b)
                    } else {
                        format!("{}-{}", bt.trim_end_matches('a').trim_end_matches('c'), b)
                    }
                } else {
                    format!("board-{}", b)
                };
                Board { label, chips, hue: BASE_HUES[b % BASE_HUES.len()] }
            })
            .collect();

        let inter = inter_board_links(boards.len());
        Self { boards, inter_board_links: inter }
    }

    /// Returns `true` when at least one board contains more than one chip.
    ///
    /// `false` means every device is a standalone single-chip card.
    /// Visualizations use this to suppress board-label rows and
    /// intra/inter-board link decorations that have no meaning in that case.
    pub fn has_multi_chip_boards(&self) -> bool {
        self.boards.iter().any(|b| b.chips.len() > 1)
    }

    /// Returns `true` when devices `a` and `b` are on the same board.
    pub fn same_board(&self, a: usize, b: usize) -> bool {
        self.boards.iter().any(|board| {
            board.chips.contains(&a) && board.chips.contains(&b)
        })
    }

    /// Base hue for the board that owns `device_idx`, or 0.0 if not found.
    pub fn board_hue(&self, device_idx: usize) -> f32 {
        self.boards
            .iter()
            .find(|b| b.chips.contains(&device_idx))
            .map(|b| b.hue)
            .unwrap_or(0.0)
    }

    /// Label of the board that owns `device_idx`.
    pub fn board_label(&self, device_idx: usize) -> &str {
        self.boards
            .iter()
            .find(|b| b.chips.contains(&device_idx))
            .map(|b| b.label.as_str())
            .unwrap_or("?")
    }

    /// Index (0-based) of the board that owns `device_idx`, if known.
    pub fn board_index(&self, device_idx: usize) -> Option<usize> {
        self.boards
            .iter()
            .position(|b| b.chips.contains(&device_idx))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Returns `true` when `board_type` names a known single-chip PCIe card.
///
/// Known single-chip product families:
/// - p150 (Blackhole), n150 (Wormhole), e75 / e150 (Grayskull)
///
/// Dual-chip carrier families (p300, n300) return `false`.
/// Unknown strings return `false` (conservative: assume multi-chip).
fn is_single_chip_card(board_type: &str) -> bool {
    let lower = board_type.to_lowercase();
    lower.contains("p150") || lower.contains("n150")
        || lower.contains("e75") || lower.contains("e150")
}

/// Build a full mesh of inter-board links for `num_boards` boards.
///
/// For 1 board: empty.
/// For 2 boards: [(0,1)].
/// For 3+ boards: all pairs — small enough that O(n²) is fine.
fn inter_board_links(num_boards: usize) -> Vec<(usize, usize)> {
    let mut links = Vec::new();
    for a in 0..num_boards {
        for b in (a + 1)..num_boards {
            links.push((a, b));
        }
    }
    links
}

// ─── sync_score ───────────────────────────────────────────────────────────────

/// Compute the activity synchronisation score between two chips.
///
/// Returns a value in `[0.0, 1.0]`.  The geometric mean `sqrt(a * b)` rewards
/// both chips being active simultaneously — if either is idle the score drops
/// toward 0.0.
///
/// `intra_board = true` adds a floor of [`INTRA_BOARD_FLOOR`] so that
/// structural on-board links are always at least faintly rendered.
///
/// # Arguments
///
/// * `activity_a` — baseline-relative activity for chip A (0.0 = idle, 1.0 = max)
/// * `activity_b` — baseline-relative activity for chip B
/// * `intra_board` — `true` when A and B share a physical board
pub fn sync_score(activity_a: f32, activity_b: f32, intra_board: bool) -> f32 {
    let a = activity_a.max(0.0).min(1.0);
    let b = activity_b.max(0.0).min(1.0);
    let raw = (a * b).sqrt();
    if intra_board { raw.max(INTRA_BOARD_FLOOR) } else { raw }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Architecture;

    fn make_devices_typed(n: usize, board_type: &str) -> Vec<Device> {
        (0..n).map(|i| Device {
            index: i,
            board_type: board_type.to_string(),
            bus_id: format!("0000:0{}:00.0", i),
            coords: String::new(),
            architecture: Architecture::Blackhole,
        }).collect()
    }

    fn make_devices(n: usize) -> Vec<Device> {
        make_devices_typed(n, "p300c")
    }

    // ── Dual-chip carrier boards (p300c) ──────────────────────────────────────

    #[test]
    fn test_p300c_pairs_4_chips() {
        let devices = make_devices(4);
        let topo = BoardTopology::from_devices(&devices);
        assert_eq!(topo.boards.len(), 2);
        assert_eq!(topo.boards[0].chips, vec![0, 1]);
        assert_eq!(topo.boards[1].chips, vec![2, 3]);
        assert_eq!(topo.inter_board_links, vec![(0, 1)]);
        assert!(topo.has_multi_chip_boards());
    }

    #[test]
    fn test_p300c_same_board() {
        let devices = make_devices(4);
        let topo = BoardTopology::from_devices(&devices);
        assert!(topo.same_board(0, 1));
        assert!(topo.same_board(2, 3));
        assert!(!topo.same_board(0, 2));
        assert!(!topo.same_board(1, 3));
    }

    // ── Single-chip PCIe cards (p150a) ────────────────────────────────────────

    #[test]
    fn test_p150a_each_chip_is_its_own_board() {
        let devices = make_devices_typed(4, "p150a");
        let topo = BoardTopology::from_devices(&devices);
        // Each card gets its own board — no pairing.
        assert_eq!(topo.boards.len(), 4);
        for (b, board) in topo.boards.iter().enumerate() {
            assert_eq!(board.chips, vec![b], "board {} should contain only chip {}", b, b);
        }
        // No chip shares a board with another.
        assert!(!topo.same_board(0, 1));
        assert!(!topo.same_board(2, 3));
        // has_multi_chip_boards is false → board-label row should be suppressed.
        assert!(!topo.has_multi_chip_boards());
    }

    #[test]
    fn test_single_chip_families() {
        for bt in &["p150a", "p150c", "n150", "e75", "e150"] {
            let devices = make_devices_typed(2, bt);
            let topo = BoardTopology::from_devices(&devices);
            assert_eq!(topo.boards.len(), 2, "board_type '{}' should give 1 chip/board", bt);
            assert!(!topo.has_multi_chip_boards(), "board_type '{}' should not have multi-chip boards", bt);
        }
    }

    #[test]
    fn test_mixed_fleet_defaults_to_pairing() {
        // Mixed fleet: conservative — treats as multi-chip carrier.
        let mut devices = make_devices_typed(2, "p300c");
        devices.push(Device {
            index: 2,
            board_type: "p150a".to_string(),
            bus_id: "0000:03:00.0".to_string(),
            coords: String::new(),
            architecture: Architecture::Blackhole,
        });
        let topo = BoardTopology::from_devices(&devices);
        // Not all single-chip → chips_per_board = 2
        assert!(topo.has_multi_chip_boards());
    }

    // ── SMBUS ID-based grouping ───────────────────────────────────────────────

    #[test]
    fn test_id_grouping_p300c() {
        let devices = make_devices(4);
        let ids = vec![
            Some("p300c-abc".to_string()),
            Some("p300c-abc".to_string()),
            Some("p300c-def".to_string()),
            Some("p300c-def".to_string()),
        ];
        let topo = BoardTopology::from_devices_with_ids(&devices, &ids);
        assert_eq!(topo.boards.len(), 2);
        assert!(topo.same_board(0, 1));
        assert!(topo.same_board(2, 3));
        assert!(!topo.same_board(0, 2));
        assert!(topo.has_multi_chip_boards());
    }

    #[test]
    fn test_id_grouping_p150a_all_distinct_ids() {
        // 4× p150a with distinct SMBUS board IDs → 4 standalone boards.
        let devices = make_devices_typed(4, "p150a");
        let ids = vec![
            Some("p150a-aaa".to_string()),
            Some("p150a-bbb".to_string()),
            Some("p150a-ccc".to_string()),
            Some("p150a-ddd".to_string()),
        ];
        let topo = BoardTopology::from_devices_with_ids(&devices, &ids);
        assert_eq!(topo.boards.len(), 4);
        assert!(!topo.has_multi_chip_boards());
    }

    #[test]
    fn test_id_grouping_falls_back_when_any_none() {
        let devices = make_devices(4);
        let ids = vec![
            Some("p300c-abc".to_string()),
            None,  // <-- missing
            Some("p300c-def".to_string()),
            Some("p300c-def".to_string()),
        ];
        let topo = BoardTopology::from_devices_with_ids(&devices, &ids);
        // Falls back to type-aware grouping (p300c → chips_per_board=2)
        assert_eq!(topo.boards[0].chips, vec![0, 1]);
        assert_eq!(topo.boards[1].chips, vec![2, 3]);
    }

    #[test]
    fn test_sync_score_intra_board() {
        // Both idle: floor kicks in
        let s = sync_score(0.0, 0.0, true);
        assert!((s - INTRA_BOARD_FLOOR).abs() < 1e-6, "got {}", s);

        // Both at max: score 1.0
        let s = sync_score(1.0, 1.0, true);
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_sync_score_inter_board() {
        // Both idle: no floor → 0.0
        let s = sync_score(0.0, 0.0, false);
        assert!(s < 1e-6);

        // Mixed: geometric mean
        let s = sync_score(0.5, 0.5, false);
        assert!((s - 0.5).abs() < 1e-4);
    }

    #[test]
    fn test_board_hue() {
        let devices = make_devices(4);
        let topo = BoardTopology::from_devices(&devices);
        let h0 = topo.board_hue(0);
        let h1 = topo.board_hue(1);
        let h2 = topo.board_hue(2);
        // Chips 0 and 1 share a board → same hue
        assert!((h0 - h1).abs() < 1e-6);
        // Board 1 has a different hue from board 0
        assert!((h0 - h2).abs() > 1.0);
    }
}

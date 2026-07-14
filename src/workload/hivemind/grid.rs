// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Decaying (source × column) activity heat. `bump` on each event, `decay`
//! once per frame so idle cells fade to cold.

use super::event::Source;
use std::collections::HashMap;

pub const DECAY: f32 = 0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    Device(u8),
    Host,
}

pub struct HeatGrid {
    heat: HashMap<(Source, Column), f32>,
    order: Vec<Source>, // sources in first-seen order
    max_device: Option<u8>,
}

impl HeatGrid {
    pub fn new() -> Self {
        Self {
            heat: HashMap::new(),
            order: Vec::new(),
            max_device: None,
        }
    }

    pub fn bump(&mut self, source: Source, device: Option<u8>) {
        let col = match device {
            Some(d) => {
                self.max_device = Some(self.max_device.map_or(d, |m| m.max(d)));
                Column::Device(d)
            }
            None => Column::Host,
        };
        if !self.order.contains(&source) {
            self.order.push(source);
        }
        let e = self.heat.entry((source, col)).or_insert(0.0);
        *e = (*e + 1.0).min(8.0);
    }

    pub fn decay(&mut self) {
        for v in self.heat.values_mut() {
            *v *= DECAY;
        }
    }

    pub fn heat(&self, source: Source, col: Column) -> f32 {
        *self.heat.get(&(source, col)).unwrap_or(&0.0)
    }

    pub fn active_sources(&self) -> Vec<Source> {
        self.order.clone()
    }

    pub fn max_device(&self) -> Option<u8> {
        self.max_device
    }
}

impl Default for HeatGrid {
    fn default() -> Self {
        Self::new()
    }
}

/// Map decayed heat (0..~8) to a block glyph. Normalizes to 0..1 for the ramp.
pub fn heat_char(heat: f32) -> char {
    crate::animation::common::value_to_block_char((heat / 8.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_tracks_column_and_first_seen_order() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        g.bump(Source::Ttnn, None);
        g.bump(Source::TtMetal, Some(3));
        assert_eq!(g.active_sources(), vec![Source::TtMetal, Source::Ttnn]);
        assert_eq!(g.max_device(), Some(3));
        assert!(g.heat(Source::TtMetal, Column::Device(0)) > 0.0);
        assert!(g.heat(Source::Ttnn, Column::Host) > 0.0);
    }

    #[test]
    fn decay_cools_idle_cells() {
        let mut g = HeatGrid::new();
        g.bump(Source::TtMetal, Some(0));
        let before = g.heat(Source::TtMetal, Column::Device(0));
        g.decay();
        let after = g.heat(Source::TtMetal, Column::Device(0));
        assert!(after < before);
        assert!((after - before * DECAY).abs() < 1e-6);
    }
}

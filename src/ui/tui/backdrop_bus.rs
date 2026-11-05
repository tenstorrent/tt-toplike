// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The TUI's side of the shared backdrop (`animation::backdrop`).
//!
//! Each view's backdrop is a setting ([`BackdropSettings`]). Three views
//! draw their default backdrop with their own painter, as before: the
//! Inference view's snake its atmosphere (the scene), the Training view its
//! sky, Die 3D its clouds. When one of them is set to another kind, the TUI
//! lays that kind into the drawn frame ([`SharedBackdrop::draw`]), in blank
//! cells only, after the view and before the status bar, the command bar
//! and the overlays. Every other view takes no backdrop
//! (`backdrop::accepts`), so nothing is laid over it.
//!
//! The update runs at the view's data rate, in the update block, after the
//! view's own update ([`SharedBackdrop::update`]). A view the last update
//! did not feed starts a fresh backdrop, so nothing that happened while it
//! was away draws.

use crate::animation::backdrop::{
    builtin, Backdrop, BackdropKind, BackdropSettings, BackdropSignals, Choice,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// The kind the TUI lays over `view` itself: the view's setting, unless the
/// view draws it with its own painter. A view that takes no backdrop has
/// none (`BackdropSettings::choice`).
pub(crate) fn layer_kind(settings: &BackdropSettings, view: &str) -> Choice {
    let c = settings.choice(view);
    if c == builtin(view) {
        None
    } else {
        c
    }
}

/// Whether `view`'s own painter is drawn: its setting is the backdrop it
/// has of its own. The snake, the Training view and Die 3D ask this.
pub(crate) fn own_painter(settings: &BackdropSettings, view: &str) -> bool {
    builtin(view).is_some() && settings.choice(view) == builtin(view)
}

/// The backdrop the TUI lays over the view on screen.
#[derive(Debug, Clone)]
pub(crate) struct SharedBackdrop {
    backdrop: Backdrop,
    /// The view fed by the last update, `None` when it drew no backdrop.
    fed: Option<&'static str>,
    /// Whether the last signals of the view on screen held an estimate.
    estimated: bool,
    /// Each chip's load scale, kept across view switches (the backdrop
    /// itself starts fresh in each view).
    scales: crate::animation::backdrop::LoadScales,
}

impl Default for SharedBackdrop {
    fn default() -> Self {
        Self {
            backdrop: Backdrop::new(None),
            fed: None,
            estimated: false,
            scales: Default::default(),
        }
    }
}

impl SharedBackdrop {
    /// One update for `view`. `gather` reads the signals; its argument says
    /// whether the service rows are needed (the scene follows them). Nothing
    /// is read when the view has no backdrop, nor when the view draws its
    /// own and the legend (which shows the `~` marker) is closed.
    pub(crate) fn update(
        &mut self,
        settings: &BackdropSettings,
        view: &'static str,
        legend_open: bool,
        gather: impl FnOnce(bool) -> BackdropSignals,
    ) {
        let choice = settings.choice(view);
        let kind = layer_kind(settings, view);
        if choice.is_none() || (kind.is_none() && !legend_open) {
            self.backdrop = Backdrop::new(None);
            self.fed = None;
            self.estimated &= choice.is_some();
            return;
        }
        let mut signals = gather(choice == Some(BackdropKind::Scene));
        self.scales.apply(&mut signals);
        self.estimated = signals.estimated;
        if kind.is_none() {
            // The view draws its own (or the arcade lays its own).
            self.backdrop = Backdrop::new(None);
            self.fed = None;
            return;
        }
        if self.fed != Some(view) {
            self.backdrop = Backdrop::new(kind);
        }
        self.backdrop.set_kind(kind);
        self.backdrop.observe(&signals);
        self.fed = Some(view);
    }

    /// Lay the backdrop into `area` of the drawn frame for `view`, the view
    /// on screen. Nothing when the view draws its own or has none, nor when
    /// the last update fed another view: a key that switches views draws
    /// once before the next update, and that frame must not carry the old
    /// view's backdrop.
    pub(crate) fn draw(&self, buf: &mut Buffer, area: Rect, view: &str) {
        if self.fed == Some(view) {
            self.backdrop.layer_buffer(buf, area);
        }
    }

    /// Whether the last signals held an estimate (for the legend's `~`).
    pub(crate) fn estimated(&self) -> bool {
        self.estimated
    }

    #[cfg(test)]
    pub(crate) fn backdrop(&self) -> &Backdrop {
        &self.backdrop
    }
}

/// The area the shared backdrop is laid in: the screen above the status bar
/// rows. The status bar does not clear the cells it leaves unprinted, so a
/// glyph there would show through it.
pub(crate) fn area(full: Rect, status_rows: u16) -> Rect {
    Rect {
        height: full.height.saturating_sub(status_rows),
        ..full
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::backdrop::BackdropConfig;

    const SKY: Choice = Some(BackdropKind::Sky);
    const CLOUDS: Choice = Some(BackdropKind::Clouds);
    const SCENE: Choice = Some(BackdropKind::Scene);

    #[test]
    fn the_tui_lays_only_what_a_view_does_not_draw_itself() {
        let mut s = BackdropSettings::new(BackdropConfig::default());
        for v in crate::animation::backdrop::VIEWS {
            assert_eq!(layer_kind(&s, v), None, "{v}: the defaults draw as before");
        }
        assert!(
            own_painter(&s, "training") && own_painter(&s, "die3d") && own_painter(&s, "inference")
        );
        s.set("training", CLOUDS).unwrap();
        s.set("die3d", SKY).unwrap();
        assert!(s.set("castle", SCENE).is_err());
        assert!(s.set("arcade", CLOUDS).is_err());
        assert_eq!(layer_kind(&s, "training"), CLOUDS);
        assert!(
            !own_painter(&s, "training"),
            "the sky is off under the clouds"
        );
        assert_eq!(layer_kind(&s, "die3d"), SKY);
        assert!(!own_painter(&s, "die3d"));
        assert_eq!(layer_kind(&s, "castle"), None, "the castle takes none");
        assert_eq!(layer_kind(&s, "arcade"), None, "the arcade takes none");
        s.set("training", None).unwrap();
        assert_eq!(layer_kind(&s, "training"), None);
        assert!(!own_painter(&s, "training"), "off is off");
    }

    /// The shared layer measures load as the Waffler does, and keeps each
    /// chip's scale across a view switch: an idle chip at 40 W of 125 W
    /// shows the dimmest clouds, which power over TDP (0.32) would not.
    #[test]
    fn the_shared_clouds_use_the_waffler_load_scale_across_views() {
        use crate::animation::backdrop::ChipDraw;
        use crate::animation::die3d::clouds::Clouds;
        let mut s = BackdropSettings::new(BackdropConfig::default());
        s.set("training", CLOUDS).unwrap();
        s.set("inference", CLOUDS).unwrap();
        let mut b = SharedBackdrop::default();
        let idle = |t: f64| BackdropSignals {
            now_s: t,
            load: Some(40.0 / 125.0),
            chips: vec![ChipDraw {
                key: "0000:01:00.0".into(),
                power_w: Some(40.0),
                fallback: Some(40.0 / 125.0),
            }],
            ..Default::default()
        };
        for i in 0..30 {
            b.update(&s, "training", false, |_| idle(i as f64));
        }
        assert_eq!(b.backdrop().cloud_intensity(), Clouds::intensity(0.0));
        // The Inference view starts a fresh backdrop, and its first update
        // already has the chip's idle.
        b.update(&s, "inference", false, |_| idle(30.0));
        assert_eq!(b.backdrop().cloud_intensity(), Clouds::intensity(0.0));
    }

    /// A frame drawn for a view the last update did not feed lays nothing
    /// (review M2).
    #[test]
    fn draw_lays_only_the_backdrop_of_the_view_it_was_fed() {
        let mut s = BackdropSettings::new(BackdropConfig::default());
        s.set("training", CLOUDS).unwrap();
        let mut b = SharedBackdrop::default();
        for i in 0..20 {
            b.update(&s, "training", false, |_| BackdropSignals {
                now_s: i as f64 / 4.0,
                pcie_bps: Some(4e9),
                load: Some(1.0),
                ..Default::default()
            });
        }
        let area = Rect::new(0, 0, 60, 20);
        let mut training = Buffer::empty(area);
        b.draw(&mut training, area, "training");
        assert_ne!(
            training,
            Buffer::empty(area),
            "the Training view's backdrop"
        );
        let mut other = Buffer::empty(area);
        b.draw(&mut other, area, "inference");
        assert_eq!(other, Buffer::empty(area), "nothing for another view");
    }

    #[test]
    fn a_view_with_no_backdrop_reads_no_signal_and_a_new_view_starts_fresh() {
        let mut s = BackdropSettings::new(BackdropConfig::default());
        let mut b = SharedBackdrop::default();
        b.update(&s, "grid", false, |_| {
            panic!("a view with no backdrop reads nothing")
        });
        assert!(!b.backdrop().active());
        s.set("training", None).unwrap();
        b.update(&s, "training", false, |_| {
            panic!("an off view reads nothing")
        });
        s.set("training", CLOUDS).unwrap();
        s.set("inference", CLOUDS).unwrap();
        let busy = |_: bool| BackdropSignals {
            pcie_bps: Some(1e10),
            ..Default::default()
        };
        for i in 0..200 {
            b.update(&s, "training", false, |n| BackdropSignals {
                now_s: i as f64 / 10.0,
                ..busy(n)
            });
        }
        assert!(b.backdrop().active());
        let before = b.backdrop().canvas(60, 20);
        // Another view: a fresh backdrop, which has not drifted.
        b.update(&s, "inference", false, |n| BackdropSignals {
            now_s: 20.0,
            ..busy(n)
        });
        assert_ne!(b.backdrop().canvas(60, 20), before);
        assert_eq!(b.backdrop().canvas(60, 20), {
            let mut fresh = Backdrop::new(CLOUDS);
            fresh.observe(&BackdropSignals {
                now_s: 20.0,
                ..busy(false)
            });
            fresh.canvas(60, 20)
        });
        // The scene asks for the service rows; the others do not.
        s.set("training", SCENE).unwrap();
        let mut asked = false;
        b.update(&s, "training", false, |rows| {
            asked = rows;
            BackdropSignals::default()
        });
        assert!(asked);
    }

    /// Busy signals: a serving model's rows, PCIe traffic and a loaded chip,
    /// so every kind has something to draw.
    fn busy_signals(t: f64) -> BackdropSignals {
        BackdropSignals {
            now_s: t,
            services: crate::workload::inference_server::mock::mock_services(t as f32 + 100.0),
            pcie_bps: Some(4e9),
            load: Some(1.0),
            ..Default::default()
        }
    }

    /// A drawn view: text with blank cells between, as a view leaves them.
    fn drawn(area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        for y in (0..area.height).step_by(3) {
            buf.set_string(0, y, "view text  │  more", ratatui::style::Style::default());
        }
        buf
    }

    /// Every view that takes no backdrop draws the same frame with any
    /// `[backdrop]` setting, after `w` and after `/backdrop`: nothing is laid
    /// over it, its signals are not read, and its setting stays off.
    #[test]
    fn a_view_that_takes_no_backdrop_draws_the_same_frame_with_any_setting() {
        use crate::animation::backdrop::{command, key_message, takes_backdrop, VIEWS};
        let area = Rect::new(0, 0, 60, 20);
        let refusing: Vec<&str> = VIEWS
            .iter()
            .copied()
            .filter(|v| !takes_backdrop(v))
            .collect();
        assert_eq!(
            refusing,
            vec![
                "insights",
                "grid",
                "starfield",
                "castle",
                "flow",
                "arcade",
                "defrag",
                "hivemind"
            ]
        );
        for v in refusing {
            for c in [None, SCENE, SKY, CLOUDS] {
                let config = BackdropConfig {
                    default: Some(c),
                    views: vec![(v, c)],
                };
                let mut s = BackdropSettings::new(config);
                assert_eq!(s.choice(v), None, "{v} {c:?}");
                assert_eq!(key_message(v, &mut s), "no backdrop on this view");
                let arg = crate::animation::backdrop::choice_name(c);
                assert_eq!(
                    command(arg, v, &mut s),
                    ("no backdrop on this view".into(), false)
                );
                assert_eq!(s.choice(v), None, "{v}: `w` and /backdrop change nothing");
                assert!(!own_painter(&s, v), "{v}");
                let mut b = SharedBackdrop::default();
                for i in 0..40 {
                    b.update(&s, v, true, |_| -> BackdropSignals {
                        panic!("{v}: a view with no backdrop reads no signal at {i}")
                    });
                }
                let mut buf = drawn(area);
                b.draw(&mut buf, area, v);
                assert_eq!(buf, drawn(area), "{v} {c:?}: byte-identical");
            }
        }
    }

    /// The three views that carry a background of their own still draw it
    /// with their own painter by default, and a kind set for one of them is
    /// laid over its frame.
    #[test]
    fn the_inference_training_and_waffler_views_still_draw_theirs() {
        let area = Rect::new(0, 0, 60, 20);
        for (v, other) in [("inference", SKY), ("training", CLOUDS), ("die3d", SCENE)] {
            let mut s = BackdropSettings::new(BackdropConfig::default());
            assert!(own_painter(&s, v), "{v}: its own painter");
            assert_eq!(layer_kind(&s, v), None, "{v}");
            s.set(v, other).unwrap();
            assert!(!own_painter(&s, v));
            assert_eq!(layer_kind(&s, v), other, "{v}");
            let mut b = SharedBackdrop::default();
            for i in 0..40 {
                b.update(&s, v, false, |_| busy_signals(i as f64 / 4.0));
            }
            let mut buf = drawn(area);
            b.draw(&mut buf, area, v);
            assert_ne!(buf, drawn(area), "{v}: {other:?} is laid");
        }
    }

    #[test]
    fn the_layer_stops_above_the_status_bar() {
        assert_eq!(area(Rect::new(0, 0, 80, 24), 1), Rect::new(0, 0, 80, 23));
        assert_eq!(area(Rect::new(0, 0, 80, 0), 1).height, 0);
    }

    /// In a view that takes a backdrop the legend ends with its backdrop
    /// line, and every choice, with or without the estimate marker, fits the
    /// legend's 30 rows and 78 columns at 80 columns. Every other view's
    /// legend has no backdrop line.
    #[test]
    fn every_backdrop_line_fits_every_legend() {
        use super::super::{
            line_cols, overlay_lines, overlay_lines_with, overlay_row_target, overlay_rows_needed,
            overlay_wrap_cols, view_name, DisplayMode, OverlayPanel,
        };
        let modes = [
            DisplayMode::Insights,
            DisplayMode::Grid,
            DisplayMode::Starfield,
            DisplayMode::MemoryCastle,
            DisplayMode::MemoryFlow,
            DisplayMode::Arcade,
            DisplayMode::Defrag,
            DisplayMode::InferenceMonitor,
            DisplayMode::HivemindSweeper,
            DisplayMode::Training,
            DisplayMode::Die3d,
        ];
        let cols = overlay_wrap_cols(80);
        let text = |l: &ratatui::text::Line<'_>| -> String {
            l.spans.iter().map(|s| s.content.as_ref()).collect()
        };
        for mode in modes {
            let default = overlay_lines(OverlayPanel::Legend, mode, cols);
            if !crate::animation::backdrop::takes_backdrop(view_name(mode)) {
                assert!(
                    default.iter().all(|l| !text(l).contains("Backdrop:")),
                    "{mode:?}"
                );
                continue;
            }
            let want = crate::animation::backdrop::legend_line(
                BackdropConfig::default().choice(view_name(mode)),
                false,
            );
            assert!(text(default.last().unwrap()).ends_with(&want), "{mode:?}");
            for c in [None, SCENE, SKY, CLOUDS] {
                for est in [false, true] {
                    let lines = overlay_lines_with(OverlayPanel::Legend, mode, cols, (c, est), &[]);
                    assert!(
                        overlay_rows_needed(lines.len())
                            <= overlay_row_target(OverlayPanel::Legend),
                        "{mode:?} {c:?}"
                    );
                    assert!(lines.iter().all(|l| line_cols(l) <= 78), "{mode:?} {c:?}");
                }
            }
        }
    }
}

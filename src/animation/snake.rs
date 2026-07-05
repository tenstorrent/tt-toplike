// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The `Snake` coordinator — one persistent creature for the `[i]` view.
//!
//! A single living identity that picks a behavior each frame from the inference
//! snapshot and delegates rendering to one of three sub-renderers:
//! - **Roaming** — [`ModelStarfield`], the cold-trail catalog screensaver;
//! - **Growing** — [`LoadSnake`], the boxed-snake loading journey (+ gold burst);
//! - **Feeding** — [`ServingCreature`], the calm vLLM-driven serving snake.
//!
//! Continuity is by *identity*, not by morphing: the coordinator carries the
//! sub-renderer state (and the resolved [`Behavior`]) across frames rather than
//! tweening between layouts. This mirrors the tri-state short-circuit that used
//! to live inline in `ui::tui::mod.rs` (featured-loading wins, then the Ready
//! burst keeps playing, then Feeding, then the cold-trail Roaming fallback).
//!
//! ## Dimensions
//!
//! [`LoadSnake`] and [`ModelStarfield`] fix their grid at construction, so the
//! coordinator carries `dims` and recreates those two whenever the view resizes
//! (mirroring the old `recreate-on-resize` logic). [`ServingCreature`] takes its
//! dims at `render`, so it needs no recreation. The `width`/`height` passed to
//! [`Snake::render`] therefore match what [`Snake::update`] built the fixed-dim
//! renderers against, and everything lines up.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};

use crate::animation::{ChipReading, LoadSnake, ModelStarfield, ServingCreature};
use crate::ui::tui::inference_panel::featured_loading;
use crate::workload::inference_server::{Phase, ServiceState};
use crate::workload::model_catalog::CatalogModel;

/// Length (cells) of the hungry roaming snake drawn over the cold starfield.
const ROAM_LEN: usize = 6;

/// The behavior the coordinator resolved for the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    /// Cold trail — drift the model-catalog starfield ([`ModelStarfield`]).
    Roaming,
    /// A model is compiling/loading (or stalled) — grow the [`LoadSnake`].
    Growing,
    /// A model is Ready — feed the [`ServingCreature`] with live serving stats.
    Feeding,
}

/// The per-frame, fully borrowed input to [`Snake::update`]. Carries `width`
/// and `height` because the fixed-dim sub-renderers must be rebuilt on resize.
pub struct SnakeWorld<'a> {
    /// The live inference-monitor rows (one per detected/known service).
    pub rows: &'a [ServiceState],
    /// The model catalog, drifting behind the Roaming starfield.
    pub catalog: &'a [CatalogModel],
    /// The detected chip set / architecture name (classifies catalog support).
    pub arch: &'a str,
    /// Live per-chip telemetry readings (power/temp/clock), one per device.
    /// Threaded into the Feeding creature for the silicon strip; empty off-TT.
    pub chips: &'a [ChipReading],
    /// Monitor tick cadence in seconds (drives per-second rate readouts).
    pub cadence_secs: u32,
    /// Render width in cells (fixed-dim renderers are built to this).
    pub width: usize,
    /// Render height in cells (fixed-dim renderers are built to this).
    pub height: usize,
}

/// Pick the behavior purely from the snapshot: a Compiling/Loading/Alarm
/// service (via [`featured_loading`]) wins as **Growing**, else any Ready
/// service means **Feeding**, else **Roaming**. This is the pure state machine;
/// [`Snake::update`] layers the loading→ready *burst override* on top so the
/// gold celebration still plays before Feeding takes over.
pub fn choose_behavior(rows: &[ServiceState]) -> Behavior {
    if featured_loading(rows).is_some() {
        Behavior::Growing
    } else if rows.iter().any(|s| s.phase == Phase::Ready) {
        Behavior::Feeding
    } else {
        Behavior::Roaming
    }
}

/// One persistent creature that coordinates the three `[i]`-view behaviors.
pub struct Snake {
    /// The `(width, height)` the fixed-dim renderers were last built for.
    dims: (usize, usize),
    /// Roaming: the drifting model-catalog starfield.
    starfield: ModelStarfield,
    /// Growing: the boxed-snake loading journey (+ gold Ready burst).
    load_snake: LoadSnake,
    /// Feeding: the calm vLLM-driven serving creature.
    serving_creature: ServingCreature,
    /// The behavior resolved by the last `update`, so `behavior()` and `render`
    /// agree within a frame.
    behavior: Behavior,
    /// The currently-fed Ready service key and when it first went Ready, so the
    /// Feeding uptime is real (reset when the fed service changes).
    served_since: Option<(String, Instant)>,
    /// Frame counter advanced only while Roaming, driving the hungry snake's
    /// horizontal drift across the cold starfield.
    roam_frame: u64,
    /// When a featured loading service was last seen, and its last sample. Once
    /// loading, we hold `Growing` for [`LOAD_GRACE`] after the last loading tick
    /// so a single transient snapshot (a probe hiccup that briefly mis-derives
    /// the phase) can't snap the view back to the cold model-catalog mid-load.
    last_loading: Option<Instant>,
    last_featured: Option<ServiceState>,
}

/// How long the loading snake sticks after the last featured-loading tick. The
/// monitor cadence is ~5s, so this rides out a lost tick or two without lingering
/// long once a load truly ends (or hands off to a Ready serving state).
const LOAD_GRACE: Duration = Duration::from_secs(15);

impl Default for Snake {
    fn default() -> Self {
        Self::new()
    }
}

impl Snake {
    /// Create the creature at zero size (the UI resizes it on its first tick).
    pub fn new() -> Self {
        Snake {
            dims: (0, 0),
            starfield: ModelStarfield::new(0, 0),
            load_snake: LoadSnake::new(0, 0),
            serving_creature: ServingCreature::new(),
            behavior: Behavior::Roaming,
            served_since: None,
            roam_frame: 0,
            last_loading: None,
            last_featured: None,
        }
    }

    /// The behavior resolved by the most recent [`update`](Self::update).
    pub fn behavior(&self) -> Behavior {
        self.behavior
    }

    /// Advance the creature one frame from `world`.
    ///
    /// Recreates the fixed-dim sub-renderers on a resize, then resolves the
    /// behavior (pure [`choose_behavior`] plus the loading→ready burst override)
    /// and updates only the active sub-renderer.
    pub fn update(&mut self, world: &SnakeWorld) {
        // Recreate the fixed-dim renderers when the view resizes (mirrors the
        // old inline recreate-on-resize). ServingCreature takes dims at render,
        // so it needs no rebuild here.
        let dims = (world.width, world.height);
        if dims != self.dims {
            self.starfield = ModelStarfield::new(dims.0, dims.1);
            self.load_snake = LoadSnake::new(dims.0, dims.1);
            self.dims = dims;
        }

        // Behavior selection with the burst override, mirroring the old
        // `show_snake` short-circuit: a featured loading service wins; else the
        // LoadSnake keeps Growing while its Ready burst plays; else a Ready
        // service Feeds; else we Roam the catalog. `finish_if_ready` is only
        // called when nothing is loading — its non-Ready arm clears the active
        // journey as a side effect, so calling it mid-load would reset the snake.
        if let Some(featured) = featured_loading(world.rows) {
            self.behavior = Behavior::Growing;
            self.last_loading = Some(Instant::now());
            self.last_featured = Some(featured.clone());
            self.load_snake.update(featured, world.cadence_secs);
        } else if self.load_snake.finish_if_ready(world.rows) {
            // Ready burst still playing — stay Growing so the gold celebration
            // renders before Feeding takes over.
            self.behavior = Behavior::Growing;
        } else if let Some(svc) = world.rows.iter().find(|s| s.phase == Phase::Ready) {
            // A Ready service is a real handoff to serving — end any load grace.
            self.last_loading = None;
            self.behavior = Behavior::Feeding;
            let uptime = self.uptime_for(&svc.key);
            self.serving_creature.update(svc, uptime, world.chips);
        } else if self.within_load_grace() {
            // The featured-loading service vanished from this snapshot but a load
            // was in flight moments ago — almost certainly a transient probe
            // hiccup (one missed tick), not a real end. Keep growing from the
            // last sample instead of snapping back to the cold catalog; we only
            // fall through to Roaming after LOAD_GRACE with no loading and no
            // Ready service (a load that genuinely stopped).
            self.behavior = Behavior::Growing;
            if let Some(f) = self.last_featured.clone() {
                self.load_snake.update(&f, world.cadence_secs);
            }
        } else {
            self.behavior = Behavior::Roaming;
            self.served_since = None;
            self.last_loading = None;
            self.last_featured = None;
            self.roam_frame = self.roam_frame.wrapping_add(1);
            self.starfield.update(world.catalog, world.arch);
        }
    }

    /// True while we're still within [`LOAD_GRACE`] of the last featured-loading
    /// tick — used to hold the loading snake across a transient snapshot gap.
    fn within_load_grace(&self) -> bool {
        self.last_loading.is_some_and(|t| t.elapsed() < LOAD_GRACE)
    }

    /// Elapsed seconds since `key` first became the fed Ready service. Resets
    /// the clock when the fed service changes so uptime tracks the current model.
    fn uptime_for(&mut self, key: &str) -> u64 {
        match &self.served_since {
            Some((k, since)) if k == key => since.elapsed().as_secs(),
            _ => {
                self.served_since = Some((key.to_string(), Instant::now()));
                0
            }
        }
    }

    /// Render the active behavior's sub-renderer to owned Ratatui lines.
    ///
    /// The `width`/`height` match what [`update`](Self::update) built the
    /// fixed-dim renderers with, so Growing/Roaming ignore them (they carry
    /// their own grid) while Feeding sizes its canvas to them here. Never panics.
    pub fn render(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        match self.behavior {
            Behavior::Feeding => self.serving_creature.render(width, height),
            Behavior::Growing => self.load_snake.render(),
            Behavior::Roaming => {
                // The same creature is present in the cold state: a small hungry
                // snake drifts across the starfield of model "food" it can't yet
                // reach. Overlay it on a ground row so the field + footer show.
                let mut lines = self.starfield.render();
                self.overlay_roamer(&mut lines, width);
                lines
            }
        }
    }

    /// Draw the hungry roaming snake onto a "ground" row of the starfield output
    /// (the row just above the footer), leaving the field and footer intact.
    /// A short body trails the drifting head; dim teal so it reads as the same
    /// creature, waiting. No-ops when there's no room. Never panics.
    fn overlay_roamer(&self, lines: &mut Vec<Line<'static>>, width: usize) {
        if width <= ROAM_LEN || lines.len() < 3 {
            return;
        }
        // Head drifts right and wraps; body trails to its left.
        let span = width - ROAM_LEN;
        let head = ROAM_LEN - 1 + (self.roam_frame as usize / 2) % span.max(1);
        let mut row: Vec<char> = vec![' '; width];
        for i in 0..ROAM_LEN {
            let x = head - i;
            row[x] = if i == 0 {
                '\u{25CF}' // ● head
            } else if i == ROAM_LEN - 1 {
                '~' // tail
            } else {
                '\u{2248}' // ≈ body ripple
            };
        }
        let text: String = row.into_iter().collect();
        let ground = lines.len() - 2; // just above the footer
        lines[ground] = Line::from(Span::styled(
            text,
            Style::default().fg(Color::Rgb(80, 160, 150)), // dim hungry teal
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::inference_server::{Phase, Readiness, ServiceState};

    /// Full-field `ServiceState` builder mirroring `inference_load::tests::svc`
    /// (serving: None) — each test overrides only the phase it cares about.
    fn svc(phase: Phase) -> ServiceState {
        ServiceState {
            key: "flux".into(),
            label: "FLUX".into(),
            phase,
            cpu_pct: 0.0,
            rss_bytes: 0,
            rss_delta: 0,
            kernel_count: 0,
            kernel_delta: 0,
            safetensors_fds: 0,
            readiness: Readiness::NotReady,
            top_proc: None,
            last_log: None,
            progress: None,
            flat_ticks: 0,
            serving: None,
        }
    }

    /// Build a `SnakeWorld` over `rows` at a real size, empty catalog.
    fn world<'a>(rows: &'a [ServiceState]) -> SnakeWorld<'a> {
        SnakeWorld {
            rows,
            catalog: &[],
            arch: "Blackhole",
            chips: &[],
            cadence_secs: 2,
            width: 60,
            height: 16,
        }
    }

    #[test]
    fn behavior_follows_the_state_machine() {
        let loading = svc(Phase::Loading);
        let ready = svc(Phase::Ready);
        let down = svc(Phase::Down);
        assert_eq!(choose_behavior(&[]), Behavior::Roaming);
        assert_eq!(
            choose_behavior(std::slice::from_ref(&down)),
            Behavior::Roaming
        );
        assert_eq!(
            choose_behavior(std::slice::from_ref(&loading)),
            Behavior::Growing
        );
        assert_eq!(
            choose_behavior(std::slice::from_ref(&ready)),
            Behavior::Feeding
        );
        // loading wins over a co-present ready
        assert_eq!(choose_behavior(&[ready, loading]), Behavior::Growing);
    }

    #[test]
    fn behavior_matches_choose_behavior_after_update() {
        // Fresh creature: no burst in flight, so update's resolved behavior
        // equals the pure state machine for each simple snapshot.
        let mut s = Snake::new();
        s.update(&world(&[]));
        assert_eq!(s.behavior(), Behavior::Roaming);

        let mut s = Snake::new();
        s.update(&world(std::slice::from_ref(&svc(Phase::Loading))));
        assert_eq!(s.behavior(), Behavior::Growing);

        let mut s = Snake::new();
        s.update(&world(std::slice::from_ref(&svc(Phase::Ready))));
        assert_eq!(s.behavior(), Behavior::Feeding);
    }

    #[test]
    fn ready_burst_keeps_growing_before_feeding() {
        // A service that loads then goes Ready must keep Growing while the gold
        // burst plays (identity carried), not snap straight to Feeding.
        let mut s = Snake::new();
        s.update(&world(std::slice::from_ref(&svc(Phase::Loading))));
        assert_eq!(s.behavior(), Behavior::Growing);
        // Same key now Ready: featured_loading is None, but finish_if_ready
        // returns true for the burst frames → stays Growing.
        let ready = svc(Phase::Ready);
        s.update(&world(std::slice::from_ref(&ready)));
        assert_eq!(s.behavior(), Behavior::Growing, "burst still playing");
    }

    #[test]
    fn roaming_overlays_a_visible_snake_on_the_starfield() {
        // Cold state must show the creature (a ● head), not just the bare field.
        let mut s = Snake::new();
        s.update(&world(&[]));
        let text: String = s
            .render(60, 16)
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
            .collect();
        assert!(text.contains('\u{25CF}'), "roaming snake head ● is drawn");
    }

    #[test]
    fn render_zero_dims_does_not_panic() {
        // At zero size in each behavior, render must not panic.
        let mut s = Snake::new();
        let z = SnakeWorld {
            rows: &[],
            catalog: &[],
            arch: "Unknown",
            chips: &[],
            cadence_secs: 2,
            width: 0,
            height: 0,
        };
        s.update(&z); // Roaming
        let _ = s.render(0, 0);

        let mut s = Snake::new();
        let mut z = z;
        let loading = [svc(Phase::Loading)];
        z.rows = &loading;
        s.update(&z); // Growing
        let _ = s.render(0, 0);

        let mut s = Snake::new();
        let ready = [svc(Phase::Ready)];
        z.rows = &ready;
        s.update(&z); // Feeding
        let _ = s.render(0, 0);
    }
}

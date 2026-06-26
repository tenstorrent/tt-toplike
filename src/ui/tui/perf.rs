// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Live self-instrumentation: actual draw FPS, last frame render time, and the
//! tool's own process CPU%. Off until toggled on, so it costs nothing by default.

use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

pub(crate) fn fps_from(frames: u32, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    frames as f64 / elapsed_secs
}

pub struct PerfMeter {
    sys: System,
    pid: Option<sysinfo::Pid>,
    window_start: Instant,
    frames_in_window: u32,
    fps: f64,
    last_frame_ms: f64,
    cpu_pct: Option<f32>,
    last_cpu_sample: Instant,
}

impl PerfMeter {
    pub fn new() -> Self {
        PerfMeter {
            sys: System::new(),
            pid: sysinfo::get_current_pid().ok(),
            window_start: Instant::now(),
            frames_in_window: 0,
            fps: 0.0,
            last_frame_ms: 0.0,
            cpu_pct: None,
            last_cpu_sample: Instant::now() - Duration::from_secs(1),
        }
    }

    /// Record one drawn frame and its render duration; recompute FPS each ~1s.
    pub fn record_frame(&mut self, render_dur: Duration) {
        self.last_frame_ms = render_dur.as_secs_f64() * 1000.0;
        self.frames_in_window += 1;
        let elapsed = self.window_start.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = fps_from(self.frames_in_window, elapsed);
            self.frames_in_window = 0;
            self.window_start = Instant::now();
        }
    }

    /// Refresh own-PID CPU at most ~1/sec. Call each loop iteration when enabled.
    pub fn maybe_sample_cpu(&mut self) {
        if self.last_cpu_sample.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_cpu_sample = Instant::now();
        if let Some(pid) = self.pid {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu(),
            );
            self.cpu_pct = self.sys.process(pid).map(|p| p.cpu_usage());
        }
    }

    pub fn status_string(&self) -> String {
        let cpu = match self.cpu_pct {
            Some(c) => format!("self {:.1}%", c),
            None => "self —".to_string(),
        };
        format!("{:.1}fps · {:.1}ms · {}", self.fps, self.last_frame_ms, cpu)
    }
}

impl Default for PerfMeter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_is_frames_over_elapsed() {
        assert!((fps_from(10, 1.0) - 10.0).abs() < 1e-9);
        assert_eq!(fps_from(5, 0.0), 0.0); // guard divide-by-zero
    }
}

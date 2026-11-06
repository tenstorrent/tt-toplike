// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests for the TUI's plugin host (`viz_host.rs`).
//!
//! The rendering, history and rate-limit tests register stub vizzes in the
//! process-wide registry, each under an id no other test uses (`vh-...`),
//! because the host reads that registry. The cycle, command and start-up
//! tests use the plain functions with a hand-built list, so no other test
//! can change what is enabled under them.

use super::super::{
    execute_command, next_display_mode, overlay_lines_with, DisplayMode, OverlayPanel,
    ThrottleState, ALL_DISPLAY_MODES,
};
use super::*;
use crate::viz::{SignalSpec, Viz};
use ratatui::{buffer::Buffer, layout::Rect};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tt_toplike_bus::EntityKey;

/// A viz that counts its renders, can panic on demand, and records whether
/// the host's render flag was set while it ran.
struct Stub {
    id: String,
    min: (u16, u16),
    panics: bool,
    signals: Vec<SignalSpec>,
    renders: Arc<AtomicUsize>,
    saw_flag: Arc<AtomicBool>,
}

impl Viz for Stub {
    fn id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        "Stub View"
    }
    fn min_size(&self) -> (u16, u16) {
        self.min
    }
    fn signals(&self) -> Vec<SignalSpec> {
        self.signals.clone()
    }
    fn render(&mut self, buf: &mut Buffer, area: Rect, _ctx: &VizCtx) {
        self.renders.fetch_add(1, Ordering::SeqCst);
        self.saw_flag.store(hook_skips_restore(), Ordering::SeqCst);
        if self.panics {
            panic!("boom in {}", self.id);
        }
        buf.set_string(area.x, area.y, "stub drew", Style::default());
    }
    fn legend(&self) -> Vec<String> {
        vec!["  ● a stub legend line".to_string()]
    }
    fn explain(&self) -> String {
        "The stub explains itself.".to_string()
    }
    fn help(&self) -> String {
        "x  does nothing".to_string()
    }
}

/// What a test reads back from a registered stub.
struct Probe {
    h: VizHandle,
    renders: Arc<AtomicUsize>,
    saw_flag: Arc<AtomicBool>,
}

/// Register a stub under `id` in the global registry.
fn stub(id: &str, min: (u16, u16), panics: bool, signals: &[&str]) -> Probe {
    let renders = Arc::new(AtomicUsize::new(0));
    let saw_flag = Arc::new(AtomicBool::new(false));
    let (r, s) = (renders.clone(), saw_flag.clone());
    let id_owned = id.to_string();
    let signals: Vec<SignalSpec> = signals
        .iter()
        .map(|n| SignalSpec {
            kind: tt_toplike_bus::EntityKind::Chip,
            name: n.to_string(),
        })
        .collect();
    let h = registry::register(
        id,
        "Stub View",
        Source::Folder,
        false,
        Box::new(move || {
            Box::new(Stub {
                id: id_owned.clone(),
                min,
                panics,
                signals: signals.clone(),
                renders: r.clone(),
                saw_flag: s.clone(),
            })
        }),
    )
    .expect("test ids are unique");
    Probe {
        h,
        renders,
        saw_flag,
    }
}

/// A host with no plugin folders and nothing enabled.
fn host() -> VizHost {
    VizHost::with_loader(Loader::new(Vec::new()), &VizSettings::default())
}

/// The buffer's rows as text, trailing spaces trimmed.
fn text(buf: &Buffer) -> String {
    let a = buf.area;
    (a.y..a.y + a.height)
        .map(|y| {
            let row: String = (a.x..a.x + a.width).map(|x| buf[(x, y)].symbol()).collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A listing row for the plain-function tests.
fn row(h: u32, id: &str, by_default: bool) -> VizEntry {
    VizEntry {
        handle: VizHandle(h),
        id: id.to_string(),
        title: format!("Title of {id}"),
        source: Source::Folder,
        enabled_by_default: by_default,
        status: EntryStatus::Ok,
    }
}

#[test]
fn too_small_terminal_shows_needs_message_and_does_not_call_render() {
    let p = stub("vh-too-small", (40, 10), false, &[]);
    let mut host = host();
    let area = Rect::new(0, 0, 30, 5);
    let mut buf = Buffer::empty(area);
    assert_eq!(
        host.render_into(p.h, &mut buf, area, None),
        RenderOutcome::TooSmall
    );
    assert_eq!(p.renders.load(Ordering::SeqCst), 0, "render was called");
    let t = text(&buf);
    assert!(t.contains("needs 40 x 10 (have 30 x 5)"), "{t}");
    // Centred: the line is on the middle row, not the first.
    assert_eq!(
        t.lines().nth(2).unwrap().trim(),
        "needs 40 x 10 (have 30 x 5)"
    );

    // Big enough: render runs (so the counter above can see a call).
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    assert_eq!(
        host.render_into(p.h, &mut buf, area, None),
        RenderOutcome::Drawn
    );
    assert_eq!(p.renders.load(Ordering::SeqCst), 1);
    assert!(text(&buf).starts_with("stub drew"));
}

#[test]
fn the_view_area_leaves_the_status_bar_row() {
    assert_eq!(view_area(Rect::new(0, 0, 80, 24)), Rect::new(0, 0, 80, 23));
    assert_eq!(view_area(Rect::new(0, 0, 80, 0)), Rect::new(0, 0, 80, 0));
}

#[test]
fn panicking_render_is_caught_marks_entry_failed_and_reports_crashed() {
    let p = stub("vh-panics", (1, 1), true, &[]);
    let mut host = host();
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    let out = host.render_into(p.h, &mut buf, area, None);
    let RenderOutcome::Crashed(msg) = &out else {
        panic!("expected Crashed, got {out:?}");
    };
    assert!(
        msg.contains("panicked in render: boom in vh-panics"),
        "{msg}"
    );
    // The panic hook's flag was up during render and is down after.
    assert!(p.saw_flag.load(Ordering::SeqCst), "flag not set in render");
    assert!(!hook_skips_restore(), "flag left set after the panic");
    // The entry is failed and its viz dropped.
    let e = entry(p.h).expect("still listed");
    assert_eq!(e.status, EntryStatus::Failed(msg.clone()));
    assert!(!drawable(p.h));
    // The frame loop's message names the viz.
    let m = host.leave_message(p.h, &out).unwrap();
    assert!(m.starts_with("viz crashed: vh-panics"), "{m}");
    // A second render does not call the viz again (nor panic): it is Gone,
    // and the message carries the failure.
    let again = host.render_into(p.h, &mut buf, area, None);
    assert_eq!(again, RenderOutcome::Gone);
    assert_eq!(p.renders.load(Ordering::SeqCst), 1);
    let m = host.leave_message(p.h, &again).unwrap();
    assert!(m.contains("unavailable: panicked in render: boom"), "{m}");
}

#[test]
fn removed_handle_reports_gone() {
    let p = stub("vh-removed", (1, 1), false, &[]);
    let mut host = host();
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    assert_eq!(
        host.render_into(p.h, &mut buf, area, None),
        RenderOutcome::Drawn
    );
    assert!(registry::remove(p.h));
    let out = host.render_into(p.h, &mut buf, area, None);
    assert_eq!(out, RenderOutcome::Gone);
    assert_eq!(p.renders.load(Ordering::SeqCst), 1);
    assert_eq!(
        host.leave_message(p.h, &out).as_deref(),
        Some("viz vh-removed was removed")
    );
    assert_eq!(host.leave_message(p.h, &RenderOutcome::Drawn), None);
}

#[test]
fn history_is_recorded_only_for_the_active_viz() {
    let a = stub("vh-hist-a", (1, 1), false, &["vh_sig_a"]);
    let b = stub("vh-hist-b", (1, 1), false, &["vh_sig_b"]);
    let chip = EntityKey::chip("0000:01:00.0");
    let world = || {
        crate::viz::testkit::WorldBuilder::new()
            .sample("0000:01:00.0", "vh_sig_a", 1.0)
            .sample("0000:01:00.0", "vh_sig_b", 2.0)
            .build()
    };
    let mut host = host();
    let t0 = Instant::now();
    let len = |host: &VizHost, s: &str| host.history().series(&chip, s).len();

    // A on screen for 8 ticks: two samples of A's signal, none of B's.
    for _ in 0..8 {
        host.tick(t0, DisplayMode::Plugin(a.h), world);
    }
    assert_eq!(len(&host, "vh_sig_a"), 2);
    assert_eq!(len(&host, "vh_sig_b"), 0);

    // No plugin on screen: the world is never even read.
    let reads = AtomicUsize::new(0);
    for _ in 0..8 {
        host.tick(t0, DisplayMode::Insights, || {
            reads.fetch_add(1, Ordering::SeqCst);
            world()
        });
    }
    assert_eq!(reads.load(Ordering::SeqCst), 0, "a hidden viz cost a read");
    assert_eq!(len(&host, "vh_sig_a"), 2);

    // B on screen: B's signal now, A's unchanged.
    for _ in 0..8 {
        host.tick(t0, DisplayMode::Plugin(b.h), world);
    }
    assert_eq!(len(&host, "vh_sig_a"), 2);
    assert_eq!(len(&host, "vh_sig_b"), 2);
    assert_eq!(host.history().series(&chip, "vh_sig_b"), &[2.0, 2.0]);
}

#[test]
fn the_loader_is_polled_at_most_once_every_two_seconds() {
    let mut host = host();
    let t0 = Instant::now();
    let no_world = || -> LocalView { panic!("no plugin is on screen") };
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut polls = Vec::new();
    for ms in [0, 16, 1000, 1999, 2000, 2016, 3999, 4000, 4001] {
        host.tick(at(ms), DisplayMode::Insights, no_world);
        polls.push(host.polls());
    }
    assert_eq!(polls, vec![1, 1, 1, 1, 2, 2, 2, 3, 3]);
}

#[test]
fn cycle_goes_from_die3d_into_plugins_in_id_order_then_to_insights() {
    // Listed in id order, as the registry lists them.
    let entries = vec![
        row(10, "a-enabled", false),
        row(11, "b-default", true),
        row(12, "c-hidden-example", false),
        row(13, "d-placeholder", true),
    ];
    let enabled: BTreeSet<String> = ["a-enabled".to_string()].into();
    let list = cycle_list(&entries, &enabled, |h| h != VizHandle(13));
    assert_eq!(list, vec![VizHandle(10), VizHandle(11)]);

    let (a, b) = (
        DisplayMode::Plugin(VizHandle(10)),
        DisplayMode::Plugin(VizHandle(11)),
    );
    assert_eq!(next_mode_in(DisplayMode::Die3d, &list), a);
    assert_eq!(next_mode_in(a, &list), b);
    assert_eq!(next_mode_in(b, &list), DisplayMode::Insights);
    // A plugin that left the list (disabled, crashed) also goes to Insights.
    assert_eq!(
        next_mode_in(DisplayMode::Plugin(VizHandle(13)), &list),
        DisplayMode::Insights
    );
    assert_eq!(
        cycle_after_in(DisplayMode::Die3d, &list),
        Some(VizHandle(10))
    );
    assert_eq!(cycle_after_in(b, &list), None);
    assert_eq!(cycle_after_in(DisplayMode::Grid, &list), None);

    // The whole `v` walk from Insights: the built-ins in today's order,
    // then the plugins, then round again.
    let mut m = DisplayMode::Insights;
    let mut walk = Vec::new();
    for _ in 0..10 {
        m = next_mode_in(m, &list);
        walk.push(m);
    }
    assert_eq!(
        walk,
        vec![
            DisplayMode::MemoryFlow,
            DisplayMode::Starfield,
            DisplayMode::MemoryCastle,
            DisplayMode::Arcade,
            DisplayMode::Defrag,
            DisplayMode::InferenceMonitor,
            DisplayMode::Die3d,
            a,
            b,
            DisplayMode::Insights,
        ]
    );
}

#[test]
fn cycle_is_unchanged_when_no_plugin_is_enabled() {
    // Today's `v` table, written out, so a change to either side shows.
    let today = [
        (DisplayMode::Insights, DisplayMode::MemoryFlow),
        (DisplayMode::Grid, DisplayMode::MemoryFlow),
        (DisplayMode::Starfield, DisplayMode::MemoryCastle),
        (DisplayMode::MemoryCastle, DisplayMode::Arcade),
        (DisplayMode::MemoryFlow, DisplayMode::Starfield),
        (DisplayMode::Arcade, DisplayMode::Defrag),
        (DisplayMode::Defrag, DisplayMode::InferenceMonitor),
        (DisplayMode::InferenceMonitor, DisplayMode::Die3d),
        (DisplayMode::HivemindSweeper, DisplayMode::Insights),
        (DisplayMode::Training, DisplayMode::Insights),
        (DisplayMode::Die3d, DisplayMode::Insights),
        (DisplayMode::Saga, DisplayMode::Insights),
    ];
    assert_eq!(today.len(), ALL_DISPLAY_MODES.len());
    for (from, to) in today {
        assert_eq!(next_display_mode(from), to, "next_display_mode({from:?})");
        assert_eq!(next_mode_in(from, &[]), to, "next_mode_in({from:?}, [])");
    }
    for m in ALL_DISPLAY_MODES {
        assert_eq!(next_mode_in(m, &[]), next_display_mode(m));
    }
    // Entries that are hidden or not drawable leave the cycle empty too.
    let entries = vec![row(20, "x-hidden", false), row(21, "y-broken", true)];
    let list = cycle_list(&entries, &BTreeSet::new(), |h| h != VizHandle(21));
    assert!(list.is_empty());
}

#[test]
fn slash_mode_by_id_switches_and_unknown_id_is_an_error_line() {
    let rows = vec![
        (VizHandle(30), "a-viz".to_string(), "A View".to_string()),
        (VizHandle(31), "b-viz".to_string(), "B View".to_string()),
    ];
    // As the Enter handler runs it: `execute_command`, then the fallthrough.
    let run = |cmd: &str, rows: &[(VizHandle, String, String)]| {
        let mut mode = DisplayMode::Insights;
        let (mut ap, mut dp) = (Duration::from_millis(16), Duration::from_millis(250));
        let (mut ov, mut ts) = (None, ThrottleState::new(false, false));
        let (msg, err, _) =
            execute_command(cmd, &mut mode, &mut ap, &mut dp, 1.0, &mut ov, &mut ts);
        let (msg, err) = command_in(cmd, (msg, err), &mut mode, rows);
        (mode, msg, err)
    };

    let (mode, msg, err) = run("/mode a-viz", &rows);
    assert_eq!((mode, err), (DisplayMode::Plugin(VizHandle(30)), false));
    assert_eq!(msg, "→ A View");
    let (mode, _, err) = run("/viz b-viz", &rows);
    assert_eq!((mode, err), (DisplayMode::Plugin(VizHandle(31)), false));
    // Built-in names win and are untouched.
    let (mode, msg, err) = run("/mode grid", &rows);
    assert_eq!(
        (mode, msg.as_str(), err),
        (DisplayMode::Grid, "→ grid", false)
    );

    // Unknown ids: an error line naming the enabled plugins; mode unchanged.
    let (mode, msg, err) = run("/mode nope", &rows);
    assert!(err);
    assert_eq!(mode, DisplayMode::Insights);
    assert!(msg.starts_with("mode: insights|grid"), "{msg}");
    assert!(msg.ends_with("; plugins: a-viz|b-viz"), "{msg}");
    let (mode, msg, err) = run("/viz nope", &rows);
    assert!(err);
    assert_eq!(mode, DisplayMode::Insights);
    assert_eq!(msg, "viz: no enabled plugin `nope`; plugins: a-viz|b-viz");
    let (_, msg, err) = run("/viz", &[]);
    assert!(err);
    assert!(msg.ends_with("plugins: none enabled"), "{msg}");

    // Other commands pass through as they were.
    let (_, msg, err) = run("/fps 30", &rows);
    assert_eq!((msg.as_str(), err), ("animation fps set to 30", false));
    let (_, msg, err) = run("/bogus", &rows);
    assert!(err && msg.starts_with("unknown command"), "{msg}");
}

#[test]
fn an_unknown_viz_flag_is_an_error_listing_every_id_and_starts_nothing() {
    use crate::cli::StartView;
    let mut hidden = row(40, "example-pulse", false);
    hidden.source = Source::File("/x/viz/examples/thermal.yaml".into());
    let entries = vec![row(41, "a-viz", true), hidden];

    let err = resolve_start(StartView::Viz("nope".into()), &entries).unwrap_err();
    assert_eq!(
        err,
        "unknown viz `nope`; available: a-viz (rust), example-pulse (/x/viz/examples/thermal.yaml)"
    );
    assert!(resolve_start(StartView::Viz(String::new()), &entries).is_err());
    assert!(resolve_start(StartView::Viz("x".into()), &[])
        .unwrap_err()
        .ends_with("available: none"));

    // A known id starts on it, even one that is off by default.
    assert_eq!(
        resolve_start(StartView::Viz("example-pulse".into()), &entries),
        Ok(DisplayMode::Plugin(VizHandle(40)))
    );
    assert_eq!(
        resolve_start(
            StartView::Mode(crate::cli::VisualizationMode::Saga),
            &entries
        ),
        Ok(DisplayMode::Saga)
    );
    assert_eq!(
        resolve_start(StartView::Default, &entries),
        Ok(DisplayMode::Insights)
    );
}

#[test]
fn viz_flag_enables_a_hidden_plugin_for_the_run() {
    let p = stub("vh-flag-hidden", (1, 1), false, &[]);
    let mut host = host();
    assert!(!host.plugins().contains(&p.h), "hidden until named");
    let cli = <crate::cli::Cli as clap::Parser>::try_parse_from([
        "tt-toplike",
        "--mode",
        "saga",
        "--viz",
        "vh-flag-hidden",
    ])
    .unwrap();
    assert_eq!(host.start_mode(&cli), Ok(DisplayMode::Plugin(p.h)));
    assert!(
        host.plugins().contains(&p.h),
        "--viz enables it for the run"
    );
    // And an unknown one is refused.
    let cli = <crate::cli::Cli as clap::Parser>::try_parse_from(["tt-toplike", "--viz", "vh-none"])
        .unwrap();
    assert!(host
        .start_mode(&cli)
        .unwrap_err()
        .contains("unknown viz `vh-none`"));
}

#[test]
fn enable_list_raises_one_problem_per_unknown_id() {
    let p = stub("vh-enabled-ok", (1, 1), false, &[]);
    let config = VizSettings {
        enable: vec![
            "vh-not-there".into(),
            "vh-enabled-ok".into(),
            "vh-not-there".into(),
        ],
        per_viz: Default::default(),
    };
    let host = VizHost::with_loader(Loader::new(Vec::new()), &config);
    assert_eq!(
        host.problems(),
        ["[viz] enable: no viz has the id `vh-not-there`"]
    );
    assert_eq!(
        host.status_problem(),
        Some((
            "config: [viz] enable: no viz has the id `vh-not-there`".to_string(),
            true
        ))
    );
    assert!(
        host.plugins().contains(&p.h),
        "an enabled id joins the cycle"
    );
}

#[test]
fn plugin_text_feeds_the_status_bar_and_the_overlays() {
    let p = stub("vh-overlays", (1, 1), false, &[]);
    let line_text = |l: &Line| {
        l.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    };
    let all = |ls: &[Line]| ls.iter().map(line_text).collect::<Vec<_>>().join("\n");
    let c = Color::Reset;

    let g = statusbar_group(DisplayMode::Plugin(p.h), Style::default(), Style::default()).unwrap();
    let g: String = g.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(g, " Esc  leave Stub View");
    assert!(statusbar_group(DisplayMode::Insights, Style::default(), Style::default()).is_none());

    let legend = all(&legend_overlay(p.h, c, c, c));
    assert_eq!(legend, "║   ● a stub legend line");
    let explain = all(&explain_overlay(p.h, [c; 4], 60));
    assert!(explain.contains("Stub View"), "{explain}");
    assert!(explain.contains("The stub explains itself."), "{explain}");
    let help = all(&help_overlay(p.h, [c; 5], 60, Vec::new()));
    assert!(help.contains("Stub View keys"), "{help}");
    assert!(help.contains("x  does nothing"), "{help}");

    // The mod.rs overlay builders route a plugin mode here.
    let mode = DisplayMode::Plugin(p.h);
    let backdrop = (None, false);
    let legend = all(&overlay_lines_with(
        OverlayPanel::Legend,
        mode,
        60,
        backdrop,
        &[],
    ));
    assert!(legend.contains("a stub legend line"), "{legend}");
    let explain = all(&overlay_lines_with(
        OverlayPanel::Explain,
        mode,
        60,
        backdrop,
        &[],
    ));
    assert!(explain.contains("The stub explains itself."), "{explain}");
    let help = all(&overlay_lines_with(
        OverlayPanel::Help,
        mode,
        60,
        backdrop,
        &[],
    ));
    assert!(help.contains("x  does nothing"), "{help}");
    assert!(help.contains("next view"), "the global keys follow: {help}");
}

#[test]
fn help_lists_plugin_views_only_when_one_is_enabled() {
    let all = |ls: &[Line]| {
        ls.iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let backdrop = (None, false);
    let rows = vec![("a-viz".to_string(), "A View".to_string())];
    let with = all(&overlay_lines_with(
        OverlayPanel::Help,
        DisplayMode::Insights,
        80,
        backdrop,
        &rows,
    ));
    assert!(with.contains("Plugin views"), "{with}");
    assert!(with.contains(" a-viz  A View"), "{with}");
    let without = all(&overlay_lines_with(
        OverlayPanel::Help,
        DisplayMode::Insights,
        80,
        backdrop,
        &[],
    ));
    assert!(!without.contains("Plugin views"));
    let c = Color::Reset;
    assert!(plugin_views_section(&[], c, c, c, c, c).is_empty());
    // The host offers rows only for the help panel.
    assert!(host().help_rows(OverlayPanel::Legend).is_empty());
}

/// A viz that panics in one named method, counts every call into it, and
/// records whether the registry lock was held on this thread when it was
/// dropped.
struct Faulty {
    id: String,
    panic_in: &'static str,
    calls: Arc<AtomicUsize>,
    dropped_under_lock: Arc<std::sync::Mutex<Option<bool>>>,
}

impl Faulty {
    fn hit(&self, method: &str) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.panic_in == method {
            panic!("{method} blew up");
        }
    }
}

impl Drop for Faulty {
    fn drop(&mut self) {
        let mut d = self.dropped_under_lock.lock().unwrap();
        if d.is_none() {
            *d = Some(registry::this_thread_holds_global());
        }
    }
}

impl Viz for Faulty {
    fn id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        "Faulty"
    }
    fn min_size(&self) -> (u16, u16) {
        self.hit("min_size");
        (1, 1)
    }
    fn signals(&self) -> Vec<SignalSpec> {
        self.hit("signals");
        vec![SignalSpec {
            kind: tt_toplike_bus::EntityKind::Chip,
            name: "vh_faulty".into(),
        }]
    }
    fn render(&mut self, _buf: &mut Buffer, _area: Rect, _ctx: &VizCtx) {
        self.hit("render");
    }
    fn legend(&self) -> Vec<String> {
        self.hit("legend");
        vec!["legend".into()]
    }
    fn explain(&self) -> String {
        self.hit("explain");
        "explain".into()
    }
    fn help(&self) -> String {
        self.hit("help");
        "help".into()
    }
    fn configure(&mut self, _cfg: &toml_edit::Table) {
        self.hit("configure");
    }
}

/// Every plugin method the host calls is guarded: a panic in any of them
/// does not unwind out of the host, turns the plugin off (Failed, no viz),
/// drops the viz outside the registry lock, and the next call does not
/// reach the plugin (so it cannot panic again).
#[test]
fn a_panic_in_any_plugin_method_turns_the_plugin_off_and_does_not_unwind() {
    let methods = [
        "min_size",
        "render",
        "signals",
        "legend",
        "explain",
        "help",
        "configure",
    ];
    for method in methods {
        let id = format!("vh-faulty-{}", method.replace('_', "-"));
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(std::sync::Mutex::new(None));
        let (c, d, i) = (calls.clone(), dropped.clone(), id.clone());
        let h = registry::register(
            &id,
            "Faulty",
            Source::Folder,
            false,
            Box::new(move || {
                Box::new(Faulty {
                    id: i.clone(),
                    panic_in: method,
                    calls: c.clone(),
                    dropped_under_lock: d.clone(),
                })
            }),
        )
        .unwrap();
        let mut per_viz = BTreeMap::new();
        per_viz.insert(id.clone(), toml_edit::Table::new());
        let config = VizSettings {
            enable: vec![id.clone()],
            per_viz,
        };
        let mut host = VizHost::with_loader(Loader::new(Vec::new()), &config);
        let world = || {
            crate::viz::testkit::WorldBuilder::new()
                .sample("0000:01:00.0", "vh_faulty", 1.0)
                .build()
        };
        let area = Rect::new(0, 0, 20, 4);
        let c = Color::Reset;
        // Each call returns normally, whether or not the plugin panics.
        let exercise = |host: &mut VizHost| match method {
            "min_size" | "render" => {
                let mut buf = Buffer::empty(area);
                let _ = host.render_into(h, &mut buf, area, None);
            }
            "signals" => {
                for _ in 0..HISTORY_EVERY {
                    host.tick(Instant::now(), DisplayMode::Plugin(h), world);
                }
            }
            "legend" => {
                let lines = legend_overlay(h, c, c, c);
                assert!(!lines.is_empty(), "{method}: a fallback line");
            }
            "explain" => {
                let _ = explain_overlay(h, [c; 4], 60);
            }
            "help" => {
                let _ = help_overlay(h, [c; 5], 60, Vec::new());
            }
            "configure" => host.configure(h),
            _ => unreachable!(),
        };
        exercise(&mut host);
        assert!(!hook_skips_restore(), "{method}: flag left set");
        let e = entry(h).expect("still listed");
        let EntryStatus::Failed(msg) = &e.status else {
            panic!("{method}: entry not failed: {:?}", e.status);
        };
        assert!(
            msg.starts_with(&format!("panicked in {method}: {method} blew up")),
            "{method}: {msg}"
        );
        assert!(!drawable(h), "{method}: the viz is still there");
        assert!(!host.plugins().contains(&h), "{method}: still in the cycle");
        assert_eq!(
            *dropped.lock().unwrap(),
            Some(false),
            "{method}: the viz was not dropped, or was dropped under the registry lock"
        );
        // Again: the plugin is not reached, so it cannot panic again.
        let before = calls.load(Ordering::SeqCst);
        exercise(&mut host);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            before,
            "{method}: called again"
        );
    }
}

#[test]
fn the_panic_hook_skips_the_restore_only_inside_a_plugin_call() {
    assert!(
        skips_restore(true),
        "a plugin panic: leave the terminal alone"
    );
    assert!(!skips_restore(false), "any other panic: the normal path");
    assert!(!hook_skips_restore(), "outside a plugin call");
    // Inside one, the stub sees it set (see the panicking-render test too).
    let p = stub("vh-hook-flag", (1, 1), false, &[]);
    let area = Rect::new(0, 0, 20, 4);
    let mut buf = Buffer::empty(area);
    assert_eq!(
        host().render_into(p.h, &mut buf, area, None),
        RenderOutcome::Drawn
    );
    assert!(p.saw_flag.load(Ordering::SeqCst));
    assert!(!hook_skips_restore());
}

#[test]
fn help_rows_list_the_enabled_plugins() {
    let p = stub("vh-help-rows", (1, 1), false, &[]);
    let config = VizSettings {
        enable: vec!["vh-help-rows".into()],
        per_viz: Default::default(),
    };
    let host = VizHost::with_loader(Loader::new(Vec::new()), &config);
    let rows = host.help_rows(OverlayPanel::Help);
    assert!(
        rows.contains(&("vh-help-rows".to_string(), "Stub View".to_string())),
        "{rows:?}"
    );
    assert!(host.help_rows(OverlayPanel::Legend).is_empty());
    let _ = p;
}

#[test]
fn the_status_bar_title_comes_from_the_cache() {
    let p = stub("vh-title-cache", (1, 1), false, &[]);
    assert_eq!(name_of(p.h), "Stub View");
    // Removed from the registry: the cached names still say who it was.
    assert!(registry::remove(p.h));
    assert_eq!(name_of(p.h), "Stub View");
    assert_eq!(id_of(p.h), "vh-title-cache");
    assert_eq!(name_of(VizHandle(u32::MAX)), format!("#{}", u32::MAX));
}

/// `--viz <id>` naming a registered entry that failed to load (a YAML file
/// that never loaded: an entry with no viz behind it) is not refused at
/// start-up. It resolves to that plugin, the first render finds no viz, and
/// `leave_message` carries the reason. `run_tui` treats a `Some` message as
/// "go back to Insights and show it" (the arm after `terminal.draw`), so
/// the user lands on Insights with the reason on the status line.
#[test]
fn viz_flag_naming_a_failed_entry_resolves_then_leaves_with_the_reason() {
    use crate::cli::StartView;
    let p = stub("vh-failed-file", (1, 1), false, &[]);
    assert!(registry::mark_failed(
        p.h,
        "bad.yaml:3: unknown widget `gauge2`"
    ));

    // Start-up accepts the id: it is registered, whatever its status.
    let entries = registry::entries();
    assert_eq!(
        resolve_start(StartView::Viz("vh-failed-file".into()), &entries),
        Ok(DisplayMode::Plugin(p.h))
    );

    // The render finds no viz behind the handle and draws nothing.
    let mut host = host();
    let area = Rect::new(0, 0, 40, 5);
    let mut buf = Buffer::empty(area);
    let outcome = host.render_into(p.h, &mut buf, area, None);
    assert_eq!(outcome, RenderOutcome::Gone);
    assert_eq!(p.renders.load(Ordering::SeqCst), 0, "no viz ran");

    // The loop's trigger: a message that names the id and the reason.
    assert_eq!(
        host.leave_message(p.h, &outcome),
        Some("viz vh-failed-file is unavailable: bad.yaml:3: unknown widget `gauge2`".to_string())
    );
}

/// The host drops the history of an entity that has left the world, for the
/// kinds it just recorded.
#[test]
fn record_forgets_entities_that_left_the_world() {
    use crate::viz::testkit::WorldBuilder;
    let p = stub("vh-forget", (1, 1), false, &["power_w"]);
    let mut host = host();
    let (a, b, c) = ("0000:01:00.0", "0000:02:00.0", "0000:03:00.0");
    let three = WorldBuilder::new()
        .sample(a, "power_w", 1.0)
        .sample(b, "power_w", 2.0)
        .sample(c, "power_w", 3.0)
        .build();
    host.record(p.h, &three);
    for bus in [a, b, c] {
        assert_eq!(
            host.history()
                .series(&EntityKey::chip(bus), "power_w")
                .len(),
            1
        );
    }
    let two = WorldBuilder::new()
        .sample(a, "power_w", 1.5)
        .sample(c, "power_w", 3.5)
        .build();
    host.record(p.h, &two);
    assert!(host
        .history()
        .series(&EntityKey::chip(b), "power_w")
        .is_empty());
    assert_eq!(
        host.history().series(&EntityKey::chip(a), "power_w"),
        [1.0, 1.5]
    );
    assert_eq!(
        host.history().series(&EntityKey::chip(c), "power_w"),
        [3.0, 3.5]
    );
}

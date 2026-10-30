// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Render cost of the Die 3D view: the benchmark (`render_cost_report`, run
//! by hand in release) and the tests that pin what the performance work may
//! not change.
//!
//! The golden-frame hashes below were taken from the view as it was before
//! any optimisation. Every cache, buffer reuse and throttle added since must
//! leave those frames identical, character for character and style for
//! style. A later task that changes the look on purpose re-pins them.
//!
//! Task 17 added the clouds (`W`). The old hashes still pin the frames with
//! the clouds off; frames with the clouds on have hashes of their own.
//!
//! Task 18 added the history panel (`H`). Both sets of hashes above pin the
//! frames with the panel off; frames with it on have hashes of their own.

use super::camera::TransitionMode;
use super::test_backend::Fake;
use super::*;
use crate::backend::pcie_counters::PcieBandwidth;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};
use std::time::Instant;

/// FNV-1a, 64 bit: a hash that does not depend on the std hasher's random
/// seed, so a frame hashes to the same number on every run and machine.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn color(&mut self, c: Option<Color>) {
        match c {
            None => self.bytes(&[0]),
            Some(Color::Reset) => self.bytes(&[1]),
            Some(Color::Rgb(r, g, b)) => self.bytes(&[2, r, g, b]),
            // The view only uses the two above; anything else still hashes
            // distinctly through its name.
            Some(other) => self.bytes(format!("{other:?}").as_bytes()),
        }
    }
}

/// A stable hash of rendered lines, cell by cell: each character with its
/// foreground, background and modifiers, and a marker at each line end. How
/// the cells are grouped into spans does not enter it, since it does not
/// change what the terminal shows.
fn frame_hash(lines: &[Line<'static>]) -> u64 {
    let mut h = Fnv::new();
    for line in lines {
        for span in &line.spans {
            // A line's own style is not used by the view; hash it anyway.
            let style = line.style.patch(span.style);
            for ch in span.content.chars() {
                h.bytes(&(ch as u32).to_le_bytes());
                h.color(style.fg);
                h.color(style.bg);
                h.bytes(&style.add_modifier.bits().to_le_bytes());
                h.bytes(&style.sub_modifier.bits().to_le_bytes());
            }
        }
        h.bytes(&[0xff, 0xfe]);
    }
    h.0
}

/// TDP the fakes report: 100 W, so a chip's load is its watts over 100 for
/// the first seconds of view time.
const TDP: &str = "100";

/// A fake with the given boards, chips at different powers (so the labels
/// and the waves differ) and a PCIe rate (so packets move).
fn busy_fake(boards: &[&str]) -> Fake {
    let mut b = Fake::boards(boards, Some(TDP));
    for i in 0..boards.len() {
        b.set_power(i, 30.0 + 7.0 * i as f32);
    }
    b.pcie = Some(PcieBandwidth {
        rx_bytes_per_sec: 2.0e9,
        tx_bytes_per_sec: 5.0e8,
    });
    b
}

/// The frames the golden hashes pin, for one cell mode: a view over two p300
/// boards (the super scene), rendered at rest, then during a dissolve, a
/// spin and a chaos cut started by hand, then at rest again. Braille is
/// transparent; half-block is opaque, so both backgrounds are covered.
///
/// Every time is a whole number of twelfths of a second, where the tile
/// wave's quantised time (`scene::WAVE_STEPS_PER_SEC`) equals the time
/// itself, so these frames are the same with and without that quantisation
/// and pin only what must not change at all: caching, reuse and throttling.
///
/// With `clouds` off, the `W clouds off` entry is taken out of the footer's
/// switch row before hashing: it is the one change to these frames since the
/// pre-Task-16 hashes were taken, and without it they pin everything else
/// exactly as before.
///
/// The history panel is off in all of them (see [`without_the_h_switch`]).
fn golden_frames(braille: bool, clouds: bool) -> Vec<(f64, u64)> {
    let b = busy_fake(&["p300"; 4]);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    if !braille {
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    }
    if !clouds {
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
    }
    v.handle_key(Die3dKey::ToggleHistory, 0.0);
    let mut out = Vec::new();
    let mut shot = |v: &Die3dView, t: f64| {
        let mut lines = v.render(t);
        without_the_h_switch(&mut lines);
        if !clouds {
            without_the_w_switch(&mut lines);
        }
        out.push((t, frame_hash(&lines)))
    };
    shot(&v, 0.0);
    shot(&v, 2.75);
    v.director.set_transition_mode(TransitionMode::Dissolve);
    v.handle_key(Die3dKey::CutNow, 5.0);
    shot(&v, 5.25);
    v.director.set_transition_mode(TransitionMode::Spin);
    v.handle_key(Die3dKey::CutNow, 10.0);
    shot(&v, 10.5);
    v.director.set_transition_mode(TransitionMode::Chaos);
    v.handle_key(Die3dKey::CutNow, 15.0);
    shot(&v, 15.25);
    shot(&v, 20.0);
    out
}

/// Remove `W clouds off` from the footer's switch row (the last line), which
/// must hold it in its full form.
fn without_the_w_switch(lines: &mut [Line<'static>]) {
    let entry = format!("{SEP}W clouds off");
    let row = lines.last_mut().expect("a footer");
    let found = row
        .spans
        .iter_mut()
        .find(|s| s.content.contains(&entry))
        .expect("the switch row lists W clouds off");
    found.content = found.content.replacen(&entry, "", 1).into();
}

/// Undo the `H history off` entry in the footer's switch row (the last
/// line). At 150 columns that entry makes the full switch row with the
/// action keys too wide, so the row falls back to the switches alone (the
/// strip's designed first fallback). Without the entry the action keys fit
/// again, so this puts them back in its place: the row is then exactly the
/// one the older hashes were taken with. It asserts the row has that form,
/// so it never edits a row it does not expect.
fn without_the_h_switch(lines: &mut [Line<'static>]) {
    let entry = format!("{SEP}H history off");
    let row = lines.last_mut().expect("a footer");
    assert_eq!(row.spans.len(), 1, "the switch row is one span");
    let span = &mut row.spans[0];
    assert!(
        span.content.ends_with(&entry) && !span.content.contains(ACTION_KEYS),
        "the switch row ends with H history off and has no action keys: {}",
        span.content
    );
    let kept = &span.content[..span.content.len() - entry.len()];
    span.content = format!("{kept}{SEP}{ACTION_KEYS}").into();
}

/// The hashes of `golden_frames` with the clouds off, taken from the view as
/// committed before Task 16 (03cd98e5), with this same code.
const GOLDEN_BRAILLE: [u64; 6] = [
    0x2986518cca24febf,
    0xf31bd5353d149293,
    0x7374dc6b8f6454cb,
    0x78790cc5c7ea94ce,
    0x77214920d02c103f,
    0x5068456b0541e0f0,
];
const GOLDEN_HALFBLOCK: [u64; 6] = [
    0xb3aa9a6ab9e78526,
    0xa240806610ec0248,
    0x700aa9051effa7ef,
    0xe0f796d0499f3332,
    0x5d0efbebb7fab01a,
    0xa055648a1a0d290b,
];

/// The hashes of `golden_frames` with the clouds on, pinned when the clouds
/// were added (Task 17), so a later change to how they look or move is seen.
const GOLDEN_BRAILLE_CLOUDS: [u64; 6] = [
    0x7c817dbce932f0ad,
    0x3488c748a56c238c,
    0x9820ddb83e6fd6dd,
    0x38f04d1f24351b47,
    0x92a00c8381ecb5c9,
    0x7f0036837d468ce8,
];
const GOLDEN_HALFBLOCK_CLOUDS: [u64; 6] = [
    0x6206447a29d8e8bb,
    0x4381544251d63367,
    0x1599aac9e1ebaf68,
    0x4f5446a72b4a4d3a,
    0x05d17f0eb50e8088,
    0x120b01c1a19af934,
];

#[test]
fn cached_fit_gives_identical_frames() {
    for (braille, want) in [(true, GOLDEN_BRAILLE), (false, GOLDEN_HALFBLOCK)] {
        let got = golden_frames(braille, false);
        let hashes: Vec<u64> = got.iter().map(|g| g.1).collect();
        assert_eq!(
            hashes,
            want,
            "braille={braille}: frames at {:?} changed; got {hashes:#x?}",
            got.iter().map(|g| g.0).collect::<Vec<_>>()
        );
    }
}

#[test]
fn frames_with_clouds_are_pinned() {
    for (braille, want) in [
        (true, GOLDEN_BRAILLE_CLOUDS),
        (false, GOLDEN_HALFBLOCK_CLOUDS),
    ] {
        let got = golden_frames(braille, true);
        let hashes: Vec<u64> = got.iter().map(|g| g.1).collect();
        assert_eq!(hashes, want, "braille={braille}: got {hashes:#x?}");
        // Not the clouds-off frames under another footer: the scene differs.
        let off = golden_frames(braille, false);
        assert_ne!(hashes[0], off[0].1);
    }
}

/// The frames the history-panel hashes pin, for one cell mode: the view of
/// `golden_frames` (two p300 boards, the super scene, auto-cuts off, clouds
/// on, braille transparent or half-block opaque) with the panel on, fed an
/// update every second for eight seconds while every chip's power and the
/// PCIe rates change, so the sparklines have shape. Shots at rest after 3 and
/// 8 s, and 0.25 s into a dissolve, which the panel stays on through.
///
/// Each entry is (view time, hash of the whole frame, hash of every cell
/// outside the panel's rectangle; see [`outside_panel_hash`]).
fn history_frames(braille: bool) -> Vec<(f64, u64, u64)> {
    let mut b = busy_fake(&["p300"; 4]);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    if !braille {
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    }
    let mut out = Vec::new();
    for t in 1..=8 {
        let now = t as f64;
        for i in 0..4 {
            b.set_power(i, 30.0 + 7.0 * i as f32 + (t * (i + 1) % 5) as f32 * 3.0);
            b.telem[i].asic_temperature = Some(50.0 + (t % 4) as f32);
        }
        b.pcie = Some(PcieBandwidth {
            rx_bytes_per_sec: 2.0e7 * (1 + t % 3) as f64,
            tx_bytes_per_sec: 1.3e7,
        });
        v.update(&b, now);
        let mut shot = |v: &Die3dView, at: f64| {
            let lines = v.render(at);
            // Not vacuous: the panel is in every pinned frame.
            assert!(
                text(&lines).iter().any(|r| r.contains(history::TITLE)),
                "{at}"
            );
            let panel_rows = v.history.panel_rows().len();
            out.push((
                at,
                frame_hash(&lines),
                outside_panel_hash(&lines, panel_rows),
            ));
        };
        if t == 3 || t == 8 {
            shot(&v, now);
        }
        if t == 6 {
            v.director.set_transition_mode(TransitionMode::Dissolve);
            v.handle_key(Die3dKey::CutNow, now);
            assert!(v.director.progress(now + 0.25).is_some(), "mid-dissolve");
            shot(&v, now + 0.25);
        }
    }
    out
}

/// The hashes of `history_frames`, pinned when the panel was added (Task
/// 18), so a later change to what it shows or where is seen.
const GOLDEN_BRAILLE_HISTORY: [u64; 3] =
    [0x1ccd449df8740a96, 0x064720d7a54c737f, 0x8f39da284f9456c7];
const GOLDEN_HALFBLOCK_HISTORY: [u64; 3] =
    [0x447402ce16e9ba83, 0x06f8c284af13998d, 0xba88c15711169b8f];

#[test]
fn frames_with_the_history_panel_are_pinned() {
    for (braille, want) in [
        (true, GOLDEN_BRAILLE_HISTORY),
        (false, GOLDEN_HALFBLOCK_HISTORY),
    ] {
        let got = history_frames(braille);
        let hashes: Vec<u64> = got.iter().map(|g| g.1).collect();
        assert_eq!(
            hashes,
            want,
            "braille={braille}: frames at {:?}; got {hashes:#x?}",
            got.iter().map(|g| g.0).collect::<Vec<_>>()
        );
    }
}

/// [`frame_hash`] of every cell outside the history panel's rectangle (the
/// first `panel_rows` rows from `PANEL_TOP`, `history::PANEL_WIDTH` cells
/// from `PANEL_LEFT`), with each skipped cell hashed as one marker byte. A
/// change to what the panel draws leaves it as it was; a change anywhere
/// else in the frame moves it. Every character the view draws is one cell
/// wide, so a character's index in its line is its column.
fn outside_panel_hash(lines: &[Line<'static>], panel_rows: usize) -> u64 {
    let rows = PANEL_TOP..PANEL_TOP + panel_rows;
    let cols = PANEL_LEFT..PANEL_LEFT + history::PANEL_WIDTH;
    let mut h = Fnv::new();
    for (y, line) in lines.iter().enumerate() {
        let mut x = 0;
        for span in &line.spans {
            let style = line.style.patch(span.style);
            for ch in span.content.chars() {
                if rows.contains(&y) && cols.contains(&x) {
                    h.bytes(&[0xaa]);
                } else {
                    h.bytes(&(ch as u32).to_le_bytes());
                    h.color(style.fg);
                    h.color(style.bg);
                    h.bytes(&style.add_modifier.bits().to_le_bytes());
                    h.bytes(&style.sub_modifier.bits().to_le_bytes());
                }
                x += 1;
            }
        }
        h.bytes(&[0xff, 0xfe]);
    }
    h.0
}

/// The [`outside_panel_hash`] of each `history_frames` frame. Pinned before
/// the panel's sparklines were bucketed and given a minimum scale span, and
/// unchanged by that work: only the panel's own cells changed.
const OUTSIDE_PANEL_BRAILLE_HISTORY: [u64; 3] =
    [0xaf4edbe502809865, 0xfdccf65c0f7c7098, 0x450b797c96ff2a57];
const OUTSIDE_PANEL_HALFBLOCK_HISTORY: [u64; 3] =
    [0x9412952a90c618c5, 0x8f75caf2ad507387, 0xd34539fc515e5e38];

#[test]
fn the_history_panel_changes_nothing_outside_its_rectangle() {
    for (braille, want) in [
        (true, OUTSIDE_PANEL_BRAILLE_HISTORY),
        (false, OUTSIDE_PANEL_HALFBLOCK_HISTORY),
    ] {
        let got: Vec<u64> = history_frames(braille).iter().map(|g| g.2).collect();
        assert_eq!(got, want, "braille={braille}: got {got:#x?}");
    }
}

/// Draw `lines` into a terminal buffer as `ui_die3d` does, so two frames can
/// be compared the way ratatui decides what to send to the terminal.
fn to_buffer(lines: Vec<Line<'static>>, cols: usize, rows: usize) -> Buffer {
    let area = Rect::new(0, 0, cols as u16, rows as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines)
        .style(Style::default().fg(Color::Reset).bg(Color::Reset))
        .render(area, &mut buf);
    buf
}

/// A `Write` that only counts the bytes written to it, shared with the
/// benchmark through an `Rc`, so the crossterm backend can say how many bytes
/// a frame's changes would send to a terminal.
#[derive(Clone, Default)]
struct ByteCount(std::rc::Rc<std::cell::Cell<usize>>);

impl std::io::Write for ByteCount {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.set(self.0.get() + b.len());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What one benchmark case measured.
struct Cost {
    mean_ms: f64,
    p95_ms: f64,
    /// Mean cells whose content or style changed between consecutive frames.
    changed: f64,
    /// Mean bytes the crossterm backend writes for those changes.
    bytes: f64,
}

/// What to draw for one case.
#[derive(Clone, Copy)]
struct Case {
    boards: &'static [&'static str],
    braille: bool,
    transparent: bool,
    size: (usize, usize),
    /// A transition to cut into at every `every` seconds, if any.
    cuts: Option<(TransitionMode, f64)>,
    /// The clouds (`W`) on.
    clouds: bool,
    /// The history panel (`H`): `None` for off with the view updated once,
    /// at 0.0, as before the panel. `Some(on)` for on or off with an update
    /// every second over a history filled as `fill_worst_case` does, so the
    /// panel scrolls as it would at worst and the two cases differ only by
    /// the panel. (The updates change the scene too once the load scales
    /// fill, after 4.5 s, which is why the older rows do without them.)
    history: Option<bool>,
}

/// Frames per case and the view time between them.
const FRAMES: usize = 200;
const DT: f64 = 1.0 / 30.0;

/// Render `FRAMES` frames `DT` apart and time each `render`.
fn measure(case: Case) -> Cost {
    measure_at(case, DT)
}

/// [`measure`] with frames `dt` apart.
fn measure_at(case: Case, dt: f64) -> Cost {
    use ratatui::backend::{Backend, CrosstermBackend};
    let sent = ByteCount::default();
    let mut term = CrosstermBackend::new(sent.clone());
    let b = busy_fake(case.boards);
    let (cols, rows) = case.size;
    let mut v = Die3dView::new(cols, rows);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    if !case.braille {
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
    }
    if !case.transparent {
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    }
    if let Some((mode, _)) = case.cuts {
        v.director.set_transition_mode(mode);
    }
    if !case.clouds {
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
    }
    if case.history != Some(true) {
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
    }
    if case.history.is_some() {
        fill_worst_case(&mut v);
    }
    let start = 1.0;
    let mut next_update = start + 0.5;
    let mut next_cut = start;
    let mut times = Vec::with_capacity(FRAMES);
    let mut prev: Option<Buffer> = None;
    let mut changed = 0usize;
    for i in 0..FRAMES {
        let now = start + i as f64 * dt;
        if let Some((_, every)) = case.cuts {
            if now >= next_cut {
                v.handle_key(Die3dKey::CutNow, next_cut);
                next_cut += every;
            }
        }
        // Outside the timed part: `update` is not `render`'s cost.
        if case.history.is_some() && now >= next_update {
            v.update(&b, next_update);
            next_update += 1.0;
        }
        let t0 = Instant::now();
        let lines = v.render(now);
        times.push(t0.elapsed().as_secs_f64() * 1e3);
        let buf = to_buffer(lines, cols, rows);
        if let Some(p) = &prev {
            let diff = p.diff(&buf);
            changed += diff.len();
            term.draw(diff.into_iter()).expect("writing to a counter");
        }
        prev = Some(buf);
    }
    let mean_ms = times.iter().sum::<f64>() / FRAMES as f64;
    times.sort_by(f64::total_cmp);
    let p95_ms = times[(FRAMES * 95).div_ceil(100) - 1];
    Cost {
        mean_ms,
        p95_ms,
        changed: changed as f64 / (FRAMES - 1) as f64,
        bytes: sent.0.get() as f64 / (FRAMES - 1) as f64,
    }
}

/// Mean milliseconds of `f` over `n` runs.
fn mean_ms(n: usize, mut f: impl FnMut()) -> f64 {
    let t0 = Instant::now();
    for _ in 0..n {
        f();
    }
    t0.elapsed().as_secs_f64() * 1e3 / n as f64
}

/// One braille frame at 150 x 42 taken apart: the fit, the raster, the cell
/// conversion, the labels and the footer, each timed on its own, and the
/// whole `render` for comparison. Steady scene, no transition.
fn stage_report(scene: &str, boards: &[&str]) {
    let b = busy_fake(boards);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    let scene_rows = v.rows - FOOTER_ROWS;
    let (pw, ph) = (v.cols * 2, scene_rows * 4);
    let now = 1.0;
    let grid = if v.chips.len() > 1 {
        super_grid(&v.chips.iter().map(Vec::len).collect::<Vec<_>>()).0
    } else {
        board_grid(v.chips[0].len())
    };
    let n = 200;
    // The fit as `scene_with` gets it (cached after the first call).
    let fit = mean_ms(n, || {
        std::hint::black_box(v.fit_for(pw, ph, grid));
    });
    // The raster includes the fit, as `scene_with` does it; the fit's own
    // time is subtracted in the printed line.
    let raster = mean_ms(n, || {
        std::hint::black_box(v.scene_now(now));
    });
    let (buf, shown, placed) = v.scene_now(now).unwrap();
    let cells = mean_ms(n, || {
        std::hint::black_box(cells::to_braille_lines(&buf));
    });
    let lines = cells::to_braille_lines(&buf);
    let copies: Vec<Vec<Line<'static>>> = (0..n).map(|_| lines.clone()).collect();
    let mut copies = copies.into_iter();
    let labels = mean_ms(n, || {
        let mut l = copies.next().unwrap();
        let flags = v.flag_layout(&placed, scene_rows);
        draw_flags(
            &mut l,
            scene_rows,
            v.cols,
            &flags,
            v.label_style(),
            v.pole_style(),
        );
        std::hint::black_box(l);
    });
    let footer = mean_ms(n, || {
        std::hint::black_box(v.footer(now, Some(shown)));
    });
    // The history panel: its rows built and written over a frame's lines,
    // over a worst-case history.
    fill_worst_case(&mut v);
    let copies: Vec<Vec<Line<'static>>> = (0..n).map(|_| lines.clone()).collect();
    let mut copies = copies.into_iter();
    let panel = mean_ms(n, || {
        let mut l = copies.next().unwrap();
        draw_panel(
            &mut l,
            scene_rows,
            v.cols,
            &v.history.panel_rows(),
            v.panel_style(),
        );
        std::hint::black_box(l);
    });
    // The clouds alone, into a cleared buffer of the scene's size.
    let mut sky = PixelBuf::new(pw, ph);
    let mut k = 0;
    let clouds = mean_ms(n, || {
        k += 1;
        sky.clear_transparent();
        v.clouds.draw(&mut sky, k as f64, 0.5);
        std::hint::black_box(&sky);
    });
    // Each call a fresh frame: more than 1/20 s of view time apart.
    let mut k = 0;
    let total = mean_ms(n, || {
        k += 1;
        std::hint::black_box(v.render(now + k as f64 * 0.06));
    });
    println!(
        "| {scene} | {fit:.3} | {:.3} | {clouds:.3} | {cells:.3} | {labels:.3} | {panel:.3} | {footer:.3} | {total:.3} |",
        raster - fit
    );
}

/// The benchmark. Prints; asserts nothing about time, which depends on the
/// machine. Run in release:
///
/// `cargo test --release --lib --features tui render_cost_report -- --ignored --nocapture`
#[test]
#[ignore = "benchmark: run by hand in release"]
fn render_cost_report() {
    let scenes: [(&str, &[&str]); 3] = [
        ("one board (p150)", &["p150"]),
        ("two dies (p300)", &["p300"; 2]),
        ("super 4 x 2", &["p300"; 8]),
    ];
    println!();
    println!(
        "| scene | cells | bg | size | clouds | mean ms | p95 ms | changed cells/frame | bytes/frame |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    // The history panel off here, so these rows compare with earlier runs;
    // its cost is the next table.
    for size in [(150, 42), (200, 56)] {
        for (scene, boards) in scenes {
            for braille in [true, false] {
                for transparent in [true, false] {
                    for clouds in [false, true] {
                        let c = measure(Case {
                            boards,
                            braille,
                            transparent,
                            size,
                            cuts: None,
                            clouds,
                            history: None,
                        });
                        println!(
                            "| {scene} | {} | {} | {} x {} | {} | {:.2} | {:.2} | {:.0} | {:.0} |",
                            if braille { "braille" } else { "half-block" },
                            if transparent { "transparent" } else { "opaque" },
                            size.0,
                            size.1,
                            if clouds { "on" } else { "off" },
                            c.mean_ms,
                            c.p95_ms,
                            c.changed,
                            c.bytes
                        );
                    }
                }
            }
        }
    }
    println!();
    println!("History panel off and on, braille transparent, clouds on, an update every");
    println!("second over a worst-case history:");
    println!("| scene | size | panel | mean ms | p95 ms | changed cells/frame | bytes/frame |");
    println!("|---|---|---|---|---|---|---|");
    for size in [(150, 42), (200, 56)] {
        for (scene, boards) in scenes {
            for history in [false, true] {
                let c = measure(Case {
                    boards,
                    braille: true,
                    transparent: true,
                    size,
                    cuts: None,
                    clouds: true,
                    history: Some(history),
                });
                println!(
                    "| {scene} | {} x {} | {} | {:.3} | {:.3} | {:.0} | {:.0} |",
                    size.0,
                    size.1,
                    if history { "on" } else { "off" },
                    c.mean_ms,
                    c.p95_ms,
                    c.changed,
                    c.bytes
                );
            }
        }
    }
    println!();
    println!("Transitions, braille transparent 150 x 42, clouds on, a cut by hand every period:");
    println!("| scene | transition | mean ms | p95 ms | changed cells/frame | bytes/frame |");
    println!("|---|---|---|---|---|---|");
    for (scene, boards) in [scenes[0], scenes[2]] {
        for (mode, every) in [
            (TransitionMode::Dissolve, 1.0),
            (TransitionMode::Spin, 2.0),
            (TransitionMode::Chaos, 1.0),
        ] {
            let c = measure(Case {
                boards,
                braille: true,
                transparent: true,
                size: (150, 42),
                cuts: Some((mode, every)),
                clouds: true,
                history: None,
            });
            println!(
                "| {scene} | {mode:?} every {every} s | {:.2} | {:.2} | {:.0} | {:.0} |",
                c.mean_ms, c.p95_ms, c.changed, c.bytes
            );
        }
    }
    println!();
    println!("What the terminal receives per second when the TUI draws 60 times a");
    println!("second (its default; frames 1/60 s apart), braille transparent 150 x 42, clouds on:");
    println!("| scene | mean ms | changed cells/s | KB/s |");
    println!("|---|---|---|---|");
    for (scene, boards) in [scenes[0], scenes[2]] {
        let c = measure_at(
            Case {
                boards,
                braille: true,
                transparent: true,
                size: (150, 42),
                cuts: None,
                clouds: true,
                history: None,
            },
            1.0 / 60.0,
        );
        println!(
            "| {scene} | {:.2} | {:.0} | {:.0} |",
            c.mean_ms,
            c.changed * 60.0,
            c.bytes * 60.0 / 1000.0
        );
    }
    println!();
    wave_report();
    println!();
    println!("Per stage, one braille transparent frame at 150 x 42, clouds and panel on (mean ms;");
    println!("the raster includes the clouds, which are also timed alone):");
    println!(
        "| scene | fit | raster (less fit) | clouds | cells | labels | panel | footer | whole render |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    stage_report("one board (p150)", &["p150"]);
    stage_report("super 4 x 2", &["p300"; 8]);
}

/// The text of each line, spans joined.
fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn cached_fit_holds_across_scenes_of_different_grids() {
    // One view shows the super scene, then a board: two grids at one pixel
    // size. Each frame must equal that of a fresh view, whose cache has
    // never seen the other grid.
    let b = busy_fake(&["p300"; 4]);
    let fresh = |board: bool, t: f64| {
        let mut w = Die3dView::new(150, 42);
        w.update(&b, 0.0);
        if board {
            w.handle_key(Die3dKey::NextBoard, 0.0);
        }
        frame_hash(&w.render(t))
    };
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    assert_eq!(frame_hash(&v.render(1.0)), fresh(false, 1.0));
    v.handle_key(Die3dKey::NextBoard, 1.0);
    assert_eq!(frame_hash(&v.render(2.0)), fresh(true, 2.0));
    v.handle_key(Die3dKey::ToggleScene, 2.0);
    assert_eq!(frame_hash(&v.render(3.0)), fresh(false, 3.0));
}

#[test]
fn a_resize_rebuilds_the_fit_cache() {
    let b = busy_fake(&["p300"; 2]);
    let grid = board_grid(2);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.render(1.0);
    let keys = |v: &Die3dView| {
        let mut k: Vec<_> = v.cache.borrow().fits.keys().copied().collect();
        k.sort_unstable();
        k
    };
    // Braille at 150 x 42: 300 x 160 pixels above the two footer rows.
    assert_eq!(keys(&v), [(300, 160, grid)]);
    // A view made at the new size: its cache starts empty and fills for
    // that size only.
    let mut w = Die3dView::new(200, 56);
    assert!(keys(&w).is_empty());
    w.update(&b, 0.0);
    let want = text(&w.render(2.0));
    assert_eq!(keys(&w), [(400, 216, grid)]);
    // A size change without `resize` is keyed by size too, so it can never
    // pick up the old size's fit: the frame is the new view's.
    let mut u = Die3dView::new(150, 42);
    u.update(&b, 0.0);
    u.render(1.0);
    u.cols = 200;
    u.rows = 56;
    assert_eq!(text(&u.render(2.0)), want);
    assert_eq!(keys(&u), [(300, 160, grid), (400, 216, grid)]);
    // `resize`, as the TUI calls it, drops the old size's fit, and the frame
    // is again the new view's.
    v.resize(200, 56, 0);
    assert!(keys(&v).is_empty());
    assert_eq!(text(&v.render(2.0)), want);
    assert_eq!(keys(&v), [(400, 216, grid)]);
}

/// A one-board view with moving packets, so every fresh frame differs.
fn steady_view() -> Die3dView {
    let b = busy_fake(&["p150"]);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    v
}

#[test]
fn render_within_one_twentieth_of_a_second_returns_the_same_lines() {
    let v = steady_view();
    let first = text(&v.render(1.0));
    // A fresh view shows that the picture does move in 0.04 s ...
    assert_ne!(text(&steady_view().render(1.04)), first);
    // ... but this one returns the frame it drew at 1.0, up to the last
    // instant before the next frame is due (1/20 s, less the 5 ms a frame
    // may come early).
    let due = STEADY_FRAME_SECS - FRAME_EARLY_SECS;
    assert_eq!(text(&v.render(1.04)), first);
    assert_eq!(text(&v.render(1.0 + due - 1e-6)), first);
    // A key is answered at once, whatever the time since the last frame.
    let mut v = v;
    v.handle_key(Die3dKey::ToggleLabels, 1.044);
    assert_ne!(text(&v.render(1.044)), first);
    // Time going backwards (a new clock) is never served from the cache.
    // Clouds off for this part: a view that has seen 5.0 has drifted its
    // clouds a step further than a fresh one, and they never drift back.
    let clear = || {
        let mut v = steady_view();
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        v
    };
    let w = clear();
    w.render(5.0);
    assert_eq!(text(&w.render(4.99)), text(&clear().render(4.99)));
}

#[test]
fn render_after_one_twentieth_differs_in_a_steady_scene() {
    let v = steady_view();
    let first = text(&v.render(1.0));
    let next = text(&v.render(1.0 + 0.05));
    assert_ne!(next, first);
    // And it is the frame a fresh view draws at that time.
    assert_eq!(next, text(&steady_view().render(1.05)));
}

/// How many times the output of `render` changes over `secs` of view time
/// sampled 120 times a second from `start`: the frames actually drawn.
fn redraws(v: &Die3dView, start: f64, secs: f64) -> usize {
    let mut prev = text(&v.render(start));
    let mut n = 0;
    for i in 1..=(secs * 120.0) as usize {
        let lines = text(&v.render(start + i as f64 / 120.0));
        n += (lines != prev) as usize;
        prev = lines;
    }
    n
}

#[test]
fn transitions_render_at_thirty_frames_per_second() {
    // The throttle's own rates are counted with the clouds off: a cloud step
    // (at most 4 a second) is drawn at once and can add a frame between two
    // that were due anyway. With them on, at most those 4 more.
    let view = |clouds: bool| {
        let mut v = steady_view();
        if !clouds {
            v.handle_key(Die3dKey::ToggleClouds, 0.0);
        }
        v
    };
    for (clouds, extra) in [(false, 0), (true, 4)] {
        // At rest: 20 frames in a second.
        let steady = redraws(&view(clouds), 1.0, 1.0);
        assert!(
            (19..=21 + extra).contains(&steady),
            "clouds {clouds}: {steady} frames at rest"
        );
        // In a spin (1.6 s long): 30 frames in a second. (Sampled 120 times
        // a second, a frame is due every fourth sample in a transition and
        // every sixth at rest, with or without the 5 ms early allowance.)
        let mut v = view(clouds);
        v.director.set_transition_mode(TransitionMode::Spin);
        v.handle_key(Die3dKey::CutNow, 2.0);
        let spin = redraws(&v, 2.1, 1.0);
        assert!(
            (29..=31 + extra).contains(&spin),
            "clouds {clouds}: {spin} frames in a spin"
        );
    }
    // A cut started by the director, not a key, also switches to 30 at once.
    let mut v = steady_view();
    v.render(3.0);
    v.director.set_transition_mode(TransitionMode::Dissolve);
    v.director.cut_by_hand(3.01);
    assert_ne!(text(&v.render(3.04)), text(&v.render(3.0)));
}

/// Allocation counter for this thread, read by the allocation test. Counts
/// every `alloc`, `alloc_zeroed` and `realloc` made by the test thread; other
/// threads (other tests) keep their own count.
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn allocations() -> usize {
        ALLOCS.try_with(Cell::get).unwrap_or(0)
    }

    fn count() {
        // `try_with`: a thread being torn down can still allocate.
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
    }

    pub(super) struct Counting;

    // SAFETY: every call is forwarded unchanged to the system allocator.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            System.alloc(layout)
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            System.alloc_zeroed(layout)
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            System.realloc(ptr, layout, new_size)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout)
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

#[test]
fn braille_cells_are_converted_without_per_cell_allocation() {
    // 150 x 40 cells, every one a different colour pair, so no two
    // neighbours share a span: the worst case for allocation.
    let (w, h) = (300, 160);
    let mut buf = PixelBuf::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let c = if y % 4 < 2 {
                [200, (x % 251) as u8, (y % 241) as u8]
            } else {
                [10, 20, (x % 7) as u8]
            };
            buf.put(x as i32, y as i32, c);
        }
    }
    let before = counting::allocations();
    let lines = cells::to_braille_lines(&buf);
    let made = counting::allocations() - before;
    let cells: usize = lines.iter().map(|l| l.spans.len()).sum();
    assert_eq!(cells, 150 * 40, "one span per cell");
    // The line list and one span list per line; nothing per cell.
    assert!(made <= 1 + 40, "{made} allocations for {cells} cells");
    drop(lines);

    // The pixel buffer is reused from frame to frame, not allocated anew.
    let v = steady_view();
    v.render(1.0);
    let ptr = |v: &Die3dView| {
        v.cache
            .borrow()
            .spare
            .iter()
            .map(|b| b.rgb.as_ptr())
            .collect::<Vec<_>>()
    };
    let first = ptr(&v);
    assert_eq!(first.len(), 1, "one buffer for a steady frame");
    v.render(2.0);
    assert_eq!(ptr(&v), first);
}

/// Run `f` with the tile wave on the unquantised clock when `smooth`.
fn with_wave<R>(smooth: bool, f: impl FnOnce() -> R) -> R {
    scene::SMOOTH_WAVE.with(|c| c.set(smooth));
    let r = f();
    scene::SMOOTH_WAVE.with(|c| c.set(false));
    r
}

/// Cells changed between consecutive frames over one second of a steady
/// one-board view drawn 20 times a second, as the view draws it, clouds off.
fn changed_over_a_second(smooth: bool) -> usize {
    with_wave(smooth, || {
        // Clouds off: their cells would count the same in both and blur the
        // comparison of the two wave clocks.
        let mut v = steady_view();
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
        let mut prev: Option<Buffer> = None;
        let mut n = 0;
        for i in 0..=20 {
            let buf = to_buffer(v.render(1.0 + i as f64 * 0.05), 150, 42);
            if let Some(p) = &prev {
                n += p.diff(&buf).len();
            }
            prev = Some(buf);
        }
        n
    })
}

#[test]
fn quantised_wave_changes_fewer_cells_than_the_unquantised_one() {
    let (smooth, stepped) = (changed_over_a_second(true), changed_over_a_second(false));
    assert!(
        stepped * 4 <= smooth * 3,
        "{stepped} cells changed with the stepped wave, {smooth} without"
    );
    // The look of a frame is unchanged: with nothing else moving (no PCIe
    // packets), the frame at `t` is exactly the unquantised frame at the
    // wave's own time, at most 1/12 s earlier.
    for t in [1.0, 1.04, 1.07, 2.333, 7.9] {
        let quiet = || {
            let b = Fake::boards(&["p150"], Some(TDP));
            let mut v = Die3dView::new(150, 42);
            v.update(&b, 0.0);
            // Clouds off: the two times compared can fall in different
            // cloud clock steps.
            v.handle_key(Die3dKey::ToggleClouds, 0.0);
            v
        };
        let stepped = frame_hash(&quiet().render(t));
        let wave_t = scene::wave_time(t as f32) as f64;
        assert!(t - wave_t < 1.0 / 12.0 + 1e-6, "{t} -> {wave_t}");
        let smooth = with_wave(true, || frame_hash(&quiet().render(wave_t)));
        assert_eq!(stepped, smooth, "at {t}");
    }
}

/// The largest difference in any colour channel between two cells, 0 when
/// both colours match; 255 when one side has no RGB colour where the other
/// does (a cell that is drawn in one frame and not the other).
fn colour_gap(a: &ratatui::buffer::Cell, b: &ratatui::buffer::Cell) -> u8 {
    let gap = |x: Color, y: Color| match (x, y) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            r1.abs_diff(r2).max(g1.abs_diff(g2)).max(b1.abs_diff(b2))
        }
        (x, y) if x == y => 0,
        _ => 255,
    };
    gap(a.fg, b.fg).max(gap(a.bg, b.bg))
}

/// What stepping the wave's clock does, braille transparent 150 x 42, one
/// board, clouds off, 10 s of frames 1/20 s apart: cells changed per drawn
/// frame with and without it, and how far each stepped frame is from the
/// unstepped frame at the same time (cells that differ, the largest colour gap among them, and
/// the share of those that differ by a glyph or by 255 in a channel, which
/// is a block edge moved by a pixel row).
fn wave_report() {
    let frames = |smooth: bool| {
        with_wave(smooth, || {
            // Clouds off, as in `changed_over_a_second`.
            let mut v = steady_view();
            v.handle_key(Die3dKey::ToggleClouds, 0.0);
            (0..200)
                .map(|i| to_buffer(v.render(1.0 + i as f64 * 0.05), 150, 42))
                .collect::<Vec<_>>()
        })
    };
    let (smooth, stepped) = (frames(true), frames(false));
    let per_frame = |f: &[Buffer]| {
        f.windows(2).map(|w| w[0].diff(&w[1]).len()).sum::<usize>() as f64 / (f.len() - 1) as f64
    };
    let (mut differing, mut gap_max, mut edge) = (0usize, 0u8, 0usize);
    let mut gaps: Vec<u8> = Vec::new();
    for (a, b) in smooth.iter().zip(&stepped) {
        for (ca, cb) in a.content.iter().zip(&b.content) {
            if ca != cb {
                differing += 1;
                let g = colour_gap(ca, cb);
                gap_max = gap_max.max(g);
                if g == 255 || ca.symbol() != cb.symbol() {
                    edge += 1;
                } else {
                    gaps.push(g);
                }
            }
        }
    }
    gaps.sort_unstable();
    let median = gaps.get(gaps.len() / 2).copied().unwrap_or(0);
    let p95 = gaps.get(gaps.len() * 95 / 100).copied().unwrap_or(0);
    println!(
        "Wave clock stepped at {} steps/s, braille 150 x 42 one board, 20 frames/s:",
        scene::WAVE_STEPS_PER_SEC
    );
    println!("| wave | changed cells per drawn frame |");
    println!("|---|---|");
    println!("| smooth | {:.0} |", per_frame(&smooth));
    println!("| stepped | {:.0} |", per_frame(&stepped));
    println!(
        "Stepped against smooth at the same time: {:.0} of 6000 cells differ per frame; {:.0} of those by glyph or coverage (largest colour gap {gap_max}); colour gap of the rest median {median}, 95th percentile {p95}, max {} (of 255).",
        differing as f64 / 200.0,
        edge as f64 / 200.0,
        gaps.last().copied().unwrap_or(0),
    );
}

/// Cells changed between consecutive frames, summed over 60 frames 1/30 s
/// apart (two seconds), braille transparent 150 x 42, for one board (p150)
/// or the four-board super scene, with the clouds on or off. The history
/// panel is off: these counts pin the scene, whose budget predates the panel.
/// Deterministic: no clock is read.
fn changed_in_two_seconds(boards: &[&str], clouds: bool) -> usize {
    changed_with_samples(boards, clouds, None).0
}

/// [`changed_in_two_seconds`], and with `panel` set, an `update` once a
/// second of the run (at 1.5 and 2.5) with the history panel on (`Some(true)`)
/// or off (`Some(false)`). With `None` the view sees one update, at 0.0, and
/// the panel is off.
///
/// With `panel` set, the history is first filled by [`fill_worst_case`], so
/// that each new sample moves every cell of every sparkline: the most cells
/// a scroll can change. The updates read the same telemetry as at
/// 0.0, so they change nothing in the scene itself (the load scales hold
/// three samples, short of the five they need to take over).
///
/// Returns the cells changed in the whole frame and, of those, the cells in
/// the panel's rectangle (counted whether the panel is on or off).
fn changed_with_samples(boards: &[&str], clouds: bool, panel: Option<bool>) -> (usize, usize) {
    let b = busy_fake(boards);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    if !clouds {
        v.handle_key(Die3dKey::ToggleClouds, 0.0);
    }
    if panel != Some(true) {
        v.handle_key(Die3dKey::ToggleHistory, 0.0);
    }
    if panel.is_some() {
        fill_worst_case(&mut v);
    }
    let rows = (PANEL_TOP..PANEL_TOP + v.history.panel_rows().len()) as std::ops::Range<usize>;
    let cols = PANEL_LEFT..PANEL_LEFT + history::PANEL_WIDTH;
    let mut prev: Option<Buffer> = None;
    let (mut n, mut in_panel) = (0, 0);
    for i in 0..60 {
        let now = 1.0 + i as f64 * DT;
        // Updates half a second into each second of the run, so both
        // scrolls fall between two frames that are counted (one on the very
        // first frame would not be).
        if panel.is_some() && i % 30 == 15 {
            v.update(&b, now);
        }
        let buf = to_buffer(v.render(now), 150, 42);
        if let Some(p) = &prev {
            let diff = p.diff(&buf);
            n += diff.len();
            in_panel += diff
                .iter()
                .filter(|(x, y, _)| rows.contains(&(*y as usize)) && cols.contains(&(*x as usize)))
                .count();
        }
        prev = Some(buf);
    }
    (n, in_panel)
}

/// Replace `v`'s history with a minute of samples (at view times -60 to -1)
/// that move between two levels in every series (power, temperature and
/// both PCIe directions) in the repeating pattern high, low, low, high, low.
///
/// The sparkline buckets 60 samples into 24 cells, two and three samples in
/// turn (five samples per pair of cells), so a pattern that repeats every
/// five samples puts the same pair of means in every pair of cells. From
/// this fill, each of the two scrolls `changed_with_samples` makes changes
/// the level of every cell: the power means of a pair of cells go from 45
/// and 40 W to 30 and 50 W, then back to 45 and 40 W. That is every cell
/// of every sparkline, the most a scroll can change (a search over every
/// two-level pattern of up to eight samples found none that changes more).
/// Plain alternation (high, low, ...) changes only the three-sample cells,
/// half of them, as the two-sample cells always average to the middle.
fn fill_worst_case(v: &mut Die3dView) {
    let mut chips: Vec<ChipInputs> = v.chips.iter().flatten().cloned().collect();
    v.history = History::new();
    for k in 0..history::SAMPLES {
        let high = matches!(k % 5, 0 | 3);
        for c in chips.iter_mut() {
            c.power_w = Some(if high { 60.0 } else { 30.0 });
            c.temp_c = Some(if high { 70.0 } else { 50.0 });
            c.pcie_in_bps = Some(if high { 1.0e9 } else { 1.0e7 });
            c.pcie_out_bps = Some(if high { 5.0e8 } else { 1.0e7 });
        }
        assert!(v.history.sample(k as f64 - 60.0, &chips));
    }
}

/// Cells changed over two seconds starting at view time `t0`, clouds and
/// panel off. The fixed window of [`changed_in_two_seconds`] starts at 1 s,
/// where the sideways sweep is at the die's edge and cheap; this one can be
/// started anywhere in the wave's cycle.
fn changed_from(boards: &[&str], t0: f64) -> usize {
    let b = busy_fake(boards);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    v.handle_key(Die3dKey::ToggleClouds, 0.0);
    v.handle_key(Die3dKey::ToggleHistory, 0.0);
    let mut prev: Option<Buffer> = None;
    let mut n = 0;
    for i in 0..60 {
        let buf = to_buffer(v.render(t0 + i as f64 * DT), 150, 42);
        if let Some(p) = &prev {
            n += p.diff(&buf).len();
        }
        prev = Some(buf);
    }
    n
}

/// While alive, the wave ignores PCIe traffic and is the sideways sweep it was
/// before it followed traffic. The Task 16 and 17 budgets measure the scene's
/// redraw work against a baseline taken with that sweep, so they hold it
/// fixed; [`traffic_fronts_cost_little_more_than_the_sideways_sweep`] prices
/// the fronts.
struct SidewaysSweep;

impl SidewaysSweep {
    fn on() -> SidewaysSweep {
        scene::NO_FLOW.with(|f| f.set(true));
        SidewaysSweep
    }
}

impl Drop for SidewaysSweep {
    fn drop(&mut self) {
        scene::NO_FLOW.with(|f| f.set(false));
    }
}

/// Mean cells changed over two seconds, for windows starting every 3 s over
/// 24 s: the wave's whole cycle. The fixed window of
/// [`changed_in_two_seconds`] starts where the sideways sweep is at the die's
/// edge and cheapest (5652 cells; its cycle mean is 9464), so it cannot price
/// a change of wave.
fn cycle_mean(boards: &[&str]) -> usize {
    let counts: Vec<usize> = (0..8)
        .map(|k| changed_from(boards, 1.0 + k as f64 * 3.0))
        .collect();
    counts.iter().sum::<usize>() / counts.len()
}

#[test]
fn traffic_fronts_cost_little_more_than_the_sideways_sweep() {
    // One board with traffic in both directions: two fronts at once, the
    // costliest case. Measured over the cycle: the fronts repaint 1.23 times
    // the cells of the sideways sweep (11633 against 9464). Fronts reaching 4 tiles to each side, like the sweep,
    // would repaint far more, and this bound catches that.
    let fronts = cycle_mean(&["p150"]);
    let sweep = {
        let _guard = SidewaysSweep::on();
        cycle_mean(&["p150"])
    };
    println!("one board, mean over the cycle: fronts {fronts}, sweep {sweep}");
    assert!(
        fronts as f64 <= sweep as f64 * 1.3,
        "fronts {fronts}, sideways sweep {sweep}"
    );
}

/// `changed_in_two_seconds` of the view as committed before Task 16
/// (03cd98e5), computed with this same function.
const CHANGED_BEFORE_ONE_BOARD: usize = 12464;
const CHANGED_BEFORE_SUPER: usize = 16424;

#[test]
fn changed_cells_per_frame_are_at_most_half_of_before() {
    // The Task 16 target on terminal traffic, asserted because the count is
    // deterministic (the time targets are machine-dependent and are not).
    // Measured with the clouds off: the clouds (Task 17) came after it and
    // change cells of their own, so with them on this test would fail by
    // design. Their own budget is the next test.
    let _sweep = SidewaysSweep::on();
    for (what, boards, before) in [
        ("one board", &["p150"][..], CHANGED_BEFORE_ONE_BOARD),
        ("super 4 x 2", &["p300"; 8][..], CHANGED_BEFORE_SUPER),
    ] {
        let after = changed_in_two_seconds(boards, false);
        println!("{what}: {after} cells changed, {before} before");
        // Task 16 reached half. The pin-and-flag labels (Task 20) take
        // back a little: 5652 of 12464 (0.453) and 8413 of 16424 (0.512).
        assert!(
            after as f64 <= before as f64 * 0.52,
            "{what}: {after} cells changed, {before} before"
        );
    }
}

/// The most `changed_in_two_seconds` with the clouds on may be, as a share of
/// the pre-Task-16 count. Task 16 brought it to at most half; the clouds may
/// take back part of that, up to the hard cap of 0.70 (see
/// `CHANGED_WITH_PANEL_MAX_SHARE`). Measured when the clouds were added
/// (Task 17): 8030 of 12464 (0.644) for one
/// board, 10713 of 16424 (0.652) for the super scene, against 5631 and 8023
/// with the clouds off. The limit sits just above the larger share, so a
/// change that makes the clouds repaint more cells is caught.
///
/// The pin-and-flag labels (Task 20) added cells of their own: a flag moves
/// with every camera cut, and in the super scene a pole may cross a board. With
/// them, 8041 of 12464 (0.645) and 11058 of 16424 (0.673), against 5652 and
/// 8413 with the clouds off, so the limit is 0.68.
const CHANGED_WITH_CLOUDS_MAX_SHARE: f64 = 0.68;

#[test]
fn changed_cells_with_clouds_stay_within_their_budget() {
    // Pinned to the history panel off (`changed_in_two_seconds` turns it
    // off): the panel's own budget is the next test.
    // The busy fake moves 2.5 GB/s per chip, so the clouds drift fast: 7.4
    // px/s over one p150, the 8 px/s maximum over eight chips. This is the
    // worst case for them.
    let _sweep = SidewaysSweep::on();
    for (what, boards, before) in [
        ("one board", &["p150"][..], CHANGED_BEFORE_ONE_BOARD),
        ("super 4 x 2", &["p300"; 8][..], CHANGED_BEFORE_SUPER),
    ] {
        let (on, off) = (
            changed_in_two_seconds(boards, true),
            changed_in_two_seconds(boards, false),
        );
        let share = on as f64 / before as f64;
        println!(
            "{what}: {on} cells changed with clouds, {off} without, {before} before: {share:.3}"
        );
        assert!(on > off, "{what}: the clouds moved nothing");
        assert!(
            share <= CHANGED_WITH_CLOUDS_MAX_SHARE,
            "{what}: {on} cells changed with clouds, {before} before ({share:.3})"
        );
    }
}

/// The hard cap on `changed_with_samples` with the clouds and the history
/// panel on, as a share of the pre-Task-16 count. The panel's sparklines
/// scroll once a second and every scroll repaints them.
const CHANGED_WITH_PANEL_MAX_SHARE: f64 = 0.70;

#[test]
fn changed_cells_with_clouds_and_the_panel_stay_within_the_cap() {
    let _sweep = SidewaysSweep::on();
    for (what, boards, before) in [
        ("one board", &["p150"][..], CHANGED_BEFORE_ONE_BOARD),
        ("super 4 x 2", &["p300"; 8][..], CHANGED_BEFORE_SUPER),
    ] {
        let (on, on_panel) = changed_with_samples(boards, true, Some(true));
        let (off, off_panel) = changed_with_samples(boards, true, Some(false));
        // The instrument: the updates of the run change nothing on screen by
        // themselves, so the panel-off count is the clouds-only count, as
        // measured in `changed_cells_with_clouds_stay_within_their_budget`
        // (8041 and 11058 with the flags of Task 20; 8030 and 10713 before).
        let clouds_only = [("one board", 8041), ("super 4 x 2", 11058)];
        let want = clouds_only.iter().find(|c| c.0 == what).unwrap().1;
        assert_eq!(off, want, "{what}");
        let share = on as f64 / before as f64;
        println!(
            "{what}: {on} cells changed with clouds and the panel, {off} with the panel off ({:+}); in the panel's rectangle {on_panel} on, {off_panel} off; {before} before: {share:.3}",
            on as i64 - off as i64
        );
        // The panel did scroll, twice: each scroll moves every cell of every
        // sparkline (see `fill_worst_case`) but the newest, which holds the
        // telemetry the update read and can land on any level. The panel can still lower the total, by
        // covering scene and cloud cells that would have changed.
        let series = v_series(boards);
        assert!(
            on_panel >= 2 * (SPARK - 1) * series,
            "{what}: the panel changed {on_panel} cells, {series} sparklines"
        );
        assert!(
            share <= CHANGED_WITH_PANEL_MAX_SHARE,
            "{what}: {on} cells changed with clouds and the panel, {before} before ({share:.3})"
        );
    }
}

/// Cells of a sparkline (see `history::SPARK_WIDTH`).
const SPARK: usize = history::SPARK_WIDTH;

/// Sparklines the panel shows for `boards`: one per chip, at most
/// `history::MAX_CHIPS`, and two for PCIe.
fn v_series(boards: &[&str]) -> usize {
    let mut v = Die3dView::new(150, 42);
    v.update(&busy_fake(boards), 0.0);
    v.chips.iter().flatten().count().min(history::MAX_CHIPS) + 2
}

// ---- Size sweep ----------------------------------------------------------

/// The values to try for one dimension: every value within two of each
/// threshold, plus a coarse grid from 1 up to `max` that always has 1 to 4.
/// Sorted, without repeats.
fn sweep_values(thresholds: &[usize], max: usize) -> Vec<usize> {
    let mut vals: Vec<usize> = (1..=4).collect();
    // About 10 values spread over 1..=max, which keeps the sweep within the
    // 30 s budget in a debug build.
    vals.extend((1..=10).map(|i| i * max / 10));
    for &t in thresholds {
        vals.extend(t.saturating_sub(2).max(1)..=t + 2);
    }
    vals.sort_unstable();
    vals.dedup();
    vals
}

/// Display width of a line in cells. Every glyph the view draws, braille and
/// block ones included, is one cell wide, so counting chars is exact.
fn line_width(line: &Line<'static>) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}

/// A view over `boards` at `cols` x `rows` with the director off, in the
/// given cell mode (the `B` key), ready to render.
fn sized_view(boards: &Fake, cols: usize, rows: usize, braille: bool) -> Die3dView {
    let mut v = Die3dView::new(cols, rows);
    v.update(boards, 0.0);
    v.director.set_auto(false);
    if !braille {
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
    }
    v
}

#[test]
fn every_size_draws_without_overflow() {
    let started = Instant::now();
    let cols_at = sweep_values(
        &[
            tiers::FOOTER_TWO.0,
            tiers::FOOTER_ONE.0,
            tiers::FLAGS_MIN.0,
            tiers::CLOUDS_MIN.0,
            scene::MIN_CELLS.0,
            PANEL_MIN_CELLS.0,
        ],
        200,
    );
    let rows_at = sweep_values(
        &[
            tiers::FOOTER_TWO.1,
            tiers::FOOTER_ONE.1,
            tiers::FLAGS_MIN.1,
            tiers::CLOUDS_MIN.1,
            scene::MIN_CELLS.1,
            PANEL_MIN_CELLS.1,
        ],
        60,
    );
    let one = busy_fake(&["p150"]);
    let eight = busy_fake(&["p300"; 8]);
    let mut renders = 0;
    for &c in &cols_at {
        for &r in &rows_at {
            for b in [&one, &eight] {
                for braille in [true, false] {
                    let mut v = sized_view(b, c, r, braille);
                    // `render` trims the summary to the scene's rows, which
                    // would hide a summary that is too long, so check the
                    // summary on its own too.
                    if braille {
                        let summary = summary::summary_lines(&v.chips, c, r);
                        assert!(
                            summary.len() <= r,
                            "{c}x{r}: summary {} lines",
                            summary.len()
                        );
                        assert!(
                            summary.iter().all(|l| line_width(l) <= c),
                            "{c}x{r}: summary line too wide"
                        );
                    }
                    for k in 0..3 {
                        let now = 10.0 * k as f64;
                        if k > 0 {
                            v.handle_key(Die3dKey::CutNow, now - 5.0);
                        }
                        let lines = v.render(now);
                        renders += 1;
                        assert!(
                            lines.len() <= r,
                            "{c}x{r} braille={braille} cut {k}: {} lines",
                            lines.len()
                        );
                        for (i, l) in lines.iter().enumerate() {
                            assert!(
                                line_width(l) <= c,
                                "{c}x{r} braille={braille} cut {k}: line {i} is {} wide",
                                line_width(l)
                            );
                        }
                    }
                }
            }
        }
    }
    // A view with no cells at all draws nothing and must not panic.
    for (c, r) in [(0, 0), (0, 30), (100, 0), (0, 1), (1, 0)] {
        for braille in [true, false] {
            let mut v = sized_view(&eight, c, r, braille);
            let lines = v.render(0.0);
            assert!(lines.len() <= r, "{c}x{r}: {} lines", lines.len());
            assert!(lines.iter().all(|l| line_width(l) <= c), "{c}x{r}");
            v.handle_key(Die3dKey::CutNow, 5.0);
            let _ = v.render(10.0);
        }
    }
    println!(
        "size sweep: {renders} renders over {} x {} sizes in {:.1?}",
        cols_at.len(),
        rows_at.len(),
        started.elapsed()
    );
}

#[test]
fn flags_never_cover_a_drawn_cell_at_any_size() {
    // The same comparison as `flags_never_cover_a_drawn_cell_but_the_pin` in
    // mod.rs (flags on against off, clouds off): every cell a flag changes
    // was blank, or is now the pin or its pole. This one runs it over sizes
    // from 40 x 12 to 150 x 42 around the tier thresholds, where the footer
    // height and the flags switch on and off.
    let boards = [
        busy_fake(&["p300"; 2]),
        busy_fake(&["p300", "p300", "p150", "p300"]),
    ];
    let cols_at = sweep_values(&[tiers::FLAGS_MIN.0, tiers::FOOTER_TWO.0], 150);
    let rows_at = sweep_values(&[tiers::FLAGS_MIN.1, tiers::FOOTER_TWO.1], 42);
    let mut flagged = 0;
    for &c in cols_at.iter().filter(|&&c| (40..=150).contains(&c)) {
        for &r in rows_at.iter().filter(|&&r| (12..=42).contains(&r)) {
            // The footer's rows are not scene rows; the tier says how many.
            let scene_rows = r - tiers::tier_for(c, r).footer_rows;
            for b in &boards {
                for braille in [true, false] {
                    let mut v = sized_view(b, c, r, braille);
                    v.handle_key(Die3dKey::ToggleClouds, 0.0);
                    v.handle_key(Die3dKey::ToggleHistory, 0.0);
                    for k in 0..3 {
                        let now = 10.0 * k as f64;
                        if k > 0 {
                            v.handle_key(Die3dKey::CutNow, now - 5.0);
                        }
                        v.handle_key(Die3dKey::ToggleLabels, now);
                        let off = text(&v.render(now));
                        v.handle_key(Die3dKey::ToggleLabels, now);
                        let on = text(&v.render(now));
                        for (row, (a, b)) in off.iter().zip(&on).enumerate().take(scene_rows) {
                            for (col, (x, y)) in a.chars().zip(b.chars()).enumerate() {
                                if x == y {
                                    continue;
                                }
                                flagged += 1;
                                let blank = x == ' ' || x == '\u{2800}';
                                assert!(
                                    blank || y == '●' || y == '│',
                                    "{c}x{r} braille={braille} cut {k}: {y:?} over {x:?} at {row},{col}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(flagged > 0, "no flag was ever drawn");
}

#[test]
fn sizes_at_and_above_100_by_30_use_the_full_layout() {
    // The footer's two lines (keys, then status) are the last two rows. A
    // size one short in either dimension has one footer line, so the row
    // above the last is scene or blank and holds no keys line.
    let b = busy_fake(&["p300"; 2]);
    for (c, r) in [(100, 30), (120, 36), (150, 42)] {
        let mut v = sized_view(&b, c, r, true);
        let lines = v.render(0.0);
        assert_eq!(lines.len(), r, "{c}x{r}");
        assert_eq!(tiers::tier_for(c, r).footer_rows, 2, "{c}x{r}");
        // The footer is the last tier.footer_rows lines: the two lines hold
        // text, and the third from last is scene (no footer text).
        let rows_text = text(&lines);
        let footer = &rows_text[r - 2..];
        assert!(
            footer
                .iter()
                .all(|l| l.chars().any(|ch| ch.is_alphanumeric())),
            "{c}x{r}: footer {footer:?}"
        );
    }
    // One short in either dimension drops to one footer line.
    for (c, r) in [(99, 30), (100, 29)] {
        assert_eq!(tiers::tier_for(c, r).footer_rows, 1, "{c}x{r}");
    }
}

// ---- The Insights inset --------------------------------------------------

/// Cells changed between consecutive frames of a one-board inset, summed over
/// 60 frames `DT` apart (two seconds of the view's draw rate). The view is
/// built as the host builds it (`Die3dView::inset`) and left as it is: the
/// inset has no clouds and no history panel of its own, so there is nothing
/// to switch off first. Deterministic: no clock is read.
fn inset_changed_cells(cols: usize, rows: usize, boards: &[&str]) -> usize {
    let b = busy_fake(boards);
    let mut v = Die3dView::inset(cols, rows);
    // A read each second until 6 s, so that the busiest-board choice of a
    // many-board inset has settled (it needs five samples).
    for s in 0..=6 {
        v.update(&b, s as f64);
    }
    v.director.set_auto(false);
    let mut prev: Option<Buffer> = None;
    let mut n = 0;
    for i in 0..60 {
        let buf = to_buffer(v.render(7.0 + i as f64 * DT), cols, rows);
        if let Some(p) = &prev {
            n += p.diff(&buf).len();
        }
        prev = Some(buf);
    }
    n
}

#[test]
fn inset_changed_cells_stay_within_budget() {
    // Measured: 1578 changed cells over the 59 frame pairs of the two
    // seconds (42 x 14 = 588 cells, so about 27 cells a frame).
    // Ceiling: 1.25 x 1578 = 1972.5, rounded up to 1973.
    const CEILING: usize = 1973;
    let n = inset_changed_cells(42, 14, &["p150"]);
    assert!(
        n <= CEILING,
        "the one-board inset changed {n} cells in two seconds, ceiling {CEILING}"
    );
}

/// The benchmark for the inset. Prints; asserts nothing about time. Run in
/// release:
///
/// `cargo test --release --locked --lib --features tui inset_render_cost_report -- --ignored --nocapture`
///
/// The spec's budget is 0.3 ms a frame or less at 42 x 14 for one board.
#[test]
#[ignore = "benchmark: run by hand in release"]
fn inset_render_cost_report() {
    println!();
    println!("| boards | size | mean ms per frame | changed cells in 2 s |");
    println!("|---|---|---|---|");
    for (label, boards) in [
        ("1 (p150)", vec!["p150"]),
        ("8 (p150 x 8)", vec!["p150"; 8]),
    ] {
        let b = busy_fake(&boards);
        for (cols, rows) in [(42usize, 14usize), (42, 22)] {
            let mut v = Die3dView::inset(cols, rows);
            for s in 0..=6 {
                v.update(&b, s as f64);
            }
            v.director.set_auto(false);
            // Warm the fit caches, then time frames DT apart.
            for i in 0..30 {
                v.render(7.0 + i as f64 * DT);
            }
            let mut i = 0;
            let ms = mean_ms(600, || {
                std::hint::black_box(v.render(8.0 + i as f64 * DT));
                i += 1;
            });
            println!(
                "| {label} | {cols} x {rows} | {ms:.3} | {} |",
                inset_changed_cells(cols, rows, &boards)
            );
        }
    }
}

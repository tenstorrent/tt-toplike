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
fn golden_frames(braille: bool) -> Vec<(f64, u64)> {
    let b = busy_fake(&["p300"; 4]);
    let mut v = Die3dView::new(150, 42);
    v.update(&b, 0.0);
    v.director.set_auto(false);
    if !braille {
        v.handle_key(Die3dKey::ToggleBraille, 0.0);
        v.handle_key(Die3dKey::ToggleTransparent, 0.0);
    }
    let mut out = Vec::new();
    let mut shot = |v: &Die3dView, t: f64| out.push((t, frame_hash(&v.render(t))));
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

/// The hashes of `golden_frames`, taken from the view as committed before
/// Task 16 (03cd98e5), with this same code.
const GOLDEN_BRAILLE: [u64; 6] = [
    0x553e37addb11cf7c,
    0xa94976c09fabf489,
    0x181a000add5181cf,
    0xe4e56a8f4c342d0a,
    0x5e9919585daf1ae4,
    0x5b195ee4d9509dfc,
];
const GOLDEN_HALFBLOCK: [u64; 6] = [
    0xfa0af3ba649b7efc,
    0x20b5e7bade332bf1,
    0xbe55d5d16bd8fb5a,
    0x360c4f8696a0a5d5,
    0x61f21e853bf68cde,
    0x06dbdddaec5ebb99,
];

#[test]
fn cached_fit_gives_identical_frames() {
    for (braille, want) in [(true, GOLDEN_BRAILLE), (false, GOLDEN_HALFBLOCK)] {
        let got = golden_frames(braille);
        let hashes: Vec<u64> = got.iter().map(|g| g.1).collect();
        assert_eq!(
            hashes,
            want,
            "braille={braille}: frames at {:?} changed; got {hashes:#x?}",
            got.iter().map(|g| g.0).collect::<Vec<_>>()
        );
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
    let start = 1.0;
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
        let anchors = v.label_anchors(&placed, scene_rows);
        draw_labels(&mut l, scene_rows, v.cols, &anchors, v.label_style());
        std::hint::black_box(l);
    });
    let footer = mean_ms(n, || {
        std::hint::black_box(v.footer(now, Some(shown)));
    });
    // Each call a fresh frame: more than 1/20 s of view time apart.
    let mut k = 0;
    let total = mean_ms(n, || {
        k += 1;
        std::hint::black_box(v.render(now + k as f64 * 0.06));
    });
    println!(
        "| {scene} | {fit:.3} | {:.3} | {cells:.3} | {labels:.3} | {footer:.3} | {total:.3} |",
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
        "| scene | cells | bg | size | mean ms | p95 ms | changed cells/frame | bytes/frame |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for size in [(150, 42), (200, 56)] {
        for (scene, boards) in scenes {
            for braille in [true, false] {
                for transparent in [true, false] {
                    let c = measure(Case {
                        boards,
                        braille,
                        transparent,
                        size,
                        cuts: None,
                    });
                    println!(
                        "| {scene} | {} | {} | {} x {} | {:.2} | {:.2} | {:.0} | {:.0} |",
                        if braille { "braille" } else { "half-block" },
                        if transparent { "transparent" } else { "opaque" },
                        size.0,
                        size.1,
                        c.mean_ms,
                        c.p95_ms,
                        c.changed,
                        c.bytes
                    );
                }
            }
        }
    }
    println!();
    println!("Transitions, braille transparent 150 x 42, a cut by hand every period:");
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
            });
            println!(
                "| {scene} | {mode:?} every {every} s | {:.2} | {:.2} | {:.0} | {:.0} |",
                c.mean_ms, c.p95_ms, c.changed, c.bytes
            );
        }
    }
    println!();
    println!("What the terminal receives per second when the TUI draws 60 times a");
    println!("second (its default; frames 1/60 s apart), braille transparent 150 x 42:");
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
    println!("Per stage, one braille transparent frame at 150 x 42 (mean ms):");
    println!("| scene | fit | raster (less fit) | cells | labels | footer | whole render |");
    println!("|---|---|---|---|---|---|---|");
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
    // The TUI makes a new view on a resize: its cache starts empty and fills
    // for the new size only.
    let mut w = Die3dView::new(200, 56);
    assert!(keys(&w).is_empty());
    w.update(&b, 0.0);
    let want = text(&w.render(2.0));
    assert_eq!(keys(&w), [(400, 216, grid)]);
    // A size change in place is keyed by size too, so it can never pick up
    // the old size's fit: the frame is the new view's.
    v.cols = 200;
    v.rows = 56;
    assert_eq!(text(&v.render(2.0)), want);
    assert_eq!(keys(&v), [(300, 160, grid), (400, 216, grid)]);
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
    let w = steady_view();
    w.render(5.0);
    assert_eq!(text(&w.render(4.99)), text(&steady_view().render(4.99)));
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
    // At rest: 20 frames in a second.
    let steady = redraws(&steady_view(), 1.0, 1.0);
    assert!((19..=21).contains(&steady), "{steady} frames at rest");
    // In a spin (1.6 s long): 30 frames in a second. (Sampled 120 times a
    // second, a frame is due every fourth sample in a transition and every
    // sixth at rest, with or without the 5 ms early allowance.)
    let mut v = steady_view();
    v.director.set_transition_mode(TransitionMode::Spin);
    v.handle_key(Die3dKey::CutNow, 2.0);
    let spin = redraws(&v, 2.1, 1.0);
    assert!((29..=31).contains(&spin), "{spin} frames in a spin");
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
/// one-board view drawn 20 times a second, as the view draws it.
fn changed_over_a_second(smooth: bool) -> usize {
    with_wave(smooth, || {
        let v = steady_view();
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
/// board, 10 s of frames 1/20 s apart: cells changed per drawn frame with and
/// without it, and how far each stepped frame is from the unstepped frame at
/// the same time (cells that differ, the largest colour gap among them, and
/// the share of those that differ by a glyph or by 255 in a channel, which
/// is a block edge moved by a pixel row).
fn wave_report() {
    let frames = |smooth: bool| {
        with_wave(smooth, || {
            let v = steady_view();
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

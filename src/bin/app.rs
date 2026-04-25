// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! tt-toplike-app — native terminal window that hosts tt-toplike-tui as a PTY child.
//!
//! Architecture:
//!   eframe/egui window
//!     └── portable-pty PtyPair
//!           └── tt-toplike-tui child process (auto-detected sibling binary)
//!
//! The VT state machine (vte) parses output from the PTY master into a `Screen`
//! cell grid.  Each frame egui renders that grid using `LayoutJob` batching
//! (one job per row) for low overhead.  Keyboard events are encoded to ANSI
//! escape sequences and written back to the PTY master.

use eframe::egui::{self, Color32, FontId, Key, Modifiers, RichText};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vte::{Params, Parser, Perform};

// ── Screen cell ───────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Cell {
    ch: char,
    fg: Color32,
    bg: Color32,
    bold: bool,
    underline: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color32::from_rgb(220, 220, 220),
            bg: Color32::TRANSPARENT,
            bold: false,
            underline: false,
        }
    }
}

// ── VT parser state ───────────────────────────────────────────────────────────

/// Current SGR (Select Graphic Rendition) pen state.
#[derive(Clone, Default)]
struct Pen {
    fg: Option<Color32>,
    bg: Option<Color32>,
    bold: bool,
    underline: bool,
}

impl Pen {
    fn fg(&self) -> Color32 {
        self.fg.unwrap_or(Color32::from_rgb(220, 220, 220))
    }
    fn bg(&self) -> Color32 {
        self.bg.unwrap_or(Color32::TRANSPARENT)
    }
}

/// The parsed screen grid, updated by the VT state machine.
pub struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Cell>>,
    cursor_col: usize,
    cursor_row: usize,
    pen: Pen,
    /// Saved cursor for ESC 7 / ESC 8 — stores (col, row, pending_wrap).
    saved_cursor: (usize, usize, bool),
    /// Deferred auto-wrap: set when the last printed character landed on the
    /// final column.  The actual wrap (col→0, row+1) is deferred until the
    /// *next* printable character so that escape sequences (cursor moves, SGR)
    /// issued immediately after a full row are not displaced onto the wrong row.
    pending_wrap: bool,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::default(); cols]; rows],
            cursor_col: 0,
            cursor_row: 0,
            pen: Pen::default(),
            saved_cursor: (0, 0, false),
            pending_wrap: false,
        }
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.cells.resize(rows, vec![Cell::default(); cols]);
        for row in self.cells.iter_mut() {
            row.resize(cols, Cell::default());
        }
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.pending_wrap = false;
    }

    /// Scroll up: remove top line, push blank at bottom.
    fn scroll_up(&mut self) {
        if !self.cells.is_empty() {
            self.cells.remove(0);
            self.cells.push(vec![Cell::default(); self.cols]);
        }
    }

    fn set_cell(&mut self, row: usize, col: usize, ch: char) {
        if row < self.rows && col < self.cols {
            self.cells[row][col] = Cell {
                ch,
                fg: self.pen.fg(),
                bg: self.pen.bg(),
                bold: self.pen.bold,
                underline: self.pen.underline,
            };
        }
    }

    fn erase_line_from_cursor(&mut self) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        if row < self.rows {
            for c in col..self.cols {
                self.cells[row][c] = Cell::default();
            }
        }
    }

    fn erase_line(&mut self, row: usize) {
        if row < self.rows {
            self.cells[row] = vec![Cell::default(); self.cols];
        }
    }

    fn erase_screen(&mut self) {
        for row in 0..self.rows {
            self.cells[row] = vec![Cell::default(); self.cols];
        }
    }
}

// ── 256-colour palette ────────────────────────────────────────────────────────

fn ansi256_color(idx: u8) -> Color32 {
    // Standard 16 ANSI colours
    const ANSI16: [(u8, u8, u8); 16] = [
        (0, 0, 0),       // 0 black
        (170, 0, 0),     // 1 red
        (0, 170, 0),     // 2 green
        (170, 170, 0),   // 3 yellow
        (0, 0, 170),     // 4 blue
        (170, 0, 170),   // 5 magenta
        (0, 170, 170),   // 6 cyan
        (170, 170, 170), // 7 white
        (85, 85, 85),    // 8 bright black
        (255, 85, 85),   // 9 bright red
        (85, 255, 85),   // 10 bright green
        (255, 255, 85),  // 11 bright yellow
        (85, 85, 255),   // 12 bright blue
        (255, 85, 255),  // 13 bright magenta
        (85, 255, 255),  // 14 bright cyan
        (255, 255, 255), // 15 bright white
    ];
    if idx < 16 {
        let (r, g, b) = ANSI16[idx as usize];
        return Color32::from_rgb(r, g, b);
    }
    if idx >= 232 {
        // Greyscale ramp
        let v = 8 + (idx - 232) as u32 * 10;
        let v = v.min(255) as u8;
        return Color32::from_rgb(v, v, v);
    }
    // 6×6×6 colour cube (indices 16-231)
    let i = idx - 16;
    let b = i % 6;
    let g = (i / 6) % 6;
    let r = i / 36;
    let to_u8 = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    Color32::from_rgb(to_u8(r), to_u8(g), to_u8(b))
}

// ── SGR parser ────────────────────────────────────────────────────────────────

fn apply_sgr(pen: &mut Pen, params: &Params) {
    let mut iter = params.iter();
    loop {
        let Some(p) = iter.next() else { break };
        let code = p[0];
        match code {
            0 => *pen = Pen::default(),
            1 => pen.bold = true,
            4 => pen.underline = true,
            22 => pen.bold = false,
            24 => pen.underline = false,
            30..=37 => pen.fg = Some(ansi256_color(code as u8 - 30)),
            38 => {
                // Extended fg: 38;5;n or 38;2;r;g;b
                if let Some(sub) = iter.next() {
                    match sub[0] {
                        5 => {
                            if let Some(idx) = iter.next() {
                                pen.fg = Some(ansi256_color(idx[0] as u8));
                            }
                        }
                        2 => {
                            let r = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            let g = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            let b = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            pen.fg = Some(Color32::from_rgb(r, g, b));
                        }
                        _ => {}
                    }
                }
            }
            39 => pen.fg = None,
            40..=47 => pen.bg = Some(ansi256_color(code as u8 - 40)),
            48 => {
                if let Some(sub) = iter.next() {
                    match sub[0] {
                        5 => {
                            if let Some(idx) = iter.next() {
                                pen.bg = Some(ansi256_color(idx[0] as u8));
                            }
                        }
                        2 => {
                            let r = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            let g = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            let b = iter.next().map(|p| p[0] as u8).unwrap_or(0);
                            pen.bg = Some(Color32::from_rgb(r, g, b));
                        }
                        _ => {}
                    }
                }
            }
            49 => pen.bg = None,
            90..=97 => pen.fg = Some(ansi256_color(code as u8 - 90 + 8)),
            100..=107 => pen.bg = Some(ansi256_color(code as u8 - 100 + 8)),
            _ => {}
        }
    }
}

// ── vte::Perform impl ─────────────────────────────────────────────────────────

impl Perform for Screen {
    fn print(&mut self, c: char) {
        // Perform the deferred wrap that was set when the previous character
        // landed on the last column.  Escape sequences between two printable
        // characters cancel the pending wrap (they move the cursor themselves).
        if self.pending_wrap {
            self.pending_wrap = false;
            self.cursor_col = 0;
            self.cursor_row += 1;
            if self.cursor_row >= self.rows {
                self.scroll_up();
                self.cursor_row = self.rows - 1;
            }
        }
        self.set_cell(self.cursor_row, self.cursor_col, c);
        if self.cursor_col + 1 >= self.cols {
            // At the last column: defer the wrap so subsequent escape sequences
            // still see the cursor on this row.
            self.pending_wrap = true;
        } else {
            self.cursor_col += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                self.cursor_col = 0;
                self.pending_wrap = false;
            }
            b'\n' => {
                self.pending_wrap = false;
                self.cursor_row += 1;
                if self.cursor_row >= self.rows {
                    self.scroll_up();
                    self.cursor_row = self.rows - 1;
                }
            }
            0x08 => {
                self.pending_wrap = false;
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let p: Vec<u16> = params.iter().map(|s| s[0]).collect();
        let p1 = p.first().copied().unwrap_or(0) as usize;
        let p2 = p.get(1).copied().unwrap_or(0) as usize;

        match action {
            // Cursor up
            'A' => {
                self.pending_wrap = false;
                self.cursor_row = self.cursor_row.saturating_sub(p1.max(1));
            }
            // Cursor down
            'B' => {
                self.pending_wrap = false;
                self.cursor_row = (self.cursor_row + p1.max(1)).min(self.rows.saturating_sub(1));
            }
            // Cursor forward
            'C' => {
                self.pending_wrap = false;
                self.cursor_col = (self.cursor_col + p1.max(1)).min(self.cols.saturating_sub(1));
            }
            // Cursor back
            'D' => {
                self.pending_wrap = false;
                self.cursor_col = self.cursor_col.saturating_sub(p1.max(1));
            }
            // Cursor position (1-based)
            'H' | 'f' => {
                self.pending_wrap = false;
                self.cursor_row = p1.saturating_sub(1).min(self.rows.saturating_sub(1));
                self.cursor_col = p2.saturating_sub(1).min(self.cols.saturating_sub(1));
                if p1 == 0 { self.cursor_row = 0; }
                if p2 == 0 { self.cursor_col = 0; }
            }
            // Erase in display
            'J' => match p1 {
                0 => {
                    self.erase_line_from_cursor();
                    for r in (self.cursor_row + 1)..self.rows {
                        self.erase_line(r);
                    }
                }
                1 => {
                    for r in 0..self.cursor_row {
                        self.erase_line(r);
                    }
                }
                2 | 3 => self.erase_screen(),
                _ => {}
            },
            // Erase in line
            'K' => match p1 {
                0 => self.erase_line_from_cursor(),
                1 => {
                    let row = self.cursor_row;
                    let col = self.cursor_col;
                    if row < self.rows {
                        for c in 0..=col.min(self.cols.saturating_sub(1)) {
                            self.cells[row][c] = Cell::default();
                        }
                    }
                }
                2 => self.erase_line(self.cursor_row),
                _ => {}
            },
            // Insert lines
            'L' => {
                let n = p1.max(1);
                for _ in 0..n {
                    if self.cursor_row < self.rows {
                        self.cells.insert(self.cursor_row, vec![Cell::default(); self.cols]);
                        if self.cells.len() > self.rows {
                            self.cells.pop();
                        }
                    }
                }
            }
            // Delete lines
            'M' => {
                let n = p1.max(1);
                for _ in 0..n {
                    if self.cursor_row < self.cells.len() {
                        self.cells.remove(self.cursor_row);
                        self.cells.push(vec![Cell::default(); self.cols]);
                    }
                }
            }
            // Scroll up
            'S' => {
                for _ in 0..p1.max(1) {
                    self.scroll_up();
                }
            }
            // SGR
            'm' => apply_sgr(&mut self.pen, params),
            // Save / restore cursor (ANSI SC/RC, no intermediate)
            's' if intermediates.is_empty() => {
                self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap);
            }
            'u' if intermediates.is_empty() => {
                let (c, r, pw) = self.saved_cursor;
                self.cursor_col = c.min(self.cols.saturating_sub(1));
                self.cursor_row = r.min(self.rows.saturating_sub(1));
                self.pending_wrap = pw;
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = (self.cursor_col, self.cursor_row, self.pending_wrap),
            b'8' => {
                let (c, r, pw) = self.saved_cursor;
                self.cursor_col = c.min(self.cols.saturating_sub(1));
                self.cursor_row = r.min(self.rows.saturating_sub(1));
                self.pending_wrap = pw;
            }
            b'M' => {
                // Reverse index (scroll down)
                if self.cursor_row == 0 {
                    self.cells.insert(0, vec![Cell::default(); self.cols]);
                    if self.cells.len() > self.rows {
                        self.cells.pop();
                    }
                } else {
                    self.cursor_row -= 1;
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

// ── eframe App ───────────────────────────────────────────────────────────────

/// Shared state between the PTY reader thread and the render thread.
struct PtyState {
    screen: Screen,
    parser: Parser,
    #[allow(dead_code)] // reserved for future buffered keyboard input
    input_queue: Vec<u8>,
    child_exited: bool,
}

struct AppState {
    pty_master: Box<dyn portable_pty::MasterPty + Send>,
    pty_writer: Box<dyn Write + Send>,
    pty_state: Arc<Mutex<PtyState>>,
    cols: u16,
    rows: u16,
    cell_w: f32,
    cell_h: f32,
    font_size: f32,
}

fn find_tui_binary() -> Option<PathBuf> {
    // Look for tt-toplike-tui next to the current executable.
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("tt-toplike-tui");
    if candidate.exists() {
        return Some(candidate);
    }
    // Fallback: search PATH
    which_tui()
}

fn which_tui() -> Option<PathBuf> {
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let p = PathBuf::from(dir).join("tt-toplike-tui");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Encode an egui Key + Modifiers into ANSI bytes written to the PTY.
fn encode_key(key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    let shift = modifiers.shift;
    let _ctrl = modifiers.ctrl;
    Some(match key {
        Key::Enter => b"\r".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab => if shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() },
        Key::ArrowUp => b"\x1b[A".to_vec(),
        Key::ArrowDown => b"\x1b[B".to_vec(),
        Key::ArrowRight => b"\x1b[C".to_vec(),
        Key::ArrowLeft => b"\x1b[D".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => b"\x1b[15~".to_vec(),
        Key::F6 => b"\x1b[17~".to_vec(),
        Key::F7 => b"\x1b[18~".to_vec(),
        Key::F8 => b"\x1b[19~".to_vec(),
        Key::F9 => b"\x1b[20~".to_vec(),
        Key::F10 => b"\x1b[21~".to_vec(),
        Key::F11 => b"\x1b[23~".to_vec(),
        Key::F12 => b"\x1b[24~".to_vec(),
        _ => return None,
    })
}

struct TermApp {
    state: Option<AppState>,
    error: Option<String>,
}

impl TermApp {
    fn new(cc: &eframe::CreationContext<'_>, tui_args: Vec<String>) -> Self {
        // Monospace font — egui ships one by default.
        let font_size: f32 = 14.0;
        let cell_w: f32 = font_size * 0.6;
        let cell_h: f32 = font_size * 1.25;

        // Determine terminal size from available viewport.
        let vp = cc.egui_ctx.input(|i| {
            i.viewport().inner_rect
                .or(i.viewport().outer_rect)
                .unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)))
        });
        let cols = ((vp.width() / cell_w) as u16).max(80);
        let rows = ((vp.height() / cell_h) as u16).max(24);

        let tui_path = match find_tui_binary() {
            Some(p) => p,
            None => {
                return Self {
                    state: None,
                    error: Some(
                        "tt-toplike-tui not found. Place it next to tt-toplike-app.".into(),
                    ),
                }
            }
        };

        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => {
                return Self {
                    state: None,
                    error: Some(format!("Failed to open PTY: {e}")),
                }
            }
        };

        // Build child command
        let mut cmd = CommandBuilder::new(&tui_path);
        // Pass any extra args (e.g. --mode arcade)
        for arg in &tui_args {
            cmd.arg(arg);
        }
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "en_US.UTF-8");

        let _child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                return Self {
                    state: None,
                    error: Some(format!("Failed to spawn tt-toplike-tui: {e}")),
                }
            }
        };
        // slave is dropped after spawn — PTY ownership passes to child process.
        drop(pair.slave);

        let pty_writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                return Self {
                    state: None,
                    error: Some(format!("Failed to get PTY writer: {e}")),
                }
            }
        };

        let pty_state = Arc::new(Mutex::new(PtyState {
            screen: Screen::new(cols as usize, rows as usize),
            parser: Parser::default(),
            input_queue: Vec::new(),
            child_exited: false,
        }));

        // Spawn PTY reader thread
        {
            let state_clone = Arc::clone(&pty_state);
            let ctx = cc.egui_ctx.clone();
            let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
            std::thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => {
                                if let Ok(mut s) = state_clone.lock() {
                                    s.child_exited = true;
                                }
                                ctx.request_repaint();
                                break;
                            }
                            Ok(n) => {
                                if let Ok(mut s) = state_clone.lock() {
                                    // Feed bytes through VT parser into Screen.
                                    // We must split the borrow: extract bytes slice first,
                                    // then call advance with parser borrowing screen.
                                    let bytes_to_parse: Vec<u8> = buf[..n].to_vec();
                                    for byte in bytes_to_parse {
                                        // Safety: parser and screen are separate fields.
                                        // Use raw pointer split to satisfy borrow checker.
                                        let parser_ptr: *mut Parser = &mut s.parser;
                                        let screen_ptr: *mut Screen = &mut s.screen;
                                        unsafe { (*parser_ptr).advance(&mut *screen_ptr, byte) };
                                    }
                                }
                                ctx.request_repaint();
                            }
                        }
                    }
                })
                .expect("spawn pty-reader");
        }

        Self {
            state: Some(AppState {
                pty_master: pair.master,
                pty_writer,
                pty_state,
                cols,
                rows,
                cell_w,
                cell_h,
                font_size,
            }),
            error: None,
        }
    }
}

impl eframe::App for TermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Dark background matching TUI
        ctx.set_visuals(egui::Visuals::dark());

        if let Some(err) = &self.error {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Error: {err}"))
                        .color(Color32::RED)
                        .size(16.0),
                );
            });
            return;
        }

        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        // ── Handle keyboard input ──────────────────────────────────────────────
        ctx.input(|input| {
            let mut bytes: Vec<u8> = Vec::new();

            for event in &input.events {
                match event {
                    egui::Event::Text(text) => {
                        bytes.extend_from_slice(text.as_bytes());
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        if modifiers.ctrl {
                            // Ctrl+letter → single byte 0x01-0x1A
                            let name: &str = key.name();
                            if let Some(ch) = name.chars().next() {
                                if ch.is_ascii_alphabetic() {
                                    let b = (ch.to_ascii_uppercase() as u8).wrapping_sub(b'@');
                                    bytes.push(b);
                                    continue;
                                }
                            }
                        }
                        if let Some(seq) = encode_key(*key, *modifiers) {
                            bytes.extend_from_slice(&seq);
                        }
                    }
                    _ => {}
                }
            }

            if !bytes.is_empty() {
                let _ = state.pty_writer.write_all(&bytes);
                let _ = state.pty_writer.flush();
            }
        });

        // ── Handle window resize ───────────────────────────────────────────────
        let rect = ctx.available_rect();
        let new_cols = ((rect.width() / state.cell_w) as u16).max(10);
        let new_rows = ((rect.height() / state.cell_h) as u16).max(4);
        if new_cols != state.cols || new_rows != state.rows {
            state.cols = new_cols;
            state.rows = new_rows;
            let _ = state.pty_master.resize(PtySize {
                rows: new_rows,
                cols: new_cols,
                pixel_width: 0,
                pixel_height: 0,
            });
            if let Ok(mut s) = state.pty_state.lock() {
                s.screen.resize(new_cols as usize, new_rows as usize);
            }
        }

        // ── Render terminal ───────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                if let Ok(pty) = state.pty_state.lock() {
                    if pty.child_exited {
                        ui.label(
                            RichText::new("tt-toplike-tui exited. Close window.")
                                .color(Color32::YELLOW),
                        );
                        return;
                    }

                    let font_id = FontId::monospace(state.font_size);
                    let painter = ui.painter();
                    let origin = ui.min_rect().min;

                    for (row_idx, row) in pty.screen.cells.iter().enumerate() {
                        let y = origin.y + row_idx as f32 * state.cell_h;

                        // Draw background rects for the entire row in one pass, then text.
                        for (col_idx, cell) in row.iter().enumerate() {
                            let x = origin.x + col_idx as f32 * state.cell_w;

                            // Background
                            if cell.bg != Color32::TRANSPARENT {
                                let rect = egui::Rect::from_min_size(
                                    egui::pos2(x, y),
                                    egui::vec2(state.cell_w, state.cell_h),
                                );
                                painter.rect_filled(rect, 0.0, cell.bg);
                            }

                            // Character
                            if cell.ch != ' ' {
                                let mut job = egui::text::LayoutJob::default();
                                let mut fmt = egui::text::TextFormat::default();
                                fmt.font_id = font_id.clone();
                                fmt.color = cell.fg;
                                if cell.bold {
                                    fmt.color = brighten(cell.fg);
                                }
                                if cell.underline {
                                    fmt.underline = egui::Stroke::new(1.0, cell.fg);
                                }
                                job.append(&cell.ch.to_string(), 0.0, fmt);
                                let galley = painter.layout_job(job);
                                painter.galley(egui::pos2(x, y), galley, cell.fg);
                            }
                        }
                    }
                }
            });

        // Request continuous repaints so the TUI animation keeps running.
        ctx.request_repaint();
    }
}

/// Slightly brighten a colour for bold text.
fn brighten(c: Color32) -> Color32 {
    let [r, g, b, a] = c.to_array();
    Color32::from_rgba_premultiplied(
        r.saturating_add(50),
        g.saturating_add(50),
        b.saturating_add(50),
        a,
    )
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Collect args after `--` separator as child args.
    // e.g.: tt-toplike-app -- --mode arcade --mock --mock-devices 4
    let args: Vec<String> = std::env::args().collect();
    let tui_args: Vec<String> = args
        .iter()
        .skip_while(|a| *a != "--")
        .skip(1)
        .cloned()
        .collect();

    // Default: arcade mode
    let tui_args = if tui_args.is_empty() {
        vec!["--mode".into(), "arcade".into()]
    } else {
        tui_args
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TT-Toplike")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([400.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "TT-Toplike",
        native_options,
        Box::new(move |cc| Ok(Box::new(TermApp::new(cc, tui_args.clone())))),
    )?;

    Ok(())
}

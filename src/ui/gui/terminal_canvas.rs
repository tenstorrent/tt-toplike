// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.


//! Terminal Canvas - Renders a character grid with monospace font
//!
//! This widget renders a TerminalGrid using iced's canvas API,
//! creating a faux-terminal display in the GUI.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Text};
use iced::{Color, Point, Rectangle, Size, Theme};
use super::terminal_grid::TerminalGrid;

/// Terminal canvas widget for rendering character grids
pub struct TerminalCanvas {
    /// The terminal grid to render
    grid: TerminalGrid,
    /// Character cell width in pixels
    cell_width: f32,
    /// Character cell height in pixels
    cell_height: f32,
}

impl TerminalCanvas {
    /// Create a new terminal canvas
    ///
    /// # Arguments
    ///
    /// * `grid` - The terminal grid to render
    /// * `cell_width` - Width of each character cell in pixels
    /// * `cell_height` - Height of each character cell in pixels
    pub fn new(grid: TerminalGrid, cell_width: f32, cell_height: f32) -> Self {
        Self {
            grid,
            cell_width,
            cell_height,
        }
    }

    /// Update the grid content
    pub fn set_grid(&mut self, grid: TerminalGrid) {
        self.grid = grid;
    }

    /// Get the total width in pixels
    pub fn pixel_width(&self) -> f32 {
        self.grid.width() as f32 * self.cell_width
    }

    /// Get the total height in pixels
    pub fn pixel_height(&self) -> f32 {
        self.grid.height() as f32 * self.cell_height
    }
}

impl canvas::Program<()> for TerminalCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Draw background (terminal black)
        let background = Path::rectangle(Point::ORIGIN, bounds.size());
        frame.fill(
            &background,
            Color::from_rgb(0.05, 0.05, 0.08), // Dark blue-black
        );

        // Calculate actual cell size based on available space
        let actual_cell_width = bounds.width / self.grid.width() as f32;
        let actual_cell_height = bounds.height / self.grid.height() as f32;

        // Use the smaller of the two to maintain aspect ratio
        let cell_size = actual_cell_width.min(actual_cell_height);

        // Font size is slightly smaller than cell height for padding
        // iced 0.14+ requires f32 for Pixels::from()
        let font_size = cell_size * 0.85;

        // Render each character
        for (row, col, cell) in self.grid.iter_cells() {
            let x = col as f32 * cell_size;
            let y = row as f32 * cell_size;

            // Draw background color if specified
            if let Some(bg_color) = cell.bg_color {
                let bg_rect = Path::rectangle(
                    Point::new(x, y),
                    Size::new(cell_size, cell_size),
                );
                frame.fill(&bg_rect, bg_color);
            }

            // Draw character (skip spaces for performance)
            if cell.character != ' ' {
                let text = Text {
                    content: cell.character.to_string(),
                    position: Point::new(x + cell_size * 0.1, y + cell_size * 0.1),
                    color: cell.fg_color,
                    size: font_size.into(),
                    font: iced::Font::MONOSPACE,
                    ..Default::default()
                };
                frame.fill_text(text);
            }
        }

        vec![frame.into_geometry()]
    }
}

/// Create a terminal canvas widget
///
/// # Arguments
///
/// * `grid` - The terminal grid to render
/// * `cell_width` - Width of each character cell in pixels
/// * `cell_height` - Height of each character cell in pixels
///
/// # Example
///
/// ```rust,no_run
/// use tt_toplike::ui::gui::{TerminalGrid, terminal_canvas};
/// use iced::Color;
///
/// let mut grid = TerminalGrid::new(80, 24);
/// grid.write_str(0, 0, "Hello Terminal!", Color::from_rgb(0.0, 1.0, 0.0));
///
/// let canvas = terminal_canvas::view(grid, 10.0, 20.0);
/// ```
pub fn view(grid: TerminalGrid, cell_width: f32, cell_height: f32) -> Canvas<TerminalCanvas, (), Theme> {
    let program = TerminalCanvas::new(grid, cell_width, cell_height);
    Canvas::new(program)
}

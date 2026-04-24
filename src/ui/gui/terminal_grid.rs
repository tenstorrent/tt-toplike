//! Terminal Grid - Character-based display buffer
//!
//! This module provides a terminal emulator grid for rendering ASCII art
//! in the GUI with the same aesthetic as the TUI.

use iced::Color;

/// A single cell in the terminal grid
#[derive(Clone, Debug)]
pub struct TerminalCell {
    /// The character to display
    pub character: char,
    /// Foreground color (text color)
    pub fg_color: Color,
    /// Background color (optional, transparent if None)
    pub bg_color: Option<Color>,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            character: ' ',
            fg_color: Color::from_rgb(0.8, 0.8, 0.8),
            bg_color: None,
        }
    }
}

impl TerminalCell {
    /// Create a new terminal cell
    pub fn new(character: char, fg_color: Color) -> Self {
        Self {
            character,
            fg_color,
            bg_color: None,
        }
    }

    /// Create a terminal cell with background color
    pub fn with_bg(character: char, fg_color: Color, bg_color: Color) -> Self {
        Self {
            character,
            fg_color,
            bg_color: Some(bg_color),
        }
    }
}

/// Terminal grid for character-based rendering
///
/// This represents a terminal display as a grid of characters with colors,
/// similar to how a real terminal emulator works.
#[derive(Clone, Debug)]
pub struct TerminalGrid {
    /// Grid width in characters
    width: usize,
    /// Grid height in characters
    height: usize,
    /// The grid cells (row-major order)
    cells: Vec<Vec<TerminalCell>>,
}

impl TerminalGrid {
    /// Create a new terminal grid with the specified dimensions
    ///
    /// # Arguments
    ///
    /// * `width` - Width in characters
    /// * `height` - Height in characters
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use tt_toplike::ui::gui::terminal_grid::TerminalGrid;
    ///
    /// let grid = TerminalGrid::new(80, 24);  // Classic terminal size
    /// ```
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![vec![TerminalCell::default(); width]; height];
        Self {
            width,
            height,
            cells,
        }
    }

    /// Get grid width in characters
    pub fn width(&self) -> usize {
        self.width
    }

    /// Get grid height in characters
    pub fn height(&self) -> usize {
        self.height
    }

    /// Get a reference to a cell at the specified position
    ///
    /// Returns None if position is out of bounds.
    pub fn get(&self, row: usize, col: usize) -> Option<&TerminalCell> {
        self.cells.get(row).and_then(|r| r.get(col))
    }

    /// Get a mutable reference to a cell at the specified position
    ///
    /// Returns None if position is out of bounds.
    pub fn get_mut(&mut self, row: usize, col: usize) -> Option<&mut TerminalCell> {
        self.cells.get_mut(row).and_then(|r| r.get_mut(col))
    }

    /// Set a cell at the specified position
    ///
    /// Does nothing if position is out of bounds.
    pub fn set(&mut self, row: usize, col: usize, cell: TerminalCell) {
        if let Some(target) = self.get_mut(row, col) {
            *target = cell;
        }
    }

    /// Set a character with color at the specified position
    ///
    /// # Arguments
    ///
    /// * `row` - Row index (0-based)
    /// * `col` - Column index (0-based)
    /// * `character` - Character to display
    /// * `fg_color` - Foreground color
    pub fn set_char(&mut self, row: usize, col: usize, character: char, fg_color: Color) {
        self.set(row, col, TerminalCell::new(character, fg_color));
    }

    /// Set a character with foreground and background colors
    pub fn set_char_with_bg(
        &mut self,
        row: usize,
        col: usize,
        character: char,
        fg_color: Color,
        bg_color: Color,
    ) {
        self.set(row, col, TerminalCell::with_bg(character, fg_color, bg_color));
    }

    /// Clear the entire grid to default cells (spaces)
    pub fn clear(&mut self) {
        for row in &mut self.cells {
            for cell in row {
                *cell = TerminalCell::default();
            }
        }
    }

    /// Clear the grid with a specific character and color
    pub fn clear_with(&mut self, character: char, color: Color) {
        for row in &mut self.cells {
            for cell in row {
                *cell = TerminalCell::new(character, color);
            }
        }
    }

    /// Write a string at the specified position
    ///
    /// Stops writing if it reaches the end of the line.
    ///
    /// # Arguments
    ///
    /// * `row` - Starting row
    /// * `col` - Starting column
    /// * `text` - Text to write
    /// * `color` - Text color
    pub fn write_str(&mut self, row: usize, col: usize, text: &str, color: Color) {
        for (i, ch) in text.chars().enumerate() {
            let c = col + i;
            if c >= self.width {
                break;
            }
            self.set_char(row, c, ch, color);
        }
    }

    /// Write centered text on a specific row
    ///
    /// # Arguments
    ///
    /// * `row` - Row to write on
    /// * `text` - Text to center
    /// * `color` - Text color
    pub fn write_centered(&mut self, row: usize, text: &str, color: Color) {
        let text_len = text.chars().count();
        if text_len >= self.width {
            self.write_str(row, 0, text, color);
            return;
        }

        let start_col = (self.width - text_len) / 2;
        self.write_str(row, start_col, text, color);
    }

    /// Draw a horizontal line using box-drawing characters
    ///
    /// # Arguments
    ///
    /// * `row` - Row to draw on
    /// * `start_col` - Starting column
    /// * `end_col` - Ending column (inclusive)
    /// * `color` - Line color
    pub fn draw_hline(&mut self, row: usize, start_col: usize, end_col: usize, color: Color) {
        for col in start_col..=end_col.min(self.width - 1) {
            self.set_char(row, col, '─', color);
        }
    }

    /// Draw a vertical line using box-drawing characters
    ///
    /// # Arguments
    ///
    /// * `col` - Column to draw on
    /// * `start_row` - Starting row
    /// * `end_row` - Ending row (inclusive)
    /// * `color` - Line color
    pub fn draw_vline(&mut self, col: usize, start_row: usize, end_row: usize, color: Color) {
        for row in start_row..=end_row.min(self.height - 1) {
            self.set_char(row, col, '│', color);
        }
    }

    /// Draw a box border using box-drawing characters
    ///
    /// # Arguments
    ///
    /// * `row` - Top row
    /// * `col` - Left column
    /// * `width` - Box width
    /// * `height` - Box height
    /// * `color` - Border color
    pub fn draw_box(&mut self, row: usize, col: usize, width: usize, height: usize, color: Color) {
        if width < 2 || height < 2 {
            return;
        }

        // Top-left corner
        self.set_char(row, col, '┌', color);
        // Top-right corner
        self.set_char(row, col + width - 1, '┐', color);
        // Bottom-left corner
        self.set_char(row + height - 1, col, '└', color);
        // Bottom-right corner
        self.set_char(row + height - 1, col + width - 1, '┘', color);

        // Top and bottom edges
        self.draw_hline(row, col + 1, col + width - 2, color);
        self.draw_hline(row + height - 1, col + 1, col + width - 2, color);

        // Left and right edges
        self.draw_vline(col, row + 1, row + height - 2, color);
        self.draw_vline(col + width - 1, row + 1, row + height - 2, color);
    }

    /// Iterate over all cells with their positions
    ///
    /// Returns an iterator of (row, col, &TerminalCell).
    pub fn iter_cells(&self) -> impl Iterator<Item = (usize, usize, &TerminalCell)> {
        self.cells.iter().enumerate().flat_map(|(row, row_cells)| {
            row_cells
                .iter()
                .enumerate()
                .map(move |(col, cell)| (row, col, cell))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_creation() {
        let grid = TerminalGrid::new(80, 24);
        assert_eq!(grid.width(), 80);
        assert_eq!(grid.height(), 24);
    }

    #[test]
    fn test_set_and_get() {
        let mut grid = TerminalGrid::new(10, 10);
        let color = Color::from_rgb(1.0, 0.0, 0.0);
        grid.set_char(5, 5, 'X', color);

        let cell = grid.get(5, 5).unwrap();
        assert_eq!(cell.character, 'X');
    }

    #[test]
    fn test_write_str() {
        let mut grid = TerminalGrid::new(20, 5);
        let color = Color::from_rgb(1.0, 1.0, 1.0);
        grid.write_str(2, 5, "Hello", color);

        assert_eq!(grid.get(2, 5).unwrap().character, 'H');
        assert_eq!(grid.get(2, 6).unwrap().character, 'e');
        assert_eq!(grid.get(2, 7).unwrap().character, 'l');
        assert_eq!(grid.get(2, 8).unwrap().character, 'l');
        assert_eq!(grid.get(2, 9).unwrap().character, 'o');
    }

    #[test]
    fn test_write_centered() {
        let mut grid = TerminalGrid::new(20, 5);
        let color = Color::from_rgb(1.0, 1.0, 1.0);
        grid.write_centered(2, "Test", color);

        // "Test" is 4 characters, centered in 20 = starts at (20-4)/2 = 8
        assert_eq!(grid.get(2, 8).unwrap().character, 'T');
        assert_eq!(grid.get(2, 9).unwrap().character, 'e');
        assert_eq!(grid.get(2, 10).unwrap().character, 's');
        assert_eq!(grid.get(2, 11).unwrap().character, 't');
    }

    #[test]
    fn test_draw_box() {
        let mut grid = TerminalGrid::new(20, 10);
        let color = Color::from_rgb(1.0, 1.0, 1.0);
        grid.draw_box(2, 3, 10, 5, color);

        // Check corners
        assert_eq!(grid.get(2, 3).unwrap().character, '┌');
        assert_eq!(grid.get(2, 12).unwrap().character, '┐');
        assert_eq!(grid.get(6, 3).unwrap().character, '└');
        assert_eq!(grid.get(6, 12).unwrap().character, '┘');

        // Check edges
        assert_eq!(grid.get(2, 4).unwrap().character, '─');
        assert_eq!(grid.get(3, 3).unwrap().character, '│');
    }
}

//! Terminal frame capture and cell inspection.
//!
//! Converts a raw ANSI byte stream from a PTY into a [`TerminalFrame`] backed
//! by `vt100::Parser`. The frame exposes per-cell text, foreground/background
//! colors, and style flags so assertions can inspect terminal state without
//! screenshot OCR.

use unicode_width::UnicodeWidthStr;

use crate::locator::MatchedSpan;
use crate::region::Region;

/// Bit flag for bold style.
const BOLD_FLAG: u8 = 0x01;
/// Bit flag for italic style.
const ITALIC_FLAG: u8 = 0x02;
/// Bit flag for underline style.
const UNDERLINE_FLAG: u8 = 0x04;
/// Bit flag for inverse style.
const INVERSE_FLAG: u8 = 0x08;
/// Bit flag for dim (faint) style.
const DIM_FLAG: u8 = 0x10;

/// RGBA color extracted from a terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellColor {
    /// Blue component (0–255).
    pub blue: u8,
    /// Green component (0–255).
    pub green: u8,
    /// Red component (0–255).
    pub red: u8,
}

impl CellColor {
    /// Create a new cell color from RGB components.
    pub fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { blue, green, red }
    }

    /// White color constant.
    pub fn white() -> Self {
        Self::new(255, 255, 255)
    }

    /// Black color constant.
    pub fn black() -> Self {
        Self::new(0, 0, 0)
    }
}

/// Style flags for a terminal cell, stored as a compact bitfield.
///
/// Each flag occupies one bit: bold (0x01), italic (0x02), underline (0x04),
/// inverse (0x08), dim (0x10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    /// Packed bit flags for bold, italic, underline, inverse, and dim.
    flags: u8,
}

impl CellStyle {
    /// Create a `CellStyle` from raw flag bits.
    ///
    /// Bits: bold = `0x01`, italic = `0x02`, underline = `0x04`,
    /// inverse = `0x08`, dim = `0x10`.
    pub fn from_raw(flags: u8) -> Self {
        Self { flags }
    }

    /// Whether the cell is rendered in bold.
    pub fn bold(self) -> bool {
        self.flags & BOLD_FLAG != 0
    }

    /// Whether the cell is rendered in italic.
    pub fn italic(self) -> bool {
        self.flags & ITALIC_FLAG != 0
    }

    /// Whether the cell is underlined.
    pub fn underline(self) -> bool {
        self.flags & UNDERLINE_FLAG != 0
    }

    /// Whether foreground and background colors are swapped.
    pub fn inverse(self) -> bool {
        self.flags & INVERSE_FLAG != 0
    }

    /// Whether the cell is rendered with diminished intensity.
    pub fn dim(self) -> bool {
        self.flags & DIM_FLAG != 0
    }

    /// Extract style flags from a `vt100` terminal cell.
    ///
    /// Crate-internal: this borrows a `vt100::Cell`, an implementation-detail
    /// type that must not appear in testty's public API. External callers
    /// obtain a [`CellStyle`] through [`TerminalFrame::cell_style`] instead.
    pub(crate) fn from_cell(cell: &vt100::Cell) -> Self {
        let mut flags = 0u8;
        if cell.bold() {
            flags |= BOLD_FLAG;
        }
        if cell.italic() {
            flags |= ITALIC_FLAG;
        }
        if cell.underline() {
            flags |= UNDERLINE_FLAG;
        }
        if cell.inverse() {
            flags |= INVERSE_FLAG;
        }
        if cell.dim() {
            flags |= DIM_FLAG;
        }

        Self { flags }
    }
}

/// A snapshot of the terminal state at a point in time.
///
/// Wraps a `vt100::Parser` to provide structured access to cell contents,
/// colors, and styles across the terminal grid.
pub struct TerminalFrame {
    /// The underlying vt100 parser holding terminal state.
    parser: vt100::Parser,
}

impl TerminalFrame {
    /// Create a new frame by processing raw ANSI bytes through a terminal
    /// parser configured with the given dimensions.
    pub fn new(cols: u16, rows: u16, data: &[u8]) -> Self {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(data);

        Self { parser }
    }

    /// Return the number of columns in the terminal grid.
    pub fn cols(&self) -> u16 {
        self.parser.screen().size().1
    }

    /// Return the number of rows in the terminal grid.
    pub fn rows(&self) -> u16 {
        self.parser.screen().size().0
    }

    /// Extract the visible text from a single row, trimming trailing spaces.
    ///
    /// Wide-character continuation cells (the trailing half of a wide
    /// glyph) contribute nothing because the leading cell already supplies
    /// the full character. All other cells — including blank cells that
    /// were never written — are represented as a single space so column
    /// positions in the rendered terminal are preserved and substring
    /// searches do not collapse text across distant columns.
    pub fn row_text(&self, row: u16) -> String {
        let screen = self.parser.screen();
        let cols = self.cols();
        let mut text = String::with_capacity(usize::from(cols));

        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                text.push(' ');
                continue;
            };

            if cell.is_wide_continuation() {
                continue;
            }

            let contents = cell.contents();
            if contents.is_empty() {
                text.push(' ');
            } else {
                text.push_str(contents);
            }
        }

        text.trim_end().to_string()
    }

    /// Extract text from all visible rows, joined by newlines.
    ///
    /// Each row is produced by [`Self::row_text`], so wide-character
    /// continuation cells are skipped while real blank columns are
    /// preserved as spaces.
    pub fn all_text(&self) -> String {
        let rows = self.rows();
        let mut lines = Vec::with_capacity(usize::from(rows));

        for row in 0..rows {
            lines.push(self.row_text(row));
        }

        // Trim trailing empty lines.
        while lines.last().is_some_and(std::string::String::is_empty) {
            lines.pop();
        }

        lines.join("\n")
    }

    /// Return ANSI-formatted bytes that reproduce the current screen state.
    ///
    /// The returned bytes include escape sequences for colors and styles,
    /// so passing them to [`TerminalFrame::new()`] with the same dimensions
    /// produces a frame with identical cell metadata.
    pub fn contents_formatted(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }

    /// Return the text content of a single cell without region overhead.
    ///
    /// Returns `" "` for out-of-bounds cells or wide-character continuation
    /// cells that produce no content. This is cheaper than
    /// [`text_in_region()`](Self::text_in_region) with a 1×1 region because
    /// it avoids `Vec`/join/trim allocation.
    pub fn cell_text(&self, row: u16, col: u16) -> &str {
        self.parser.screen().cell(row, col).map_or(" ", |cell| {
            let text = cell.contents();
            if text.is_empty() { " " } else { text }
        })
    }

    /// Extract text only from cells within the given region.
    ///
    /// Wide-character continuation cells inside the region contribute
    /// nothing, matching the behavior of [`Self::row_text`]. Real blank
    /// columns are preserved as spaces so column positions inside the
    /// region remain accurate.
    pub fn text_in_region(&self, region: &Region) -> String {
        let mut lines = Vec::new();

        for row in region.row..region.bottom().min(self.rows()) {
            let screen = self.parser.screen();
            let mut line = String::new();

            for col in region.col..region.right().min(self.cols()) {
                let Some(cell) = screen.cell(row, col) else {
                    line.push(' ');
                    continue;
                };

                if cell.is_wide_continuation() {
                    continue;
                }

                let contents = cell.contents();
                if contents.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(contents);
                }
            }

            lines.push(line.trim_end().to_string());
        }

        while lines.last().is_some_and(std::string::String::is_empty) {
            lines.pop();
        }

        lines.join("\n")
    }

    /// Find all occurrences of `needle` in the terminal grid.
    ///
    /// Returns matched spans with their position, style, and color data.
    /// Uses a byte-offset-to-column mapping so multibyte and wide characters
    /// are located at the correct terminal column. Returns an empty list when
    /// `needle` is empty.
    pub fn find_text(&self, needle: &str) -> Vec<MatchedSpan> {
        if needle.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();

        for row in 0..self.rows() {
            let (row_content, byte_to_col) = self.row_text_with_column_map(row);
            let mut search_start = 0;

            while let Some(byte_offset) = row_content[search_start..].find(needle) {
                let match_byte_start = search_start + byte_offset;
                let match_byte_end = match_byte_start + needle.len();

                let start_col = byte_to_col[match_byte_start];
                let end_col = byte_to_col[match_byte_end];
                let span_length = end_col - start_col;

                let span = self.extract_span(row, start_col, span_length);
                matches.push(span);

                // Advance by one full character to stay on a character
                // boundary (byte + 1 may land inside a multi-byte char).
                search_start = match_byte_start
                    + row_content[match_byte_start..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
            }
        }

        matches
    }

    /// Find occurrences of `needle` within a specific region.
    pub fn find_text_in_region(&self, needle: &str, region: &Region) -> Vec<MatchedSpan> {
        self.find_text(needle)
            .into_iter()
            .filter(|span| region.encloses(&span.rect))
            .collect()
    }

    /// Extract the foreground color of a cell at the given position.
    pub fn fg_color(&self, row: u16, col: u16) -> Option<CellColor> {
        let cell = self.parser.screen().cell(row, col)?;

        convert_vt100_color(cell.fgcolor())
    }

    /// Extract the background color of a cell at the given position.
    pub fn bg_color(&self, row: u16, col: u16) -> Option<CellColor> {
        let cell = self.parser.screen().cell(row, col)?;

        convert_vt100_color(cell.bgcolor())
    }

    /// Extract style flags for a cell at the given position.
    pub fn cell_style(&self, row: u16, col: u16) -> Option<CellStyle> {
        let cell = self.parser.screen().cell(row, col)?;

        Some(CellStyle::from_cell(cell))
    }

    /// Build row text with a parallel byte-offset-to-column mapping.
    ///
    /// Returns `(text, byte_to_col)` where `byte_to_col[i]` is the terminal
    /// column that produced byte `i`. An extra sentinel entry at
    /// `byte_to_col[text.len()]` holds the column immediately after the last
    /// cell, enabling end-of-match span calculations. Trailing whitespace is
    /// trimmed to match [`Self::row_text`] behavior. Empty non-continuation
    /// cells contribute one space so text searches preserve visible columns.
    fn row_text_with_column_map(&self, row: u16) -> (String, Vec<u16>) {
        let screen = self.parser.screen();
        let cols = self.cols();
        let mut text = String::with_capacity(usize::from(cols));
        let mut byte_to_col = Vec::with_capacity(usize::from(cols) + 1);

        for col in 0..cols {
            let cell = screen.cell(row, col);
            if cell.is_some_and(vt100::Cell::is_wide_continuation) {
                continue;
            }

            let contents = cell.map_or("", |cell| cell.contents());
            if contents.is_empty() {
                text.push(' ');
                byte_to_col.push(col);
                continue;
            }

            for _ in 0..contents.len() {
                byte_to_col.push(col);
            }
            text.push_str(contents);
        }

        // Trim trailing whitespace to match row_text().
        let trimmed_len = text.trim_end().len();
        text.truncate(trimmed_len);
        byte_to_col.truncate(trimmed_len);

        // Sentinel: column just past the last cell. Uses the Unicode
        // display width of the last cell's content to account for wide
        // characters that occupy two terminal columns.
        let sentinel = if trimmed_len > 0 {
            let last_col = byte_to_col[trimmed_len - 1];
            let last_contents = screen
                .cell(row, last_col)
                .map_or("", |cell| cell.contents());
            let display_width = UnicodeWidthStr::width(last_contents).max(1);

            last_col + u16::try_from(display_width).unwrap_or(1)
        } else {
            0
        };
        byte_to_col.push(sentinel);

        (text, byte_to_col)
    }

    /// Extract a [`MatchedSpan`] for a range of cells on a single row.
    fn extract_span(&self, row: u16, col: u16, length: u16) -> MatchedSpan {
        let screen = self.parser.screen();
        let mut text = String::new();

        let first_cell = screen.cell(row, col);
        let foreground = first_cell.and_then(|cell| convert_vt100_color(cell.fgcolor()));
        let background = first_cell.and_then(|cell| convert_vt100_color(cell.bgcolor()));
        let style = first_cell.map(CellStyle::from_cell).unwrap_or_default();

        for offset in 0..length {
            if let Some(cell) = screen.cell(row, col + offset) {
                text.push_str(cell.contents());
            }
        }

        MatchedSpan {
            text,
            rect: Region::new(col, row, length, 1),
            foreground,
            background,
            style,
        }
    }
}

/// Convert a `vt100::Color` to a [`CellColor`].
///
/// Returns `None` for the default terminal color since it depends on the
/// user's terminal configuration.
fn convert_vt100_color(color: vt100::Color) -> Option<CellColor> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(idx) => Some(ansi_index_to_rgb(idx)),
        vt100::Color::Rgb(red, green, blue) => Some(CellColor::new(red, green, blue)),
    }
}

/// Map a standard ANSI color index (0–255) to an approximate RGB value.
fn ansi_index_to_rgb(idx: u8) -> CellColor {
    match idx {
        // Standard 16 colors (approximate).
        0 => CellColor::new(0, 0, 0),
        1 => CellColor::new(128, 0, 0),
        2 => CellColor::new(0, 128, 0),
        3 => CellColor::new(128, 128, 0),
        4 => CellColor::new(0, 0, 128),
        5 => CellColor::new(128, 0, 128),
        6 => CellColor::new(0, 128, 128),
        7 => CellColor::new(192, 192, 192),
        8 => CellColor::new(128, 128, 128),
        9 => CellColor::new(255, 0, 0),
        10 => CellColor::new(0, 255, 0),
        11 => CellColor::new(255, 255, 0),
        12 => CellColor::new(0, 0, 255),
        13 => CellColor::new(255, 0, 255),
        14 => CellColor::new(0, 255, 255),
        15 => CellColor::new(255, 255, 255),
        // 216-color cube (indices 16–231).
        16..=231 => {
            let adjusted = idx - 16;
            let blue = adjusted % 6;
            let green = (adjusted / 6) % 6;
            let red = adjusted / 36;
            let to_component = |value: u8| -> u8 { if value == 0 { 0 } else { 55 + 40 * value } };

            CellColor::new(to_component(red), to_component(green), to_component(blue))
        }
        // Grayscale ramp (indices 232–255).
        232..=255 => {
            let level = 8 + 10 * (idx - 232);

            CellColor::new(level, level, level)
        }
    }
}

#[cfg(test)]
#[path = "frame_test.rs"]
mod tests;

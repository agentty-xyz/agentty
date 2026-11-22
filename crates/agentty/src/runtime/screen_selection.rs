//! Mouse text selection and link clicks over the rendered screen.
//!
//! Inside `tmux` Agentty takes mouse reports to keep the wheel working, so
//! the outer terminal no longer selects text or opens links there. The
//! runtime does both from the frame buffer instead.

use std::cell::{Cell, RefCell};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthStr;

/// URL schemes that start a clickable link.
const LINK_SCHEMES: [&str; 2] = ["https://", "http://"];
/// Punctuation that ends a sentence or wraps a link without being part of it.
const TRAILING_LINK_PUNCTUATION: [char; 10] = ['.', ',', ';', ':', '!', '?', '\'', '"', '*', ']'];

/// What the runtime does for a finished mouse gesture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScreenAction {
    /// Copy the selected screen text.
    Copy(String),
    /// Open the clicked link.
    OpenLink(String),
}

/// Tracks left-button gestures and resolves them against the drawn frame.
#[derive(Default)]
pub(crate) struct ScreenSelection {
    /// Actions of finished gestures, waiting for the runtime to take them
    /// with the next frame. Reports can arrive in a burst, so several
    /// gestures may finish between two frames.
    finished: RefCell<Vec<ScreenAction>>,
    /// Copy of the frame drawn last, which is the one the user points at.
    /// Ratatui keeps that frame private, and the next one may already show
    /// scrolled or new content.
    frame: RefCell<Buffer>,
    /// Gesture whose button is still down.
    pressed: Cell<Option<Gesture>>,
}

impl ScreenSelection {
    /// Applies one mouse report and returns whether the screen needs a
    /// redraw.
    ///
    /// A release is resolved at once against the frame drawn last: a drag
    /// copies the selected text, and a click without a drag opens the link
    /// under it, if any. Keys, scrolling, or new output that arrive before
    /// the next frame therefore cannot change what the gesture meant.
    pub(crate) fn handle_mouse(&self, mouse: MouseEvent) -> bool {
        let position = Position::new(mouse.column, mouse.row);
        match (mouse.kind, self.pressed.get()) {
            (MouseEventKind::Down(MouseButton::Left), _) => {
                self.pressed.set(Some(Gesture {
                    anchor: position,
                    head: position,
                }));
            }
            (MouseEventKind::Drag(MouseButton::Left), Some(gesture)) => {
                self.pressed.set(Some(Gesture {
                    head: position,
                    ..gesture
                }));
            }
            (MouseEventKind::Up(MouseButton::Left), Some(gesture)) => {
                self.pressed.set(None);
                let gesture = Gesture {
                    head: position,
                    ..gesture
                };
                self.finished
                    .borrow_mut()
                    .extend(gesture.action(&self.frame.borrow()));
            }
            _ => return false,
        }

        true
    }

    /// Drops a selection whose button is still down and returns whether there
    /// was one. A gesture already released keeps its action.
    pub(crate) fn cancel(&self) -> bool {
        self.pressed.take().is_some()
    }

    /// Remembers the freshly rendered `buffer` for the next release,
    /// highlights the selection whose button is still down, and returns the
    /// actions of the gestures released since the last frame.
    pub(crate) fn apply(&self, buffer: &mut Buffer) -> Vec<ScreenAction> {
        {
            let mut frame = self.frame.borrow_mut();
            frame.area = buffer.area;
            frame.content.clone_from(&buffer.content);
        }
        if let Some(gesture) = self.pressed.get() {
            gesture.highlight(buffer);
        }

        self.finished.take()
    }
}

/// One left-button gesture from the pressed cell to the pointer's cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Gesture {
    /// Cell where the button went down.
    anchor: Position,
    /// Cell the pointer reached; equal to `anchor` until it drags.
    head: Position,
}

impl Gesture {
    /// Returns what a released gesture does on the drawn `buffer`.
    fn action(self, buffer: &Buffer) -> Option<ScreenAction> {
        if self.anchor == self.head {
            return link_at(buffer, self.head).map(ScreenAction::OpenLink);
        }
        let text = selected_text(buffer, self.anchor, self.head);

        (!text.is_empty()).then_some(ScreenAction::Copy(text))
    }

    /// Reverses the colors of every selected cell. A press without a drag
    /// selects nothing.
    fn highlight(self, buffer: &mut Buffer) {
        if self.anchor == self.head {
            return;
        }
        for (row, first_column, last_column) in selected_rows(buffer.area, self.anchor, self.head) {
            for column in first_column..=last_column {
                buffer[(column, row)].modifier.toggle(Modifier::REVERSED);
            }
        }
    }
}

/// Returns the selected text, one line per screen row, without trailing
/// blanks.
fn selected_text(buffer: &Buffer, anchor: Position, head: Position) -> String {
    let lines: Vec<String> = selected_rows(buffer.area, anchor, head)
        .map(|(row, first_column, last_column)| {
            let (text, _) = row_text(buffer, row, first_column, last_column, first_column);

            text.trim_end().to_string()
        })
        .collect();

    lines.join("\n").trim_end().to_string()
}

/// Returns the link drawn under `position`, if any.
fn link_at(buffer: &Buffer, position: Position) -> Option<String> {
    let area = buffer.area;
    if !area.contains(position) {
        return None;
    }
    let (text, click_offset) = row_text(
        buffer,
        position.y,
        area.left(),
        area.right().saturating_sub(1),
        position.x,
    );

    link_at_offset(&text, click_offset)
}

/// Yields each selected row with its first and last selected column.
///
/// The selection runs in reading order between the two ends, like a terminal
/// selection: whole rows in between, clamped to `area`.
fn selected_rows(
    area: Rect,
    anchor: Position,
    head: Position,
) -> impl Iterator<Item = (u16, u16, u16)> {
    let last_area_column = area.right().saturating_sub(1);
    let clamp = |position: Position| {
        Position::new(
            position.x.min(last_area_column).max(area.left()),
            position
                .y
                .min(area.bottom().saturating_sub(1))
                .max(area.top()),
        )
    };
    let (anchor, head) = (clamp(anchor), clamp(head));
    let (start, end) = if (anchor.y, anchor.x) <= (head.y, head.x) {
        (anchor, head)
    } else {
        (head, anchor)
    };

    (start.y..=end.y)
        .filter(move |_| !area.is_empty())
        .map(move |row| {
            let first_column = if row == start.y { start.x } else { area.left() };
            let last_column = if row == end.y {
                end.x
            } else {
                last_area_column
            };

            (row, first_column, last_column)
        })
}

/// Returns the text of one row between two columns, with the byte offset of
/// the symbol drawn at `marked_column`.
///
/// A wide symbol occupies several cells but is stored in the first one, so
/// the cells it hides are skipped.
fn row_text(
    buffer: &Buffer,
    row: u16,
    first_column: u16,
    last_column: u16,
    marked_column: u16,
) -> (String, usize) {
    let mut text = String::new();
    let mut marked_offset = 0;
    let mut column = first_column;
    while column <= last_column {
        let symbol = buffer[(column, row)].symbol();
        let width = u16::try_from(symbol.width()).unwrap_or(1).max(1);
        if (column..column.saturating_add(width)).contains(&marked_column) {
            marked_offset = text.len();
        }
        text.push_str(symbol);
        column = column.saturating_add(width);
    }

    (text, marked_offset)
}

/// Returns the link in `text` that covers byte `offset`, if any.
fn link_at_offset(text: &str, offset: usize) -> Option<String> {
    LINK_SCHEMES.iter().find_map(|scheme| {
        text.match_indices(scheme).find_map(|(start, _)| {
            let rest = &text[start..];
            let end = rest
                .find(|character| !is_link_character(character))
                .unwrap_or(rest.len());
            let link = trim_link(&rest[..end]);
            let is_clicked = (start..start + link.len()).contains(&offset);

            (is_clicked && link.len() > scheme.len()).then(|| link.to_string())
        })
    })
}

/// Returns whether `character` can appear inside a link. Borders, spaces,
/// and quoting characters end one.
fn is_link_character(character: char) -> bool {
    character.is_ascii_graphic() && !matches!(character, '"' | '<' | '>' | '`' | '|')
}

/// Removes punctuation that follows a link in prose, keeping a closing
/// parenthesis that the link itself opened.
fn trim_link(link: &str) -> &str {
    let mut link = link;
    while let Some(last) = link.chars().next_back() {
        let is_trailing = match last {
            ')' => link.matches(')').count() > link.matches('(').count(),
            _ => TRAILING_LINK_PUNCTUATION.contains(&last),
        };
        if !is_trailing {
            break;
        }
        link = &link[..link.len() - last.len_utf8()];
    }

    link
}

#[cfg(test)]
#[path = "screen_selection_test.rs"]
mod tests;

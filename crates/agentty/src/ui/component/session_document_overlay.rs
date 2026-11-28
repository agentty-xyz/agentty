//! Centered popup listing a session's shared notes document, entry by
//! entry, with each entry's version and author, and the typed confirmation
//! that clears it.

use std::cell::RefCell;
use std::rc::Rc;

use ag_session::{SESSION_DOCUMENT_BODY_MAX_BYTES, SessionDocument, SessionDocumentEntry};
use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::presentation::help_action::HelpAction;
use crate::presentation::session_document::{CLEAR_CONFIRMATION_WORD, SessionDocumentPanel};
use crate::ui::style::{self, palette};
use crate::ui::{Component, help_format, overlay};

/// Bottom-border shortcut that dismisses the overlay.
const CLOSE_ACTION: HelpAction = HelpAction::new("close", "Esc", "Close");

/// Bottom-border shortcut shown when the panel content overflows the popup.
const SCROLL_ACTION: HelpAction = HelpAction::new("scroll", "j/k", "Scroll");

/// Bottom-border shortcut that starts clearing the document.
const CLEAR_ACTION: HelpAction = HelpAction::new("clear", "x", "Clear");

/// Bottom-border shortcuts while the clearing confirmation is open.
const CONFIRMATION_ACTIONS: [HelpAction; 2] = [
    HelpAction::new("confirm", "Enter", "Confirm"),
    HelpAction::new("cancel", "Esc", "Cancel"),
];

/// Share of the drawing area's width the popup asks for.
const POPUP_WIDTH_PERCENT: u16 = 80;

/// Narrowest popup the panel asks for, so notes stay readable.
const MIN_POPUP_WIDTH: u16 = 60;

/// Columns every line of a note's text is indented by, so no line of it
/// can start where entry headings do and pass for one.
const NOTE_TEXT_INDENT: usize = 2;

/// Body shown before any graph node adds a note.
const EMPTY_DOCUMENT_MESSAGE: &str =
    "No session notes yet. Graph nodes add findings, decisions, and hand-offs here as they run.";

/// Cache of the wrapped entry lines of the last drawn document.
///
/// Keyed by session, document version, content width, and the active
/// theme: every write advances the version, so a reload with new entries, a
/// resize, or a theme switch rebuilds it, and only one document is kept.
#[derive(Default)]
pub(crate) struct SessionDocumentLayoutCache {
    cached: RefCell<Option<CachedEntryLines>>,
}

impl SessionDocumentLayoutCache {
    /// Returns the wrapped lines of `document`'s entries, reusing the last
    /// ones when the key matches.
    fn entry_lines(
        &self,
        session_id: &str,
        document: &SessionDocument,
        content_width: usize,
    ) -> Rc<[Line<'static>]> {
        let mut cached = self.cached.borrow_mut();
        let theme_cache_version = style::active_theme_cache_version();
        if let Some(entries) = cached.as_ref().filter(|entries| {
            entries.session_id == session_id
                && entries.version == document.version
                && entries.content_width == content_width
                && entries.theme_cache_version == theme_cache_version
        }) {
            return Rc::clone(&entries.lines);
        }
        let lines: Rc<[Line<'static>]> = document_lines(document, content_width).into();
        *cached = Some(CachedEntryLines {
            content_width,
            lines: Rc::clone(&lines),
            session_id: session_id.to_string(),
            theme_cache_version,
            version: document.version,
        });

        lines
    }
}

/// One cached set of wrapped entry lines and its key.
struct CachedEntryLines {
    content_width: usize,
    lines: Rc<[Line<'static>]>,
    session_id: String,
    theme_cache_version: u64,
    version: u64,
}

/// Centered popup overlay with one session's notes.
///
/// When the content is taller than the popup, it scrolls and the bottom
/// border shows the scroll keys.
pub(crate) struct SessionDocumentOverlay<'a> {
    cache: &'a SessionDocumentLayoutCache,
    panel: &'a SessionDocumentPanel,
    session_title: &'a str,
}

impl<'a> SessionDocumentOverlay<'a> {
    /// Creates a notes overlay for `panel` of the session titled
    /// `session_title`, wrapping entries through `cache`.
    pub(crate) fn new(
        panel: &'a SessionDocumentPanel,
        session_title: &'a str,
        cache: &'a SessionDocumentLayoutCache,
    ) -> Self {
        Self {
            cache,
            panel,
            session_title,
        }
    }

    /// Returns the bottom-border hint: the confirmation keys while it is
    /// open, else the scroll keys when the content overflows, clearing when
    /// there is something to clear, and closing.
    fn bottom_hint(&self, is_overflowing: bool) -> Line<'static> {
        let mut actions = Vec::new();
        if self.panel.clear_confirmation.is_some() {
            actions.extend(CONFIRMATION_ACTIONS);
        } else {
            if is_overflowing {
                actions.push(SCROLL_ACTION);
            }
            if self.panel.clearable_version().is_some() {
                actions.push(CLEAR_ACTION);
            }
            actions.push(CLOSE_ACTION);
        }
        let mut hint = help_format::footer_line(&actions);
        hint.spans.insert(0, Span::raw(" "));
        hint.spans.push(Span::raw(" "));

        hint
    }
}

impl Component for SessionDocumentOverlay<'_> {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let layout = PopupLayout::new(self.panel, self.cache, area);
        let title = text_util::truncate_with_ellipsis(
            &format!("Session notes · {}", self.session_title),
            layout.content_width.saturating_sub(2),
        );

        overlay::clear_popup_area(frame, layout.popup_area);
        let block = overlay::overlay_block(&title, palette::accent()).title_bottom(
            self.bottom_hint(layout.max_scroll_offset() > 0)
                .right_aligned(),
        );
        let scroll_offset = self.panel.scroll_offset.min(layout.max_scroll_offset());
        let lines = layout.lines();
        let paragraph = Paragraph::new(lines)
            .block(block)
            .scroll((scroll_offset, 0));
        frame.render_widget(paragraph, layout.popup_area);
    }
}

/// Returns the largest scroll offset of `panel` drawn in `area`: how many
/// lines it overflows the popup by, or zero when it fits.
///
/// Key handling bounds scrolling with the same layout the overlay draws.
pub(crate) fn max_scroll_offset(
    panel: &SessionDocumentPanel,
    cache: &SessionDocumentLayoutCache,
    area: Rect,
) -> u16 {
    PopupLayout::new(panel, cache, area).max_scroll_offset()
}

/// Popup size and body lines of the panel for one drawing area: the
/// notice, the cached entries, then the clearing confirmation.
struct PopupLayout {
    content_width: usize,
    entries: Rc<[Line<'static>]>,
    footer: Vec<Line<'static>>,
    header: Vec<Line<'static>>,
    popup_area: Rect,
    required_height: u16,
}

impl PopupLayout {
    /// Fits `panel` into `area`.
    fn new(panel: &SessionDocumentPanel, cache: &SessionDocumentLayoutCache, area: Rect) -> Self {
        let popup_width = (area.width * POPUP_WIDTH_PERCENT / 100)
            .max(MIN_POPUP_WIDTH)
            .min(area.width);
        let content_width = overlay::overlay_content_width(popup_width);
        let mut header = Vec::new();
        if let Some(notice) = &panel.notice {
            header.extend(wrapped(
                notice,
                content_width,
                Style::default().fg(palette::warning()),
            ));
            header.push(Line::default());
        }
        let entries = match &panel.document {
            Ok(document) => cache.entry_lines(panel.restore.session_id(), document, content_width),
            Err(error) => wrapped(
                &format!("Could not load the session notes: {error}"),
                content_width,
                Style::default().fg(palette::danger()),
            )
            .into(),
        };
        let footer = panel
            .clear_confirmation
            .as_ref()
            .map_or_else(Vec::new, |confirmation| {
                let mut footer = vec![Line::default()];
                footer.extend(wrapped(
                    &format!(
                        "Type {CLEAR_CONFIRMATION_WORD}, then Enter, to clear the session notes: \
                         {}▏",
                        confirmation.input.text()
                    ),
                    content_width,
                    Style::default()
                        .fg(palette::warning())
                        .add_modifier(Modifier::BOLD),
                ));

                footer
            });
        let line_count = header.len() + entries.len() + footer.len();
        let required_height = overlay::overlay_required_height(line_count);
        let popup_area = overlay::centered_popup_area(area, 0, 0, popup_width, required_height);

        Self {
            content_width,
            entries,
            footer,
            header,
            popup_area,
            required_height,
        }
    }

    /// Returns every body line, borrowing the cached entries.
    fn lines(&self) -> Vec<Line<'_>> {
        self.header
            .iter()
            .chain(self.entries.iter())
            .chain(&self.footer)
            .map(text_util::borrowed_paint_line)
            .collect()
    }

    /// Returns how many lines the content overflows the popup by.
    fn max_scroll_offset(&self) -> u16 {
        self.required_height.saturating_sub(self.popup_area.height)
    }
}

/// Builds the document's lines for `content_width` columns: its version
/// and size, then every entry in version order.
fn document_lines(document: &SessionDocument, content_width: usize) -> Vec<Line<'static>> {
    let mut lines = wrapped(
        &format!(
            "Version {} · {} of {SESSION_DOCUMENT_BODY_MAX_BYTES} bytes",
            document.version,
            document.body.len()
        ),
        content_width,
        Style::default().fg(palette::text_muted()),
    );
    lines.push(Line::default());
    if document.entries.is_empty() {
        lines.extend(wrapped(
            EMPTY_DOCUMENT_MESSAGE,
            content_width,
            Style::default().fg(palette::text_muted()),
        ));

        return lines;
    }
    for (index, entry) in document.entries.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.extend(entry_lines(entry, content_width));
    }

    lines
}

/// Returns one entry's heading, naming its version, author, and action,
/// and its wrapped text indented by [`NOTE_TEXT_INDENT`] beneath it.
fn entry_lines(entry: &SessionDocumentEntry, content_width: usize) -> Vec<Line<'static>> {
    let heading = format!(
        "v{} · {} · {}",
        entry.version,
        entry.author.label(),
        entry.action_label()
    );
    let mut lines = wrapped(
        &heading,
        content_width,
        Style::default()
            .fg(palette::accent())
            .add_modifier(Modifier::BOLD),
    );
    let indent = " ".repeat(NOTE_TEXT_INDENT.min(content_width / 2));
    lines.extend(
        wrapped(
            &entry.text,
            content_width.saturating_sub(indent.len()),
            Style::default().fg(palette::text()),
        )
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw(indent.clone()));

            line
        }),
    );

    lines
}

/// Wraps each line of `text` to `content_width` columns in `style`,
/// breaking oversized words and repeating a line's leading spaces on each
/// row it wraps to.
fn wrapped(text: &str, content_width: usize, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let content = line.trim_start_matches(' ');
        let indent = " ".repeat((line.len() - content.len()).min(content_width / 2));
        let width = content_width.saturating_sub(indent.len()).max(1);
        lines.extend(
            text_util::wrap_lines_breaking_long_words(content, width)
                .into_iter()
                .map(|row| {
                    let mut spans = vec![Span::styled(indent.clone(), style)];
                    spans.extend(
                        row.spans
                            .into_iter()
                            .map(|span| Span::styled(span.content.into_owned(), style)),
                    );

                    Line::from(spans)
                }),
        );
    }

    lines
}

#[cfg(test)]
#[path = "session_document_overlay_test.rs"]
mod tests;

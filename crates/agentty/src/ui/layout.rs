use ag_tui_text::text_util;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::ui::input_layout;

const APP_FRAME_BAR_HEIGHT: u16 = 1;
const CHAT_INPUT_MIN_PANEL_HEIGHT: u16 = CHAT_INPUT_BORDER_HEIGHT + 1;
const CHAT_INPUT_BORDER_HEIGHT: u16 = 2;
const QUESTION_PANEL_HELP_HEIGHT: u16 = 1;
const QUESTION_PANEL_SPACER_HEIGHT: u16 = 1;
const SESSION_HEADER_HEIGHT_MIN: u16 = 2;
const TAB_PAGE_INSET: u16 = 1;
const SINGLE_LINE_FOOTER_HEIGHT: u16 = 1;

/// Height allocation for question mode's prompt, answer input, and footer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelLayout {
    /// Height reserved for the trailing help footer row.
    pub help_height: u16,
    /// Height reserved for the bordered answer input widget.
    pub input_height: u16,
    /// Height reserved for the question title and wrapped question text.
    pub question_height: u16,
    /// Height reserved for the blank spacer between question and input.
    pub spacer_height: u16,
}

/// Concrete sub-areas used to paint the question-mode bottom panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuestionPanelAreas {
    /// Area used for the footer help text row.
    pub help_area: Rect,
    /// Area used for the bordered answer input widget.
    pub input_area: Rect,
    /// Area used for the predefined answer options.
    pub options_area: Rect,
    /// Area used for the question title and wrapped question text.
    pub question_area: Rect,
    /// Area used for the blank spacer between question text and input.
    pub spacer_area: Rect,
}

/// Top-level frame areas for one rendered session chat page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionChatAreas {
    /// Area used for the footer/help row below the session transcript.
    pub bottom_area: Rect,
    /// Area used for the title and metadata header.
    pub header_area: Rect,
    /// Area used for the bordered transcript/output panel.
    pub output_area: Rect,
}

/// Sub-areas used to render the prompt composer and its footer help row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptPanelAreas {
    /// Area used for the footer/help row below the composer.
    pub footer_area: Rect,
    /// Area used for the bordered prompt input widget.
    pub input_area: Rect,
}

/// Vertical bands of one rendered app frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppFrameAreas {
    /// Area routed to the active page between the two bars.
    pub content_area: Rect,
    /// Area used for the bottom footer bar.
    pub footer_bar_area: Rect,
    /// Area used for the top status bar.
    pub status_bar_area: Rect,
}

/// Content and footer areas used by top-level tab pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabPageAreas {
    /// Area used for the one-line footer below the tab page content.
    pub footer_area: Rect,
    /// Area used for the tab page's primary panels or tables.
    pub main_area: Rect,
}

/// Split an area into a centered content column with side gutters.
pub fn centered_horizontal_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(2),
            Constraint::Percentage(80),
            Constraint::Min(2),
        ])
        .split(area)
}

/// Returns a fixed-size content area centered within the available `area`.
///
/// The returned rectangle is clamped to the bounds of `area` so callers can
/// safely request a preferred content size even when the terminal is smaller.
pub fn centered_content_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let origin_x = area.x + area.width.saturating_sub(width) / 2;
    let origin_y = area.y + area.height.saturating_sub(height) / 2;

    Rect::new(origin_x, origin_y, width, height)
}

/// Splits the terminal frame into status bar, page content, and footer bar.
///
/// Runtime scroll math reuses this split so key handlers derive page geometry
/// from the same area the renderer paints into.
pub fn app_frame_areas(area: Rect) -> AppFrameAreas {
    let vertical_chunks = Layout::default()
        .constraints([
            Constraint::Length(APP_FRAME_BAR_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(APP_FRAME_BAR_HEIGHT),
        ])
        .split(area);

    AppFrameAreas {
        content_area: vertical_chunks[1],
        footer_bar_area: vertical_chunks[2],
        status_bar_area: vertical_chunks[0],
    }
}

/// Splits a tab page area so content starts directly below the tab header.
///
/// The tab header already provides the vertical separation through its bottom
/// border, so tab pages keep only side and bottom insets. This prevents a
/// blank row between the header border and the first table or panel while
/// preserving the one-line footer convention.
pub fn tab_page_areas(area: Rect) -> TabPageAreas {
    let horizontal_inset = TAB_PAGE_INSET.min(area.width / 2);
    let content_x = area.x.saturating_add(horizontal_inset);
    let content_width = area
        .width
        .saturating_sub(horizontal_inset.saturating_mul(2));
    let footer_y = area
        .y
        .saturating_add(area.height.saturating_sub(TAB_PAGE_INSET + 1));
    let main_height = area.height.saturating_sub(TAB_PAGE_INSET + 1);

    TabPageAreas {
        footer_area: Rect::new(content_x, footer_y, content_width, 1.min(area.height)),
        main_area: Rect::new(content_x, area.y, content_width, main_height),
    }
}

/// Splits one session chat page into header, transcript, and bottom panel.
///
/// The outer frame keeps a one-cell margin, reserves `bottom_height` for the
/// prompt/help region, and then dedicates the requested header rows above the
/// bordered session output panel.
pub fn session_chat_areas(area: Rect, bottom_height: u16, header_height: u16) -> SessionChatAreas {
    let header_height = header_height.max(SESSION_HEADER_HEIGHT_MIN);
    let vertical_chunks = Layout::default()
        .constraints([Constraint::Min(0), Constraint::Length(bottom_height)])
        .margin(1)
        .split(area);
    let output_chunks = Layout::default()
        .constraints([Constraint::Length(header_height), Constraint::Min(0)])
        .split(vertical_chunks[0]);

    SessionChatAreas {
        bottom_area: vertical_chunks[1],
        header_area: output_chunks[0],
        output_area: output_chunks[1],
    }
}

/// Splits one prompt bottom panel into input and footer rows.
///
/// Panels that only have one visible row keep the whole area for the input and
/// collapse the footer to height `0`.
pub fn prompt_panel_areas(area: Rect) -> PromptPanelAreas {
    if area.height <= SINGLE_LINE_FOOTER_HEIGHT {
        return PromptPanelAreas {
            footer_area: Rect::new(area.x, area.y.saturating_add(area.height), area.width, 0),
            input_area: area,
        };
    }

    let sections = Layout::default()
        .constraints([
            Constraint::Min(0),
            Constraint::Length(SINGLE_LINE_FOOTER_HEIGHT),
        ])
        .split(area);

    PromptPanelAreas {
        footer_area: sections[1],
        input_area: sections[0],
    }
}

/// Returns the lookup row capacity above the question input, excluding borders.
/// Returns zero when no complete item row fits.
pub(crate) fn question_at_mention_max_visible(
    bottom_area: Rect,
    input_area: Rect,
    default_max_visible: usize,
) -> usize {
    usize::from(input_area.y.saturating_sub(bottom_area.y))
        .saturating_sub(2)
        .min(default_max_visible)
}

/// Calculates the height split for question mode's prompt, input, and footer.
///
/// The answer input keeps its visible content row whenever `available_height`
/// allows it. When space is tight, the wrapped question text yields height
/// before the spacer row and bordered input collapse.
pub fn question_panel_layout(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    max_input_panel_height: u16,
) -> QuestionPanelLayout {
    let question_text_height = wrapped_text_height(question, width);
    // +1 for the "Question N/M" title line when there is question text.
    let requested_question_height = if question_text_height > 0 {
        question_text_height.saturating_add(1)
    } else {
        0
    };
    let requested_input_height = input_layout::calculate_input_height(width, input)
        .min(max_input_panel_height.max(CHAT_INPUT_MIN_PANEL_HEIGHT));
    let requested_spacer_height = if requested_question_height > 0 {
        QUESTION_PANEL_SPACER_HEIGHT
    } else {
        0
    };
    let preferred_total_height = requested_question_height
        .saturating_add(requested_input_height)
        .saturating_add(requested_spacer_height)
        .saturating_add(QUESTION_PANEL_HELP_HEIGHT);
    let total_height = preferred_total_height.min(available_height);
    let help_height = total_height.min(QUESTION_PANEL_HELP_HEIGHT);
    let remaining_height = total_height.saturating_sub(help_height);
    let input_height = remaining_height.min(requested_input_height);
    let question_and_spacer_height = remaining_height.saturating_sub(input_height);
    let spacer_height =
        if question_and_spacer_height >= requested_question_height + requested_spacer_height {
            requested_spacer_height
        } else {
            0
        };
    let question_height = question_and_spacer_height.saturating_sub(spacer_height);

    QuestionPanelLayout {
        help_height,
        input_height,
        question_height,
        spacer_height,
    }
}

/// Returns the total height reserved for a question-mode bottom panel.
///
/// The caller supplies the maximum panel height budget; this helper accounts
/// for question text, predefined options, the answer input, spacer, and help
/// footer using the same allocation logic used during rendering.
pub fn question_panel_reserved_height(
    width: u16,
    available_height: u16,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> u16 {
    let options_height = question_options_height(option_count, available_height);
    let panel_layout = question_panel_layout(
        width,
        available_height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );

    panel_layout
        .question_height
        .saturating_add(options_height)
        .saturating_add(panel_layout.spacer_height)
        .saturating_add(panel_layout.input_height)
        .saturating_add(panel_layout.help_height)
}

/// Splits an already reserved question-mode bottom area into paintable rows.
///
/// The returned rectangles mirror [`question_panel_reserved_height`] so render
/// code can consume precomputed geometry instead of repeating the area math.
pub fn question_panel_areas(
    area: Rect,
    question: &str,
    input: &str,
    option_count: usize,
    max_input_panel_height: u16,
) -> QuestionPanelAreas {
    let options_height = question_options_height(option_count, area.height);
    let panel_layout = question_panel_layout(
        area.width,
        area.height.saturating_sub(options_height),
        question,
        input,
        max_input_panel_height,
    );
    let chunks = Layout::default()
        .constraints([
            Constraint::Length(panel_layout.question_height),
            Constraint::Length(options_height),
            Constraint::Length(panel_layout.spacer_height),
            Constraint::Length(panel_layout.input_height),
            Constraint::Length(panel_layout.help_height),
        ])
        .split(area);

    QuestionPanelAreas {
        help_area: chunks[4],
        input_area: chunks[3],
        options_area: chunks[1],
        question_area: chunks[0],
        spacer_area: chunks[2],
    }
}

/// Returns the total height for a question-mode options section.
///
/// The section contains one header row plus one row per predefined option and
/// clamps to `max_height` so callers can preserve space for surrounding UI.
pub fn question_options_height(option_count: usize, max_height: u16) -> u16 {
    if option_count == 0 {
        return 0;
    }

    u16::try_from(option_count)
        .unwrap_or(u16::MAX)
        .saturating_add(1)
        .min(max_height)
}

/// Returns the wrapped line count for plain text rendered in a paragraph.
fn wrapped_text_height(text: &str, width: u16) -> u16 {
    let wrapped_line_count = text_util::wrap_lines(text, usize::from(width.max(1))).len();

    u16::try_from(wrapped_line_count).unwrap_or(u16::MAX).max(1)
}

#[cfg(test)]
#[path = "layout_test.rs"]
mod tests;

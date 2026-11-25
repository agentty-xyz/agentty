//! Usage overlay rendering tests: table, notes, footer, narrow widths, and
//! context gauge tones.

use ag_contracts::{CostBasis, UsagePurpose};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{UsageOverlay, context_spans, context_tone, max_scroll_offset};
use crate::domain::session_usage::{ContextNow, SessionUsage, UsageRecord};
use crate::ui::Component;
use crate::ui::style::palette;
use crate::ui::usage_format::{ContextSummary, UsagePanelContent, usage_panel_content};

/// Returns a priced record of `model` under `purpose`.
fn record(model: &str, purpose: UsagePurpose) -> UsageRecord {
    UsageRecord {
        cache_read_tokens: Some(3_000),
        cache_write_tokens: Some(100),
        context_peak_tokens: Some(9_000),
        context_window_tokens: Some(10_000),
        cost_basis: Some(CostBasis::ListPrice),
        cost_micro_usd: Some(240_000),
        detailed_invocation_count: 2,
        input_tokens: 4_000,
        invocation_count: 2,
        model: model.to_string(),
        output_tokens: 500,
        purpose,
        reasoning_tokens: Some(50),
        request_count: 3,
        unpriced_invocation_count: 0,
        unreported_request_count: 0,
    }
}

/// Returns panel content for a turn model, a title model, and an
/// unreported review model.
fn panel_content() -> UsagePanelContent {
    let mut unreported = record("qwen3-coder", UsagePurpose::Review);
    unreported.cache_read_tokens = None;
    unreported.cache_write_tokens = None;
    unreported.context_peak_tokens = None;
    unreported.cost_basis = None;
    unreported.cost_micro_usd = None;
    unreported.input_tokens = 0;
    unreported.output_tokens = 0;
    unreported.reasoning_tokens = None;
    unreported.unpriced_invocation_count = 2;
    unreported.unreported_request_count = 3;

    usage_panel_content(&SessionUsage {
        context: Some(ContextNow {
            tokens: 8_000,
            window_tokens: Some(10_000),
        }),
        records: vec![
            record("claude-opus-5-5", UsagePurpose::Turn),
            record("claude-haiku-5-5", UsagePurpose::Title),
            unreported,
        ],
        turn_count: 2,
    })
}

/// Renders the overlay into an in-memory terminal and returns its rows.
fn render_rows(content: &UsagePanelContent, width: u16, height: u16) -> Vec<String> {
    render_scrolled_rows(content, width, height, 0)
}

/// Renders the overlay scrolled by `scroll_offset` and returns its rows.
fn render_scrolled_rows(
    content: &UsagePanelContent,
    width: u16,
    height: u16,
    scroll_offset: u16,
) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            UsageOverlay::new(content, "Retry sync client")
                .scroll_offset(scroll_offset)
                .render(frame, frame.area());
        })
        .expect("failed to render usage overlay");

    buffer_rows(terminal.backend().buffer())
}

#[test]
fn test_short_overlay_scrolls_to_the_total_and_notes() {
    // Arrange
    let content = panel_content();

    // Act
    let top = render_scrolled_rows(&content, 140, 14, 0);
    let bottom = render_scrolled_rows(&content, 140, 14, u16::MAX);
    let tall = render_rows(&content, 140, 40);

    // Assert
    assert!(contains(&top, "j/k: scroll"));
    assert!(contains(&top, "Est. cost"));
    assert!(!contains(&top, "Costs are estimated"));
    assert!(contains(&bottom, "Costs are estimated"));
    assert!(contains(&bottom, "Session total"));
    assert!(!contains(&tall, "j/k: scroll"));
    assert!(max_scroll_offset(&content, Rect::new(0, 0, 140, 14)) > 0);
    assert_eq!(max_scroll_offset(&content, Rect::new(0, 0, 140, 40)), 0);
}

/// Collects one string per buffer row.
fn buffer_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Returns whether any row contains `text`.
fn contains(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

#[test]
fn test_wide_overlay_shows_every_column_rows_totals_notes_and_footer() {
    // Arrange
    let content = panel_content();

    // Act
    let rows = render_rows(&content, 140, 40);

    // Assert
    assert!(contains(&rows, "Usage · Retry sync client"));
    for header in [
        "Model",
        "Req",
        "Input",
        "Cached",
        "Cache wr",
        "Output",
        "Reason",
        "Ctx peak",
        "Est. cost",
    ] {
        assert!(contains(&rows, header), "missing header {header}");
    }
    assert!(contains(&rows, "claude-haiku-5-5 title"));
    assert!(contains(&rows, "qwen3-coder review"));
    assert!(contains(&rows, "unknown"));
    assert!(contains(&rows, "Session total"));
    assert!(contains(&rows, "≥$0.48"));
    assert!(contains(&rows, "▰▰▰▰▱ 8.0k / 10.0k now (80%)"));
    assert!(contains(&rows, "≥ Lower bound: 3 requests (qwen3-coder)"));
    assert!(contains(&rows, "$ Costs are estimated"));
    assert!(contains(&rows, "Esc: close"));
}

#[test]
fn test_narrow_overlay_drops_low_priority_columns_and_keeps_cost() {
    // Arrange
    let content = panel_content();

    // Act
    let rows = render_rows(&content, 56, 40);

    // Assert
    assert!(!contains(&rows, "Cache wr"));
    assert!(!contains(&rows, "Reason"));
    assert!(contains(&rows, "Est. cost"));
    assert!(contains(&rows, "Session total"));
    assert!(rows.iter().all(|row| row.chars().count() == 56));
}

#[test]
fn test_tiny_overlay_truncates_the_model_column() {
    // Arrange
    let content = panel_content();

    // Act
    let rows = render_rows(&content, 24, 40);

    // Assert
    assert!(contains(&rows, "..."));
    assert!(contains(&rows, "Est. cost"));
}

#[test]
fn test_empty_overlay_explains_that_no_usage_is_recorded() {
    // Arrange
    let content = usage_panel_content(&SessionUsage::default());

    // Act
    let rows = render_rows(&content, 100, 20);

    // Assert
    assert!(contains(&rows, "No usage recorded for this session yet."));
    assert!(!contains(&rows, "Session total"));
}

#[test]
fn test_context_tone_rises_with_fill() {
    // Arrange, Act, Assert
    assert_eq!(context_tone(None), palette::text_muted());
    assert_eq!(context_tone(Some(74)), palette::text_muted());
    assert_eq!(context_tone(Some(75)), palette::warning());
    assert_eq!(context_tone(Some(90)), palette::danger());
}

#[test]
fn test_context_spans_show_the_latest_fill_alone_without_a_peak() {
    // Arrange
    let context = ContextSummary {
        gauge_filled: Some(1),
        now: Some("1.0k / 10.0k now (10%)".to_string()),
        now_percent: Some(10),
        peak: None,
    };

    // Act
    let text = context_spans(&context)
        .iter()
        .map(|span| span.content.to_string())
        .collect::<String>();

    // Assert
    assert_eq!(text, "Context    ▰▱▱▱▱ 1.0k / 10.0k now (10%)");
}

#[test]
fn test_context_spans_show_peak_alone_without_a_latest_fill() {
    // Arrange
    let context = ContextSummary {
        gauge_filled: None,
        now: None,
        now_percent: None,
        peak: Some("71.2k".to_string()),
    };

    // Act
    let text = context_spans(&context)
        .iter()
        .map(|span| span.content.to_string())
        .collect::<String>();

    // Assert
    assert_eq!(text, "Context    peak 71.2k");
}

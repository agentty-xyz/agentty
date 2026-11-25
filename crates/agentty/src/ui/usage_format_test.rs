//! Usage formatting tests: money edges, column dropping, row cells and
//! marks, totals, summary figures, and notes.

use std::collections::HashMap;

use ag_contracts::{
    ContextFill, CostBasis, CostEstimate, ModelUsage, PlanLogin, TurnUsage, UsagePurpose,
};

use super::{
    CostFigure, ReadoutContext, UsageColumn, UsageFigure, UsageNote, UsageTableRow, context_gauge,
    context_tone, format_money, format_percent, login_label, purpose_label,
    session_readout_figures, usage_panel_content, visible_usage_columns,
};
use crate::domain::session::SessionId;
use crate::domain::session_usage::{
    ContextNow, LiveUsageKey, LiveUsageRun, SessionUsage, UsageRecord,
};
use crate::ui::style::palette;

/// Returns a fully detailed, priced record of `model` under `purpose`.
fn priced_record(model: &str, purpose: UsagePurpose) -> UsageRecord {
    UsageRecord {
        api_equivalent_login: None,
        api_equivalent_micro_usd: None,
        api_equivalent_upper_bound: false,
        cache_read_tokens: Some(3_790_000),
        cache_write_tokens: Some(212_000),
        context_peak_tokens: Some(612_400),
        context_window_tokens: Some(1_000_000),
        cost_basis: Some(CostBasis::ListPrice),
        cost_micro_usd: Some(3_270_000),
        cost_upper_bound: false,
        detailed_invocation_count: 7,
        input_tokens: 4_120_000,
        invocation_count: 7,
        model: model.to_string(),
        output_tokens: 48_900,
        purpose,
        reasoning_tokens: Some(11_200),
        request_count: 34,
        unpriced_invocation_count: 0,
        unreported_request_count: 0,
    }
}

/// Returns a record whose requests all reported nothing and had no price.
fn unreported_record(model: &str) -> UsageRecord {
    UsageRecord {
        api_equivalent_login: None,
        api_equivalent_micro_usd: None,
        api_equivalent_upper_bound: false,
        cache_read_tokens: None,
        cache_write_tokens: None,
        context_peak_tokens: None,
        context_window_tokens: None,
        cost_basis: None,
        cost_micro_usd: None,
        cost_upper_bound: false,
        detailed_invocation_count: 1,
        input_tokens: 0,
        invocation_count: 1,
        model: model.to_string(),
        output_tokens: 0,
        purpose: UsagePurpose::Review,
        reasoning_tokens: None,
        request_count: 2,
        unpriced_invocation_count: 1,
        unreported_request_count: 2,
    }
}

/// Returns a record written before detailed usage existed.
fn legacy_record(model: &str) -> UsageRecord {
    UsageRecord {
        api_equivalent_login: None,
        api_equivalent_micro_usd: None,
        api_equivalent_upper_bound: false,
        cache_read_tokens: None,
        cache_write_tokens: None,
        context_peak_tokens: None,
        context_window_tokens: None,
        cost_basis: None,
        cost_micro_usd: None,
        cost_upper_bound: false,
        detailed_invocation_count: 0,
        input_tokens: 1_000,
        invocation_count: 3,
        model: model.to_string(),
        output_tokens: 200,
        purpose: UsagePurpose::Turn,
        reasoning_tokens: None,
        request_count: 0,
        unpriced_invocation_count: 0,
        unreported_request_count: 0,
    }
}

/// Returns usage of `records` with a known context and two turns.
fn session_usage(records: Vec<UsageRecord>) -> SessionUsage {
    SessionUsage {
        context: Some(ContextNow {
            tokens: 384_200,
            window_tokens: Some(1_000_000),
        }),
        records,
        turn_count: 2,
    }
}

/// Returns the marks of `notes` in order.
fn note_marks(notes: &[UsageNote]) -> Vec<&'static str> {
    notes.iter().map(|note| note.mark).collect()
}

/// Returns how the panel writes each cell of `row`.
fn texts(row: &UsageTableRow) -> [String; UsageColumn::COUNT] {
    row.cells.each_ref().map(UsageFigure::panel_text)
}

#[test]
fn test_lower_bound_sub_cent_cost_reads_as_counted_with_one_mark() {
    // Arrange
    let mut record = priced_record("model", UsagePurpose::Turn);
    record.cost_micro_usd = Some(1_200);
    record.unpriced_invocation_count = 1;
    let mut exact = record.clone();
    exact.unpriced_invocation_count = 0;

    // Act
    let lower_bound = usage_panel_content(&session_usage(vec![record]));
    let exact_content = usage_panel_content(&session_usage(vec![exact]));

    // Assert
    assert_eq!(
        lower_bound.rows[0].cell(UsageColumn::Cost).panel_text(),
        "≥$0.00"
    );
    assert_eq!(lower_bound.summary.cost.panel_text(), "≥$0.00");
    assert!(lower_bound.summary.cost.is_lower_bound());
    assert_eq!(exact_content.summary.cost.panel_text(), "<$0.01");
}

#[test]
fn test_format_money_covers_rounding_and_magnitude_edges() {
    // Arrange
    let cases = [
        (0, "$0.00"),
        (1, "<$0.01"),
        (4_999, "<$0.01"),
        (5_000, "$0.01"),
        (3_274_999, "$3.27"),
        (3_275_000, "$3.28"),
        (999_994_999, "$999.99"),
        (999_995_000, "$1,000"),
        (1_234_567_890, "$1,230"),
        (12_345_000_000, "$12,300"),
        (123_456_000_000, "$123,000"),
        (1_235_000_000_000, "$1,240,000"),
    ];

    // Act
    let formatted = cases.map(|(micro_usd, _)| format_money(micro_usd));

    // Assert
    assert_eq!(formatted, cases.map(|(_, expected)| expected.to_string()));
}

#[test]
fn test_format_percent_rounds_and_tolerates_an_empty_whole() {
    // Arrange, Act, Assert
    assert_eq!(format_percent(612_400, 1_000_000), "61%");
    assert_eq!(format_percent(5, 1_000), "1%");
    assert_eq!(format_percent(1, 0), "0%");
}

#[test]
fn test_visible_usage_columns_drop_by_priority_and_keep_cost() {
    // Arrange
    let widths = [4, 7, 7, 8, 7, 7, 8, 9];

    // Act
    let all = visible_usage_columns(&widths, 14, 2, 200);
    let narrow = visible_usage_columns(&widths, 14, 2, 50);
    let tiny = visible_usage_columns(&widths, 14, 2, 10);

    // Assert
    assert_eq!(all, UsageColumn::ALL);
    assert_eq!(
        narrow,
        [
            UsageColumn::Requests,
            UsageColumn::Input,
            UsageColumn::Output,
            UsageColumn::Cost,
        ]
    );
    assert_eq!(tiny, [UsageColumn::Cost]);
}

#[test]
fn test_column_headers_and_indices_follow_display_order() {
    // Arrange, Act
    let headers = UsageColumn::ALL.map(UsageColumn::header);
    let indices = UsageColumn::ALL.map(UsageColumn::index);

    // Assert
    assert_eq!(
        headers,
        [
            "Req",
            "Input",
            "Cached",
            "Cache wr",
            "Output",
            "Reason",
            "Ctx peak",
            "Est. cost"
        ]
    );
    assert_eq!(indices, [0, 1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn test_purpose_labels_hide_session_turns() {
    // Arrange, Act
    let labels = [
        UsagePurpose::Turn,
        UsagePurpose::Title,
        UsagePurpose::CommitAssist,
        UsagePurpose::Review,
        UsagePurpose::Rebase,
        UsagePurpose::Other,
    ]
    .map(purpose_label);

    // Assert
    assert_eq!(
        labels,
        [
            None,
            Some("title"),
            Some("commit"),
            Some("review"),
            Some("rebase"),
            Some("other"),
        ]
    );
}

#[test]
fn test_priced_session_shows_exact_figures_and_only_the_estimate_note() {
    // Arrange
    let usage = session_usage(vec![priced_record("claude-opus-5-5", UsagePurpose::Turn)]);

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(
        texts(&content.rows[0]),
        [
            "34", "4.1M", "3.8M", "212.0k", "48.9k", "11.2k", "61%", "$3.27"
        ]
        .map(str::to_string)
    );
    assert_eq!(content.total.model, "Session total");
    assert_eq!(texts(&content.total), texts(&content.rows[0]));
    assert_eq!(content.summary.cost.panel_text(), "$3.27");
    assert_eq!(content.summary.cost_explanation, "from list prices");
    assert_eq!(content.summary.tokens.panel_text(), "4.2M");
    assert_eq!(content.summary.turns, "2");
    assert_eq!(content.summary.requests.panel_text(), "34");
    assert_eq!(
        content.summary.cache.as_deref(),
        Some("92% of input served from cache")
    );
    let fill = content.summary.context.expect("context summary");
    assert_eq!(fill.gauge_filled, Some(2));
    assert_eq!(fill.now.as_deref(), Some("384.2k / 1.0M now (38%)"));
    assert_eq!(fill.now_percent, Some(38));
    assert_eq!(fill.peak.as_deref(), Some("612.4k / 1.0M (61%)"));
    assert_eq!(
        content.notes,
        [UsageNote {
            is_estimate: true,
            mark: "$",
            text: "Costs are estimated from built-in list prices; the provider bill is \
                   authoritative."
                .to_string(),
        }]
    );
}

#[test]
fn test_unreported_and_unpriced_model_marks_lower_bounds_and_unknowns() {
    // Arrange
    let usage = session_usage(vec![
        priced_record("claude-opus-5-5", UsagePurpose::Turn),
        unreported_record("qwen3-coder"),
    ]);

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(content.rows[1].purpose, Some("review"));
    assert_eq!(
        texts(&content.rows[1]),
        ["2", "?", "?", "?", "?", "?", "?", "unknown"].map(str::to_string)
    );
    assert_eq!(
        texts(&content.total),
        [
            "36",
            "≥4.1M",
            "≥3.8M",
            "≥212.0k",
            "≥48.9k",
            "≥11.2k",
            "61%",
            "≥$3.27"
        ]
        .map(str::to_string)
    );
    assert_eq!(content.summary.cost_explanation, "lower bound");
    assert_eq!(note_marks(&content.notes), ["≥", "?", "unknown", "$"]);
    assert_eq!(
        content.notes[0].text,
        "Lower bound: 2 requests (qwen3-coder) reported no token counts; the cost leaves out \
         unpriced runs (qwen3-coder); some models do not report every column."
    );
    assert_eq!(
        content.notes[1].text,
        "Unreported: qwen3-coder reported no token counts."
    );
    assert_eq!(
        content.notes[2].text,
        "Unpriced: no price for qwen3-coder, which the total leaves out."
    );
    assert_eq!(
        content
            .notes
            .iter()
            .map(|note| note.is_estimate)
            .collect::<Vec<_>>(),
        [false, false, false, true]
    );
}

#[test]
fn test_legacy_rows_read_not_recorded_and_lower_the_totals() {
    // Arrange
    let usage = SessionUsage {
        context: None,
        records: vec![legacy_record("claude-opus-4-1")],
        turn_count: 3,
    };

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(
        texts(&content.rows[0]),
        ["—", "≥1.0k", "—", "—", "200", "—", "—", "—"].map(str::to_string)
    );
    assert_eq!(
        texts(&content.total),
        ["—", "≥1.0k", "—", "—", "200", "—", "—", "—"].map(str::to_string)
    );
    assert_eq!(content.summary.cost_explanation, "not recorded");
    assert_eq!(content.summary.tokens.panel_text(), "≥1.2k");
    assert_eq!(content.summary.context, None);
    assert_eq!(content.summary.cache, None);
    assert_eq!(note_marks(&content.notes), ["≥", "—"]);
    assert_eq!(
        content.notes[0].text,
        "Lower bound: 3 runs (claude-opus-4-1) were recorded before detailed usage, so input may \
         leave out cache reads."
    );
    assert_eq!(
        content.notes[1].text,
        "Not recorded: runs recorded before detailed usage."
    );
}

#[test]
fn test_partly_detailed_row_marks_requests_and_cost_as_lower_bounds() {
    // Arrange
    let mut record = priced_record("model", UsagePurpose::Turn);
    record.invocation_count = 8;
    record.cache_read_tokens = None;
    record.cache_write_tokens = None;
    record.reasoning_tokens = None;
    record.context_window_tokens = None;

    // Act
    let content = usage_panel_content(&session_usage(vec![record]));

    // Assert
    assert_eq!(
        texts(&content.rows[0]),
        ["≥34", "≥4.1M", "—", "—", "48.9k", "—", "612.4k", "≥$3.27"].map(str::to_string)
    );
    assert_eq!(
        content.notes[1].text,
        "Not recorded: the agent backend does not report this figure; runs recorded before \
         detailed usage."
    );
    assert_eq!(content.summary.cache, None);
}

#[test]
fn test_unpriced_session_reads_unknown_without_an_estimate_note() {
    // Arrange
    let mut record = priced_record("local-model", UsagePurpose::Turn);
    record.cost_basis = None;
    record.cost_micro_usd = None;
    record.unpriced_invocation_count = 7;

    // Act
    let content = usage_panel_content(&session_usage(vec![record]));

    // Assert
    assert_eq!(
        content.total.cell(UsageColumn::Cost).panel_text(),
        "unknown"
    );
    assert_eq!(
        content.summary.cost_explanation,
        "no price for these models"
    );
    assert_eq!(note_marks(&content.notes), ["unknown"]);
    assert_eq!(content.notes[0].text, "Unpriced: no price for local-model.");
}

#[test]
fn test_mixed_and_single_bases_name_their_sources() {
    // Arrange
    let mut agent = priced_record("alpha", UsagePurpose::Turn);
    agent.cost_basis = Some(CostBasis::AgentReported);
    let mut host = priced_record("beta", UsagePurpose::Turn);
    host.cost_basis = Some(CostBasis::HostPrice);

    // Act
    let agent_only = usage_panel_content(&session_usage(vec![agent.clone()]));
    let host_only = usage_panel_content(&session_usage(vec![host.clone()]));
    let mixed = usage_panel_content(&session_usage(vec![agent, host]));

    // Assert
    assert_eq!(agent_only.summary.cost_explanation, "as the agent reported");
    assert!(agent_only.notes[0].text.contains("by the agent itself"));
    assert_eq!(host_only.summary.cost_explanation, "from configured prices");
    assert!(host_only.notes[0].text.contains("from configured prices"));
    assert_eq!(mixed.summary.cost_explanation, "from several price sources");
    assert!(mixed.notes[0].text.contains("from several price sources"));
    assert_eq!(mixed.total.cell(UsageColumn::Cost).panel_text(), "$6.54");
}

#[test]
fn test_context_without_window_shows_absolute_figures() {
    // Arrange
    let mut record = priced_record("model", UsagePurpose::Turn);
    record.context_window_tokens = None;
    let usage = SessionUsage {
        context: Some(ContextNow {
            tokens: 71_200,
            window_tokens: None,
        }),
        records: vec![record],
        turn_count: 1,
    };

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    let fill = content.summary.context.expect("context summary");
    assert_eq!(fill.gauge_filled, None);
    assert_eq!(fill.now.as_deref(), Some("71.2k now"));
    assert_eq!(fill.now_percent, None);
    assert_eq!(fill.peak.as_deref(), Some("612.4k"));
    assert_eq!(
        content.total.cell(UsageColumn::ContextPeak).panel_text(),
        "612.4k"
    );
}

#[test]
fn test_context_peak_prefers_the_fullest_window_and_partial_cache_scope() {
    // Arrange
    let mut large = priced_record("large", UsagePurpose::Turn);
    large.context_peak_tokens = Some(500_000);
    large.context_window_tokens = Some(10_000_000);
    let mut small = priced_record("small", UsagePurpose::Turn);
    small.context_peak_tokens = Some(90_000);
    small.context_window_tokens = Some(100_000);
    let mut uncached = priced_record("uncached", UsagePurpose::Turn);
    uncached.cache_read_tokens = None;
    let usage = SessionUsage {
        context: None,
        records: vec![large, small, uncached],
        turn_count: 0,
    };

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(
        content.total.cell(UsageColumn::ContextPeak).panel_text(),
        "90%"
    );
    assert_eq!(
        content
            .summary
            .context
            .expect("context summary")
            .peak
            .as_deref(),
        Some("90.0k / 100.0k (90%)")
    );
    assert_eq!(
        content.summary.cache.as_deref(),
        Some("92% of input served from cache (models that report it)")
    );
}

#[test]
fn test_all_unreported_session_reads_unknown_everywhere() {
    // Arrange
    let usage = SessionUsage {
        context: None,
        records: vec![unreported_record("qwen3-coder")],
        turn_count: 1,
    };

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(
        texts(&content.total),
        ["2", "?", "?", "?", "?", "?", "?", "unknown"].map(str::to_string)
    );
    assert_eq!(content.summary.tokens.panel_text(), "?");
    assert_eq!(note_marks(&content.notes), ["?", "unknown"]);
}

#[test]
fn test_empty_session_shows_no_figures_or_notes() {
    // Arrange
    let usage = SessionUsage::default();

    // Act
    let content = usage_panel_content(&usage);

    // Assert
    assert_eq!(content.rows, []);
    assert_eq!(
        texts(&content.total),
        ["—", "0", "—", "—", "0", "—", "—", "—"].map(str::to_string)
    );
    assert_eq!(content.summary.tokens.panel_text(), "0");
    assert_eq!(content.notes, []);
}

#[test]
fn test_single_unreported_request_and_run_use_singular_nouns() {
    // Arrange
    let mut partly = priced_record("alpha", UsagePurpose::Turn);
    partly.unreported_request_count = 1;
    partly.invocation_count = 8;

    // Act
    let content = usage_panel_content(&session_usage(vec![partly]));

    // Assert
    assert_eq!(
        content.notes[0].text,
        "Lower bound: 1 request (alpha) reported no token counts; 1 run (alpha) was recorded \
         before detailed usage, so input may leave out cache reads."
    );
    assert_eq!(
        content.rows[0].cell(UsageColumn::Cached).panel_text(),
        "≥3.8M"
    );
}

/// Returns a record of `model` whose usage `login` billed, priced only as
/// an API equivalent.
fn api_equivalent_record(model: &str, login: PlanLogin) -> UsageRecord {
    UsageRecord {
        api_equivalent_login: Some(login),
        api_equivalent_micro_usd: Some(3_270_000),
        cost_basis: None,
        cost_micro_usd: None,
        ..priced_record(model, UsagePurpose::Turn)
    }
}

#[test]
fn test_plan_login_costs_read_as_api_equivalents_named_in_the_title() {
    // Arrange
    let plan = UsageRecord {
        api_equivalent_upper_bound: true,
        ..api_equivalent_record("chatgpt/gpt-6.1-sol", PlanLogin::ChatGpt)
    };

    // Act
    let content = usage_panel_content(&session_usage(vec![plan]));

    // Assert
    assert_eq!(
        content.rows[0].cell(UsageColumn::Cost).panel_text(),
        "≈$3.27"
    );
    assert_eq!(content.total.cell(UsageColumn::Cost).panel_text(), "≈$3.27");
    assert_eq!(content.summary.cost.panel_text(), "≈$3.27");
    assert_eq!(
        content.summary.cost_explanation,
        "API-equivalent, plan-billed"
    );
    assert_eq!(content.logins, [PlanLogin::ChatGpt]);
    assert_eq!(
        [PlanLogin::ChatGpt, PlanLogin::Claude].map(login_label),
        ["ChatGPT subscription", "Claude subscription"]
    );
    assert_eq!(
        content.notes,
        [
            UsageNote {
                is_estimate: false,
                mark: "≈",
                text: "API-equivalent: ChatGPT plan login; priced at OpenAI API list prices; 92% \
                       cached input priced as uncached, with no cache price; a subscription bills \
                       no per-token charge."
                    .to_string(),
            },
            UsageNote {
                is_estimate: true,
                mark: "$",
                text: "Costs are estimated at API list prices for plan-billed usage; the provider \
                       bill is authoritative."
                    .to_string(),
            },
        ]
    );
}

#[test]
fn test_cached_input_without_a_cache_price_marks_upper_and_both_bounds() {
    // Arrange
    let overpriced = UsageRecord {
        cost_upper_bound: true,
        ..priced_record("muse-spark-1.3", UsagePurpose::Turn)
    };

    // Act
    let upper = usage_panel_content(&session_usage(vec![overpriced.clone()]));
    let both = usage_panel_content(&session_usage(vec![
        overpriced,
        unreported_record("qwen3-coder"),
    ]));

    // Assert
    assert_eq!(upper.rows[0].cell(UsageColumn::Cost).panel_text(), "≤$3.27");
    assert_eq!(upper.summary.cost.panel_text(), "≤$3.27");
    assert_eq!(upper.summary.cost_explanation, "upper bound");
    assert_eq!(upper.logins, []);
    assert_eq!(note_marks(&upper.notes), ["≤", "$"]);
    assert_eq!(
        upper.notes[0].text,
        "Upper bound: 92% cached, no cache price for muse-spark-1.3."
    );
    assert_eq!(both.summary.cost.panel_text(), "~$3.27");
    assert_eq!(both.summary.cost_explanation, "lower and upper bound");
    assert_eq!(note_marks(&both.notes), ["≥", "≤", "?", "unknown", "$"]);
}

#[test]
fn test_api_equivalents_of_several_logins_name_no_single_login() {
    // Arrange
    let several_logins = UsageRecord {
        api_equivalent_login: None,
        ..api_equivalent_record("gpt-5.3-codex", PlanLogin::ChatGpt)
    };

    // Act
    let content = usage_panel_content(&session_usage(vec![several_logins]));

    // Assert
    assert_eq!(content.logins, []);
    assert_eq!(content.summary.cost.panel_text(), "≈$3.27");
    assert_eq!(
        content.notes[0].text,
        "API-equivalent: usage of several subscription logins, priced at their API list prices; a \
         subscription bills no per-token charge."
    );
}

#[test]
fn test_api_equivalents_are_never_summed_into_charged_estimates() {
    // Arrange
    let claude_plan = api_equivalent_record("claude-opus-5-5", PlanLogin::Claude);
    let charged = priced_record("kimi-k3", UsagePurpose::Turn);
    let partly_priced = UsageRecord {
        unpriced_invocation_count: 1,
        ..api_equivalent_record("claude-sonnet-5-5", PlanLogin::Claude)
    };
    // One model both charged through an API key and plan-billed.
    let both_in_one_row = UsageRecord {
        api_equivalent_login: Some(PlanLogin::Claude),
        api_equivalent_micro_usd: Some(1_000_000),
        ..priced_record("claude-haiku-5-5", UsagePurpose::Turn)
    };

    // Act
    let side_by_side =
        usage_panel_content(&session_usage(vec![claude_plan.clone(), charged.clone()]));
    let lower_side_by_side = usage_panel_content(&session_usage(vec![
        claude_plan,
        charged,
        unreported_record("qwen3-coder"),
    ]));
    let lower_equivalent = usage_panel_content(&session_usage(vec![partly_priced]));
    let one_row = usage_panel_content(&session_usage(vec![both_in_one_row]));

    // Assert
    assert_eq!(side_by_side.summary.cost.panel_text(), "$3.27 + ≈$3.27");
    assert_eq!(
        side_by_side.summary.cost_explanation,
        "estimated, plus API-equivalent, plan-billed"
    );
    assert_eq!(side_by_side.logins, [PlanLogin::Claude]);
    assert_eq!(
        side_by_side.notes[0].text,
        "API-equivalent: Claude subscription login; priced from Claude Code's estimate or \
         Anthropic API list prices; a subscription bills no per-token charge."
    );
    assert!(
        side_by_side.notes[1]
            .text
            .contains("from built-in list prices")
    );
    assert_eq!(
        one_row.rows[0].cell(UsageColumn::Cost).panel_text(),
        "$3.27 + ≈$1.00"
    );
    assert_eq!(one_row.summary.cost.panel_text(), "$3.27 + ≈$1.00");
    assert_eq!(one_row.logins, [PlanLogin::Claude]);
    assert_eq!(
        lower_side_by_side.summary.cost.panel_text(),
        "≥$3.27 + ≥≈$3.27",
        "unpriced usage could be either, so neither sum is complete"
    );
    assert_eq!(
        lower_equivalent.rows[0]
            .cell(UsageColumn::Cost)
            .panel_text(),
        "≥≈$3.27"
    );
    assert_eq!(lower_equivalent.summary.cost.panel_text(), "≥≈$3.27");
    assert_eq!(note_marks(&lower_equivalent.notes), ["≥", "≈", "$"]);
}

/// Returns one priced request of `model` with `input_tokens` of input, a
/// million-token window, and an estimate of `micro_usd`.
fn live_request(model: &str, input_tokens: u64, micro_usd: u64) -> ModelUsage {
    ModelUsage {
        cache_read_tokens: Some(input_tokens / 2),
        cache_write_tokens: Some(0),
        context: Some(ContextFill::of_request(input_tokens, Some(1_000_000))),
        cost: Some(CostEstimate::new(CostBasis::ListPrice, micro_usd)),
        input_tokens,
        output_tokens: 3_100,
        reasoning_tokens: Some(0),
        requests: 1,
        ..ModelUsage::new(model)
    }
}

/// Returns live usage of `run` in `session_id` made of `models`.
fn live_entry(
    session_id: &str,
    run: LiveUsageRun,
    models: Vec<ModelUsage>,
) -> (LiveUsageKey, TurnUsage) {
    (
        LiveUsageKey {
            run,
            session_id: SessionId::from(session_id),
        },
        TurnUsage {
            models,
            ..TurnUsage::default()
        },
    )
}

#[test]
fn test_readout_figures_are_absent_until_the_session_records_usage() {
    // Arrange
    let live = HashMap::from([live_entry(
        "other",
        LiveUsageRun::SessionTurn,
        vec![live_request("model", 10, 10)],
    )]);

    // Act
    let figures = session_readout_figures(None, &SessionId::from("session"), &live);

    // Assert
    assert_eq!(figures, None);
}

#[test]
fn test_idle_readout_figures_restore_stored_context_and_session_cost() {
    // Arrange
    let stored = session_usage(vec![priced_record("model", UsagePurpose::Turn)]);

    // Act
    let figures =
        session_readout_figures(Some(&stored), &SessionId::from("session"), &HashMap::new())
            .expect("stored usage has readout figures");

    // Assert
    assert_eq!(figures.turn, None);
    assert_eq!(
        figures.context,
        Some(ReadoutContext {
            gauge_filled: Some(2),
            percent: Some(38),
            tokens: "384.2k".to_string(),
        })
    );
    assert_eq!(figures.session_cost.panel_text(), "$3.27");
}

#[test]
fn test_running_readout_figures_show_the_session_turn_and_fold_every_run_into_the_session() {
    // Arrange
    let stored = session_usage(vec![priced_record("model", UsagePurpose::Turn)]);
    let live = HashMap::from([
        live_entry(
            "session",
            LiveUsageRun::SessionTurn,
            vec![live_request("model", 412_000, 240_000)],
        ),
        live_entry(
            "session",
            LiveUsageRun::Attempt("attempt-1".to_string()),
            vec![live_request("model", 100, 1_000_000)],
        ),
        live_entry(
            "other",
            LiveUsageRun::SessionTurn,
            vec![live_request("model", 900_000, 9_000_000)],
        ),
    ]);

    // Act
    let figures = session_readout_figures(Some(&stored), &SessionId::from("session"), &live)
        .expect("running session has readout figures");

    // Assert
    let turn = figures.turn.expect("session turn reported a request");
    assert_eq!(turn.input.panel_text(), "412.0k");
    assert_eq!(turn.output.panel_text(), "3.1k");
    assert_eq!(turn.cost.panel_text(), "$0.24");
    assert_eq!(figures.session_cost.panel_text(), "$4.51");
    assert_eq!(
        figures.context.map(|context| context.percent),
        Some(Some(41)),
        "the session turn's fill wins over the attempt's"
    );
}

#[test]
fn test_readout_turn_figures_cover_money_edges_and_unreported_requests() {
    // Arrange
    let session_id = SessionId::from("session");
    let turn_cost = |models: Vec<ModelUsage>| {
        let live = HashMap::from([live_entry("session", LiveUsageRun::SessionTurn, models)]);
        let figures = session_readout_figures(None, &session_id, &live)
            .expect("live usage has readout figures");

        figures.turn.expect("session turn reported a request")
    };
    let mut lower_bound_request = live_request("model", 1_000, 4_000);
    lower_bound_request.unreported_requests = 1;

    // Act
    let sub_cent = turn_cost(vec![live_request("model", 1_000, 4_000)]);
    let lower_bound_sub_cent = turn_cost(vec![lower_bound_request]);
    let thousands = turn_cost(vec![live_request("model", 1_000, 1_234_000_000)]);
    let unreported = turn_cost(vec![ModelUsage::unreported("model", 2)]);

    // Assert
    assert_eq!(sub_cent.cost.panel_text(), "<$0.01");
    assert_eq!(lower_bound_sub_cent.cost.panel_text(), "≥$0.00");
    assert_eq!(lower_bound_sub_cent.input.panel_text(), "≥1.0k");
    assert_eq!(thousands.cost.panel_text(), "$1,230");
    assert_eq!(unreported.input, UsageFigure::Unreported);
    assert_eq!(unreported.cost, UsageFigure::Unpriced);
}

#[test]
fn test_readout_figures_ignore_a_session_turn_without_model_usage() {
    // Arrange
    let stored = session_usage(vec![priced_record("model", UsagePurpose::Turn)]);
    let live = HashMap::from([live_entry("session", LiveUsageRun::SessionTurn, Vec::new())]);

    // Act
    let figures = session_readout_figures(Some(&stored), &SessionId::from("session"), &live)
        .expect("stored usage has readout figures");

    // Assert
    assert_eq!(figures.turn, None);
    assert_eq!(
        figures.session_cost,
        UsageFigure::Cost(CostFigure {
            is_api_equivalent: false,
            is_lower_bound: false,
            is_upper_bound: false,
            text: "$3.27".to_string(),
        })
    );
}

#[test]
fn test_readout_context_without_window_keeps_the_absolute_size() {
    // Arrange
    let stored = SessionUsage {
        context: Some(ContextNow {
            tokens: 71_200,
            window_tokens: Some(0),
        }),
        records: Vec::new(),
        turn_count: 1,
    };

    // Act
    let figures =
        session_readout_figures(Some(&stored), &SessionId::from("session"), &HashMap::new())
            .expect("stored context has readout figures");

    // Assert
    assert_eq!(
        figures.context,
        Some(ReadoutContext {
            gauge_filled: None,
            percent: None,
            tokens: "71.2k".to_string(),
        })
    );
    assert_eq!(figures.session_cost, UsageFigure::NotRecorded);
}

#[test]
fn test_context_gauge_and_tone_follow_the_fill() {
    // Arrange, Act, Assert
    assert_eq!(context_gauge(0), "▱▱▱▱▱");
    assert_eq!(context_gauge(2), "▰▰▱▱▱");
    assert_eq!(context_gauge(9), "▰▰▰▰▰");
    assert_eq!(context_tone(None), palette::text_muted());
    assert_eq!(context_tone(Some(74)), palette::text_muted());
    assert_eq!(context_tone(Some(75)), palette::warning());
    assert_eq!(context_tone(Some(90)), palette::danger());
}

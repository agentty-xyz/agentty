//! Tests for Claude Code usage: normalized cached input, message-id
//! deduplication, subagent exclusion, context windows, and each turn's share
//! of the per-model `modelUsage` totals.

use ag_contracts::{ContextFill, CostBasis, ModelUsage, TurnUsage};
use semver::Version;
use serde_json::{Value, json};

use super::{ModelTotals, micro_usd};
use crate::agent::claude_usage::{AgentTotals, ClaudeRun, ClaudeUsageMeter, turn_usage};

/// Model a turn requests, which untrusted runs are charged under.
const REQUESTED: &str = "claude-sonnet-5-5";
/// Model of every main-loop response [`claude_run`] observes.
const CONTEXT_MODEL: &str = "claude-opus-5-5";
/// Claude Code release that restores a resumed session's earlier totals.
const CUMULATIVE: &str = "2.1.277";

/// One turn usage row: model, input, output, requests, cost basis, and the
/// latest request's input fill.
type Row<'a> = (&'a str, u64, u64, u32, Option<CostBasis>, Option<u64>);

/// One [`turn_usage`] scenario.
struct TurnCase {
    /// Checkpoint the host passed in.
    checkpoint: Option<String>,
    /// Index of the run whose totals become the new checkpoint.
    checkpoint_run: Option<usize>,
    /// Scenario label for assertion messages.
    name: &'static str,
    /// Whether the turn continued the session.
    resumed: bool,
    /// Expected rows of the turn usage.
    rows: Vec<Row<'static>>,
    /// Release and `modelUsage` of each process, in order.
    runs: Vec<(Option<&'static str>, Option<Value>)>,
}

/// Returns a main-loop assistant message event with `id` and `input` fresh
/// input tokens over a 1,000-token cached prompt.
fn assistant(id: &str, input: u64, parent: Option<&str>) -> Value {
    json!({
        "type": "assistant",
        "parent_tool_use_id": parent,
        "message": {
            "id": id,
            "model": CONTEXT_MODEL,
            "usage": {
                "input_tokens": input,
                "cache_read_input_tokens": 1_000,
                "cache_creation_input_tokens": 10,
                "output_tokens": 1
            }
        }
    })
}

/// Returns the main-loop usage and totals of one process that reported
/// `version`, two main-loop responses, and a result with `model_usage`.
///
/// Its main loop has 3,030 input tokens, 400 output tokens, two requests,
/// and a latest fill of 1,030 tokens.
fn claude_run(
    version: Option<&str>,
    model_usage: Option<Value>,
) -> (ModelUsage, Option<AgentTotals>) {
    let mut meter = ClaudeUsageMeter::default();
    let mut result = json!({
        "type": "result",
        "usage": {"input_tokens": 30, "cache_read_input_tokens": 3_000, "output_tokens": 400}
    });
    if let Some(model_usage) = model_usage {
        result["modelUsage"] = model_usage;
    }
    meter.observe(&json!({"type": "system", "subtype": "init", "claude_code_version": version}));
    meter.observe(&assistant("msg-1", 5, None));
    meter.observe(&assistant("msg-2", 20, None));
    meter.observe(&result);

    (meter.usage(), meter.totals())
}

/// Returns one `modelUsage` entry without cache counts.
fn entry(input: u64, output: u64, cost_usd: Option<f64>) -> Value {
    json!({"inputTokens": input, "outputTokens": output, "costUSD": cost_usd})
}

/// Returns the checkpoint a resumed process reporting `model_usage` leaves.
fn checkpoint_of(model_usage: Value) -> String {
    let (_, totals) = claude_run(Some(CUMULATIVE), Some(model_usage));
    let models = totals.expect("the run reports totals").models;

    serde_json::to_string(&models).expect("totals serialize")
}

/// Returns the expected row of a model that carries the run's context fill
/// of 1,030 tokens.
fn context_row(
    model: &'static str,
    input: u64,
    output: u64,
    requests: u32,
    basis: CostBasis,
) -> Row<'static> {
    (model, input, output, requests, Some(basis), Some(1_030))
}

/// Returns the expected row of a model priced at list price for one request
/// without the run's context fill.
fn listed_row(model: &'static str, input: u64, output: u64) -> Row<'static> {
    (model, input, output, 1, Some(CostBasis::ListPrice), None)
}

/// Returns the comparable rows of `usage`.
fn rows(usage: &TurnUsage) -> Vec<Row<'_>> {
    usage
        .models
        .iter()
        .map(|row| {
            (
                row.model.as_str(),
                row.input_tokens,
                row.output_tokens,
                row.requests,
                row.cost.map(|cost| cost.basis),
                row.context.map(|context| context.last_input_tokens),
            )
        })
        .collect()
}

/// Runs `case` through [`turn_usage`] and checks its rows and checkpoint.
fn assert_turn_case(case: &TurnCase) {
    // Arrange
    let runs = case
        .runs
        .iter()
        .map(|(version, model_usage)| claude_run(*version, model_usage.clone()))
        .collect::<Vec<_>>();
    let expected_checkpoint = case.checkpoint_run.map(|index| {
        let totals = runs[index].1.as_ref().expect("the run reports totals");

        serde_json::to_string(&totals.models).expect("totals serialize")
    });

    // Act
    let usage = turn_usage(
        REQUESTED,
        case.resumed,
        case.checkpoint.as_deref(),
        runs.iter().map(|(main_loop, totals)| ClaudeRun {
            main_loop,
            totals: totals.as_ref(),
        }),
    );

    // Assert
    assert_eq!(rows(&usage), case.rows, "{}", case.name);
    assert_eq!(usage.checkpoint, expected_checkpoint, "{}", case.name);
}

#[test]
fn responses_count_once_per_message_id_until_the_result_arrives() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();

    // Act
    let reported = [
        meter.observe(&assistant("msg-1", 5, None)),
        meter.observe(&assistant("msg-1", 5, None)),
        meter.observe(&assistant("msg-2", 20, None)),
        meter.observe(&assistant("sub-1", 900, Some("toolu-1"))),
        meter.observe(&json!({"type": "system"})),
        meter.observe(&json!({"type": "assistant"})),
    ];
    let usage = meter.usage();

    // Assert
    assert_eq!(reported, [true, false, true, false, false, false]);
    assert_eq!(
        usage,
        ModelUsage {
            cache_read_tokens: Some(2_000),
            cache_write_tokens: Some(20),
            context: Some(ContextFill {
                last_input_tokens: 1_030,
                peak_input_tokens: 1_030,
                window_tokens: None,
            }),
            input_tokens: 2_045,
            output_tokens: 2,
            requests: 2,
            ..ModelUsage::new("")
        }
    );
}

#[test]
fn results_supersede_responses_and_name_the_context_window() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();
    meter.observe(&assistant("msg-1", 5, None));

    // Act
    meter.observe(&json!({
        "type": "result",
        "usage": {
            "input_tokens": 30,
            "cache_read_input_tokens": 3_000,
            "cache_creation_input_tokens": 0,
            "output_tokens": 400
        },
        "modelUsage": {
            "claude-opus-5-5": {"contextWindow": 1_000_000},
            "claude-haiku-5-5": {}
        }
    }));
    let usage = meter.usage();

    // Assert
    assert_eq!(usage.input_tokens, 3_030);
    assert_eq!(usage.cache_read_tokens, Some(3_000));
    assert_eq!(usage.output_tokens, 400);
    assert_eq!(
        usage.context.and_then(|context| context.window_tokens),
        Some(1_000_000)
    );
    assert_eq!(meter.totals(), None, "entries without counts are no totals");
}

#[test]
fn totals_follow_the_latest_result_with_the_release_and_context_model() {
    // Arrange
    let mut meter = ClaudeUsageMeter::default();
    meter.observe(&json!({"type": "system", "claude_code_version": "2.1.300"}));
    meter.observe(&assistant("msg-1", 5, None));

    // Act
    meter.observe_payload(&json!([
        {"type": "result", "usage": {"input_tokens": 1, "output_tokens": 1},
         "modelUsage": {"claude-opus-5-5": {"inputTokens": 1, "outputTokens": 1}}},
        {"type": "result", "usage": {"input_tokens": 1, "output_tokens": 1},
         "modelUsage": {"claude-haiku-5-5": {
             "inputTokens": 2, "outputTokens": 3, "cacheReadInputTokens": 4,
             "cacheCreationInputTokens": 5, "thinkingTokens": 1, "costUSD": 0.03}}}
    ]));
    let totals = meter.totals().expect("the latest result reports totals");

    // Assert
    assert_eq!(totals.context_model.as_deref(), Some(CONTEXT_MODEL));
    assert_eq!(totals.version, Some(Version::new(2, 1, 300)));
    assert_eq!(
        totals.models.into_iter().collect::<Vec<_>>(),
        [(
            "claude-haiku-5-5".to_string(),
            ModelTotals {
                cache_read: 4,
                cache_write: 5,
                cost: Some(30_000),
                input: 2,
                output: 3,
                thinking: Some(1),
            }
        )]
    );
    assert_eq!(meter.usage().input_tokens, 2);
}

#[test]
fn micro_usd_rounds_dollars_and_rejects_negative_or_non_finite_amounts() {
    // Arrange
    let cases = [
        (0.012_345_6, Some(12_346)),
        (0.0, Some(0)),
        (-0.5, None),
        (f64::NAN, None),
        (f64::INFINITY, None),
    ];

    for (dollars, expected) in cases {
        // Act
        let micro = micro_usd(dollars);

        // Assert
        assert_eq!(micro, expected, "{dollars}");
    }
}

#[test]
fn missing_counts_are_unreported() {
    // Arrange
    let mut silent = ClaudeUsageMeter::default();
    let mut partial = ClaudeUsageMeter::default();

    // Act
    partial.observe(&json!({"type": "result", "usage": {"output_tokens": 3}}));

    // Assert
    assert_eq!(silent.usage(), ModelUsage::unreported("", 1));
    assert!(!silent.observe(&json!({"type": "assistant", "message": {"id": "msg"}})));
    let partial = partial.usage();
    assert_eq!(partial.unreported_requests, 1);
    assert_eq!(partial.cache_read_tokens, None);
}

#[test]
fn model_totals_since_keeps_optional_counts_only_when_both_report_them() {
    // Arrange
    let totals = |cost, thinking| ModelTotals {
        cost,
        thinking,
        ..ModelTotals::UNUSED
    };
    let cases = [
        (
            (Some(5), Some(7)),
            (Some(3), Some(2)),
            Some((Some(2), Some(5))),
        ),
        ((Some(5), None), (None, Some(2)), Some((None, None))),
        ((Some(1), Some(7)), (Some(3), Some(2)), None),
        ((Some(5), Some(1)), (Some(3), Some(2)), None),
    ];

    for ((cost, thinking), (earlier_cost, earlier_thinking), expected) in cases {
        // Act
        let share = totals(cost, thinking).since(totals(earlier_cost, earlier_thinking));

        // Assert
        assert_eq!(
            share,
            expected.map(|(cost, thinking)| totals(cost, thinking)),
            "{cost:?} {thinking:?}"
        );
    }
}

#[test]
fn turn_usage_charges_each_run_its_share_of_the_totals() {
    // Arrange
    let earlier = Some(checkpoint_of(
        json!({"claude-opus-5-5": entry(100, 10, Some(0.005)), REQUESTED: entry(7, 7, None)}),
    ));
    let current = json!({
        "claude-haiku-5-5": entry(20, 2, None),
        "claude-opus-5-5": entry(130, 50, Some(0.015)),
        REQUESTED: entry(7, 7, None)
    });
    let haiku = listed_row("claude-haiku-5-5", 20, 2);
    let reported = CostBasis::AgentReported;
    let cases = [
        TurnCase {
            checkpoint: earlier.clone(),
            checkpoint_run: None,
            name: "fresh runs are charged absolute totals",
            resumed: false,
            rows: vec![haiku, context_row(CONTEXT_MODEL, 1_100, 50, 2, reported)],
            runs: vec![(
                Some(CUMULATIVE),
                Some(json!({
                    "claude-haiku-5-5": entry(20, 2, None),
                    "claude-opus-5-5": {"inputTokens": 100, "outputTokens": 50, "cacheReadInputTokens": 1_000, "costUSD": 0.01}
                })),
            )],
        },
        TurnCase {
            checkpoint: earlier.clone(),
            checkpoint_run: Some(0),
            name: "resumed runs on cumulative releases are charged deltas",
            resumed: true,
            rows: vec![haiku, context_row(CONTEXT_MODEL, 30, 40, 2, reported)],
            runs: vec![(Some(CUMULATIVE), Some(current.clone()))],
        },
        TurnCase {
            checkpoint: earlier.clone(),
            checkpoint_run: Some(0),
            name: "resumed runs on older releases are charged absolute totals",
            resumed: true,
            rows: vec![
                haiku,
                context_row(CONTEXT_MODEL, 130, 50, 2, reported),
                listed_row(REQUESTED, 7, 7),
            ],
            runs: vec![(Some("2.1.276"), Some(current))],
        },
        TurnCase {
            checkpoint: earlier,
            checkpoint_run: Some(1),
            name: "a repair run is charged against the turn run's totals",
            resumed: true,
            rows: vec![context_row(CONTEXT_MODEL, 60, 50, 4, reported)],
            runs: vec![
                (
                    Some(CUMULATIVE),
                    Some(json!({"claude-opus-5-5": entry(130, 50, Some(0.015))})),
                ),
                (
                    Some(CUMULATIVE),
                    Some(json!({"claude-opus-5-5": entry(160, 60, Some(0.02))})),
                ),
            ],
        },
        TurnCase {
            checkpoint: None,
            checkpoint_run: None,
            name: "context falls back to the requested model",
            resumed: false,
            rows: vec![context_row(REQUESTED, 40, 4, 2, CostBasis::ListPrice)],
            runs: vec![(
                Some(CUMULATIVE),
                Some(json!({
                    "claude-opus-5-5": entry(0, 0, Some(0.0)),
                    REQUESTED: entry(40, 4, None)
                })),
            )],
        },
    ];

    // Act & Assert
    for case in &cases {
        assert_turn_case(case);
    }
}

#[test]
fn turn_usage_charges_untrusted_runs_their_main_loop_at_list_price() {
    // Arrange
    let earlier = checkpoint_of(json!({"claude-opus-5-5": entry(100, 10, None)}));
    let current = Some(json!({"claude-opus-5-5": entry(130, 50, None)}));
    let main_loop = context_row(REQUESTED, 3_030, 400, 2, CostBasis::ListPrice);
    let case = |name, checkpoint: Option<&str>, version, model_usage: Option<Value>| TurnCase {
        checkpoint: checkpoint.map(str::to_string),
        checkpoint_run: model_usage.is_some().then_some(0),
        name,
        resumed: true,
        rows: vec![main_loop],
        runs: vec![(version, model_usage)],
    };
    let cases = [
        case(
            "unknown release",
            Some(&earlier),
            Some("unknown"),
            current.clone(),
        ),
        case("no checkpoint", None, Some(CUMULATIVE), current.clone()),
        case(
            "unparsable checkpoint",
            Some("{"),
            Some(CUMULATIVE),
            current,
        ),
        case(
            "a total went down",
            Some(&earlier),
            Some(CUMULATIVE),
            Some(json!({"claude-opus-5-5": entry(90, 50, None)})),
        ),
        case("no totals", Some(&earlier), Some(CUMULATIVE), None),
    ];

    // Act & Assert
    for case in &cases {
        assert_turn_case(case);
    }
}

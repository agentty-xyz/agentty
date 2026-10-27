//! Deterministic context-benchmark metric checks.

use std::time::Duration;

use ag_harness::tool::ReadAction;
use ag_harness::turn::ToolActivity;
use serde_json::{Value, json};

use crate::metric::{deferred_reads, duplicate_reads, record_cache_reuse};

fn file_read(path: &str, start_line: u64) -> ToolActivity {
    ToolActivity::Read {
        duration: Duration::ZERO,
        end_line: Some(start_line + 10),
        path: path.to_string(),
        start_line,
        truncated: false,
    }
}

fn search(path: &str) -> ToolActivity {
    ToolActivity::ReadInspection {
        action: ReadAction::Search,
        arguments: json!({ "action": "search", "path": path, "query": "pub fn" }).to_string(),
        duration: Duration::ZERO,
        summary: "pub fn".to_string(),
    }
}

fn legacy_search(path: &str) -> ToolActivity {
    ToolActivity::ReadInspection {
        action: ReadAction::Search,
        arguments: String::new(),
        duration: Duration::ZERO,
        summary: format!("pub fn in {path}"),
    }
}

fn turn_request(input: u64, hit: u64, reductions: &Value) -> Value {
    json!({
        "purpose": "turn",
        "input_tokens": input,
        "cache_hit_tokens": hit,
        "reductions": reductions,
    })
}

#[test]
fn duplicate_reads_key_inspections_by_their_arguments() {
    // Arrange
    let cases = [
        (
            "one search query across different path filters",
            vec![search("src/a.rs"), search("src/b.rs"), search("src/c.rs")],
            0,
        ),
        (
            "the same search twice",
            vec![search("src/a.rs"), search("src/b.rs"), search("src/a.rs")],
            1,
        ),
        (
            "legacy inspections without arguments key on their summary",
            vec![
                legacy_search("src/a.rs"),
                legacy_search("src/b.rs"),
                legacy_search("src/a.rs"),
                search("src/a.rs"),
            ],
            1,
        ),
        (
            "file reads repeat by path and starting line",
            vec![
                file_read("src/a.rs", 1),
                file_read("src/a.rs", 200),
                file_read("src/a.rs", 1),
            ],
            1,
        ),
        (
            "a repeat answered without running",
            vec![
                file_read("src/a.rs", 1),
                ToolActivity::ReadRepeated {
                    action: ReadAction::File,
                    duration: Duration::ZERO,
                    summary: "src/a.rs".to_string(),
                },
            ],
            1,
        ),
        (
            "a deferred read is not a read, so fetching it later is no repeat",
            vec![
                ToolActivity::ReadDeferred {
                    action: ReadAction::File,
                    duration: Duration::ZERO,
                    summary: "src/a.rs".to_string(),
                },
                file_read("src/a.rs", 1),
            ],
            0,
        ),
        (
            "writes and rejected reads are not reads",
            vec![
                ToolActivity::Write {
                    bytes_written: 1,
                    duration: Duration::ZERO,
                    path: "src/a.rs".to_string(),
                },
                ToolActivity::ReadRejected {
                    duration: Duration::ZERO,
                    path: "src/a.rs".to_string(),
                },
                ToolActivity::ReadRejected {
                    duration: Duration::ZERO,
                    path: "src/a.rs".to_string(),
                },
            ],
            0,
        ),
    ];

    for (case, tool_calls, expected) in cases {
        // Act
        let duplicates = duplicate_reads(&tool_calls);

        // Assert
        assert_eq!(duplicates, expected, "{case}");
    }
}

#[test]
fn deferred_reads_count_only_reads_answered_with_their_call() {
    // Arrange
    let deferred = ToolActivity::ReadDeferred {
        action: ReadAction::Search,
        duration: Duration::ZERO,
        summary: "pub fn".to_string(),
    };
    let tool_calls = vec![file_read("src/a.rs", 1), deferred.clone(), deferred];

    // Act
    let deferred = deferred_reads(&tool_calls);

    // Assert
    assert_eq!(deferred, 2);
}

#[test]
fn cache_reuse_attributes_lost_prefixes_to_reductions_or_the_provider() {
    // Arrange
    let none = json!({});
    let elided = json!({ "elided_results": 2 });
    let mut requests = vec![
        turn_request(1_000, 0, &none),
        turn_request(4_000, 990, &none),
        turn_request(6_000, 1_024, &elided),
        json!({
            "purpose": "context_summary",
            "input_tokens": 2_000,
            "cache_hit_tokens": 0,
            "reductions": {},
        }),
        turn_request(5_000, 0, &elided),
        turn_request(7_000, 6_000, &elided),
    ];

    // Act
    let totals = record_cache_reuse(&mut requests);

    // Assert
    let pairs = requests
        .iter()
        .map(|request| {
            (
                request["cache_expected_tokens"].clone(),
                request["prefix_held"].clone(),
                request["reduction_event"].clone(),
                request["lost_to"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [
            (Value::Null, Value::Null, Value::Null, Value::Null),
            (json!(1_000), json!(true), json!(false), Value::Null),
            (json!(4_000), json!(false), json!(true), json!("harness")),
            (Value::Null, Value::Null, Value::Null, Value::Null),
            (json!(5_000), json!(false), json!(false), json!("provider")),
            (json!(5_000), json!(true), json!(false), Value::Null),
        ]
    );
    assert_eq!(
        Value::Object(totals),
        json!({
            "cache_hit_tokens": 8_014,
            "cached_share": 0.3206,
            "reduction_events": 1,
            "rewritten_prefixes": 0,
            "turn_boundaries": { "pairs": 0, "held": 0, "rewritten": 0 },
            "held_prefixes": 2,
            "lost_prefixes": { "harness": 1, "provider": 1 },
            "lost_cache_tokens": { "harness": 2_976, "provider": 5_000 },
        })
    );
}

#[test]
fn cache_reuse_charges_the_harness_only_for_rewritten_messages() {
    // Arrange
    let none = json!({});
    let cut = json!({ "truncated_results": 1 });
    let sent = |turn: usize, digests: &[u64], input: u64, hit: u64, reductions: &Value| {
        let mut request = turn_request(input, hit, reductions);
        request["turn"] = json!(turn);
        request["sent"] = json!(digests);

        request
    };
    let mut requests = vec![
        sent(0, &[1, 2], 1_000, 0, &none),
        // Newly cuts its latest round but keeps every earlier message.
        sent(0, &[1, 2, 3, 4], 3_000, 0, &cut),
        // The next turn replays the first turn with message 3 rewritten.
        sent(1, &[1, 2, 5, 4, 6, 7], 4_000, 500, &none),
        sent(1, &[1, 2, 5, 4, 6, 7, 8], 4_500, 4_000, &none),
    ];

    // Act
    let totals = record_cache_reuse(&mut requests);

    // Assert
    let pairs = requests
        .iter()
        .map(|request| {
            (
                request["prefix_messages"].clone(),
                request["prefix_rewritten"].clone(),
                request.get("sent").cloned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [
            (Value::Null, Value::Null, None),
            (json!(2), json!(false), None),
            (json!(2), json!(true), None),
            (json!(6), json!(false), None),
        ]
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request["lost_to"].clone())
            .collect::<Vec<_>>(),
        [
            Value::Null,
            json!("provider"),
            json!("harness"),
            Value::Null
        ]
    );
    assert_eq!(totals["reduction_events"], json!(2));
    assert_eq!(totals["rewritten_prefixes"], json!(1));
    assert_eq!(
        totals["turn_boundaries"],
        json!({ "pairs": 1, "held": 0, "rewritten": 1 })
    );
    assert_eq!(
        totals["lost_prefixes"],
        json!({ "harness": 1, "provider": 1 })
    );
    assert_eq!(
        totals["lost_cache_tokens"],
        json!({ "harness": 2_500, "provider": 1_000 })
    );
}

#[test]
fn cache_reuse_without_usage_reports_no_share() {
    // Arrange
    let mut requests = vec![
        json!({
            "purpose": "turn",
            "input_tokens": null,
            "cache_hit_tokens": null,
            "reductions": {},
        }),
        json!({
            "purpose": "turn",
            "input_tokens": 900,
            "cache_hit_tokens": null,
            "reductions": { "truncated_results": 1 },
        }),
    ];

    // Act
    let totals = record_cache_reuse(&mut requests);

    // Assert
    assert_eq!(requests[1]["cache_expected_tokens"], Value::Null);
    assert_eq!(requests[1]["prefix_held"], Value::Null);
    assert_eq!(requests[1]["reduction_event"], json!(true));
    assert_eq!(totals["cache_hit_tokens"], Value::Null);
    assert_eq!(totals["cached_share"], Value::Null);
    assert_eq!(totals["reduction_events"], json!(1));
}

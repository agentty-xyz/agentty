//! Deterministic context-benchmark metric checks.

use std::time::Duration;

use ag_harness::tool::ReadAction;
use ag_harness::turn::ToolActivity;
use serde_json::{Value, json};

use crate::metric::{duplicate_reads, record_cache_reuse};

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
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [
            (Value::Null, Value::Null, Value::Null),
            (json!(1_000), json!(true), json!(false)),
            (json!(4_000), json!(false), json!(true)),
            (Value::Null, Value::Null, Value::Null),
            (json!(5_000), json!(false), json!(false)),
            (json!(5_000), json!(true), json!(false)),
        ]
    );
    assert_eq!(
        Value::Object(totals),
        json!({
            "cache_hit_tokens": 8_014,
            "cached_share": 0.3206,
            "reduction_events": 1,
            "held_prefixes": 2,
            "lost_prefixes": { "harness": 1, "provider": 1 },
            "lost_cache_tokens": { "harness": 2_976, "provider": 5_000 },
        })
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

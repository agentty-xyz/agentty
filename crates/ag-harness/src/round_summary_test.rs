//! Round-summary source rendering and request construction tests.

use serde_json::{Value, json};

use crate::compaction::{MAX_SOURCE_TEXT_BYTES, summary_schema};
use crate::lifecycle::ContextSummaryTrigger;
use crate::model::ModelMessage;
use crate::round_summary::{ROUND_SUMMARY_INSTRUCTIONS, RoundSummaryJob, render_source};
use crate::tool::ToolCall;

fn read_round(id: &str, path: &str, content: &str) -> Vec<ModelMessage> {
    let call = ToolCall::from_json(
        id.to_string(),
        "read",
        &json!({"action": "file", "path": path}).to_string(),
        None,
    )
    .expect("read call");

    vec![
        ModelMessage::AssistantToolCall(call),
        ModelMessage::ToolResult {
            call_id: id.to_string(),
            content: content.to_string(),
            name: "read".to_string(),
        },
    ]
}

fn source_lines(source: &str) -> Vec<Value> {
    source
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).expect("source line is JSON"))
        .collect()
}

#[test]
fn render_source_lists_the_task_earlier_summary_and_numbered_rounds() {
    // Arrange
    let task = ModelMessage::User("List every public function.".to_string());
    let previous = json!({"context": "src/a.rs defines alpha", "decisions": [], "state": "a done"});
    let first = read_round("r1", "src/b.rs", "pub fn beta() {}");
    let second = read_round("r2", "src/c.rs", "pub fn gamma() {}");

    // Act
    let source = render_source(
        &task,
        Some(&previous),
        &[&first, &second],
        MAX_SOURCE_TEXT_BYTES,
        2048,
    );

    // Assert
    assert!(
        source
            .lines()
            .next()
            .expect("preamble")
            .contains("at most 2048 bytes")
    );
    let lines = source_lines(&source);
    assert_eq!(lines[0], json!({"task": "List every public function."}));
    assert_eq!(lines[1], json!({"earlier_summary": previous}));
    assert_eq!(lines[2]["round"], 1);
    assert_eq!(
        lines[2]["messages"][0]["tool_calls"][0]["arguments"],
        json!({"path": "src/b.rs"})
    );
    assert_eq!(lines[2]["messages"][1]["result"], "pub fn beta() {}");
    assert_eq!(lines[3]["round"], 2);
    assert_eq!(lines.len(), 4);
}

#[test]
fn render_source_cuts_every_text_to_the_level() {
    // Arrange
    let task = ModelMessage::User(format!("task {}", "t".repeat(100)));
    let round = read_round("r1", "src/b.rs", &"x".repeat(100));

    // Act
    let source = render_source(&task, None, &[&round], 0, 2048);

    // Assert
    let lines = source_lines(&source);
    assert_eq!(lines[0]["task"], "[... 105 bytes omitted ...]");
    assert_eq!(
        lines[1]["messages"][1]["result"],
        "[... 100 bytes omitted ...]"
    );
}

#[test]
fn job_requests_a_tool_free_summary_of_its_source() {
    // Arrange
    let schema = summary_schema().expect("summary schema");

    // Act
    let job = RoundSummaryJob::new(
        3..9,
        ContextSummaryTrigger::ContextOverflow,
        "source text".to_string(),
        512,
        schema.clone(),
    );

    // Assert
    assert_eq!(job.range(), 3..9);
    assert_eq!(job.trigger(), ContextSummaryTrigger::ContextOverflow);
    assert_eq!(job.max_summary_bytes(), 512);
    assert_eq!(job.source_bytes(), "source text".len());
    assert_eq!(
        job.request().messages()[0],
        ModelMessage::System(ROUND_SUMMARY_INSTRUCTIONS.to_string())
    );
    assert_eq!(job.request().prompt(), "source text");
    assert_eq!(job.request().tools(), []);
    assert_eq!(job.request().schema(), &schema);
}

//! Round-summary source rendering and request construction tests.

use serde_json::{Value, json};

use crate::compaction::{MAX_SOURCE_TEXT_BYTES, summary_schema};
use crate::lifecycle::ContextSummaryTrigger;
use crate::model::{ModelMessage, ModelRequest, ReasoningEffort};
use crate::round_summary::{
    CONTINUED_SUMMARY_PREFIX, CoveredCalls, ROUND_SUMMARY_INSTRUCTIONS, RoundSummaryJob,
    render_source,
};
use crate::schema_contract::OutputSchema;
use crate::tool::{ToolCall, ToolDefinition};

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
    assert_eq!(
        job.request().model_reasoning_effort(),
        Some(ReasoningEffort::Low)
    );
    assert_eq!(job.request().prefix_schema(), None);
    assert!(ROUND_SUMMARY_INSTRUCTIONS.contains("Record only what the rounds show: never infer"));
    assert!(ROUND_SUMMARY_INSTRUCTIONS.contains("within the schema's maxLength and maxItems"));
}

#[test]
fn continuing_job_appends_its_instruction_to_the_last_sent_request() {
    // Arrange
    let turn_schema = OutputSchema::new(json!({"type": "object"})).expect("turn schema");
    let sent = ModelRequest::new("List every public function.", turn_schema.clone())
        .with_tool(ToolDefinition::read())
        .with_model_reasoning_effort(ReasoningEffort::High)
        .with_cache_key("session-1".to_string());
    let schema = summary_schema().expect("summary schema");
    let covered = |carries_summary| CoveredCalls {
        carries_summary,
        first: "r1",
        last: "r4",
    };

    // Act
    let job = RoundSummaryJob::continuing(
        2..9,
        ContextSummaryTrigger::Proactive,
        &sent,
        &covered(false),
        700,
        512,
        schema.clone(),
    );
    let carried = RoundSummaryJob::continuing(
        2..9,
        ContextSummaryTrigger::Proactive,
        &sent,
        &covered(true),
        700,
        512,
        schema.clone(),
    );

    // Assert
    let request = job.request();
    let (instruction, continued) = request.messages().split_last().expect("instruction");
    assert_eq!(continued, sent.messages());
    let ModelMessage::User(text) = instruction else {
        std::panic::resume_unwind(Box::new(format!(
            "instruction expected, got {instruction:?}"
        )));
    };
    assert!(text.starts_with(CONTINUED_SUMMARY_PREFIX));
    assert!(text.contains("do not call any tool"));
    assert!(text.contains("from call `r1` through call `r4`"));
    assert!(text.contains("at most 512 bytes"));
    assert!(text.ends_with(&schema.value().to_string()));
    assert!(!text.contains("Carry forward"));
    assert!(
        carried
            .request()
            .messages()
            .last()
            .is_some_and(|message| matches!(
                message,
                ModelMessage::User(text) if text.contains("Carry forward")
            ))
    );
    assert_eq!(request.tools(), sent.tools());
    assert_eq!(
        request.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert_eq!(request.cache_key(), Some("session-1"));
    assert_eq!(request.prefix_schema(), Some(&turn_schema));
    assert_eq!(request.schema(), &schema);
    assert_eq!(job.range(), 2..9);
    assert_eq!(job.source_bytes(), 700);
    assert_eq!(job.max_summary_bytes(), 512);
}

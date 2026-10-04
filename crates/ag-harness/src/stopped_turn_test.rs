use super::{RejectedContent, StoppedTurn};
use crate::model::ModelMessage;
use crate::tool::{ToolCall, ToolCallArguments};

fn note_text(stopped: StoppedTurn, error_type: &str) -> String {
    match stopped.note(error_type) {
        ModelMessage::User(text) => Some(text),
        _ => None,
    }
    .expect("note should be a user message")
}

#[test]
fn notes_name_the_stop_and_warn_about_partial_effects() {
    // Arrange
    let stops = [
        (StoppedTurn::Failed, "turn_failed", "failed"),
        (StoppedTurn::Interrupted, "turn_aborted", "was interrupted"),
    ];

    // Act / Assert
    for (stopped, tag, outcome) in stops {
        let text = note_text(stopped, "cancelled");
        assert!(text.starts_with(&format!("<{tag}>\n")));
        assert!(text.ends_with(&format!("\n</{tag}>")));
        assert!(text.contains(&format!(
            "The previous turn {outcome} before it finished (reason: cancelled)"
        )));
        assert!(text.contains("may have partially executed"));
    }
}

#[test]
fn reasons_are_sanitized_bounded_and_never_empty() {
    // Arrange
    let oversized = "x".repeat(100);

    // Act
    let control = note_text(StoppedTurn::Failed, "Model(\nInvalidOutput)");
    let bounded = note_text(StoppedTurn::Failed, &oversized);
    let missing = note_text(StoppedTurn::Interrupted, "  ");

    // Assert
    assert!(control.contains("(reason: Model(\u{fffd}InvalidOutput))"));
    assert!(bounded.contains(&format!("(reason: {})", "x".repeat(64))));
    assert!(missing.contains("(reason: unknown)"));
}

#[test]
fn only_turns_ending_with_a_note_reduce_to_input_and_note() {
    // Arrange
    let note = StoppedTurn::Interrupted.note("cancelled");
    let stopped = [
        ModelMessage::User("input".into()),
        ModelMessage::Assistant("progress".into()),
        note.clone(),
    ];
    let completed = [
        ModelMessage::User("input".into()),
        ModelMessage::Assistant("answer".into()),
    ];
    let lookalike = [ModelMessage::User("<turn_failed>".into())];

    // Act
    let reduced = StoppedTurn::input_and_note(&stopped);
    let unreduced = [
        StoppedTurn::input_and_note(&completed),
        StoppedTurn::input_and_note(&lookalike),
        StoppedTurn::input_and_note(&[note]),
    ];

    // Assert
    assert_eq!(
        reduced,
        Some(vec![ModelMessage::User("input".into()), stopped[2].clone()])
    );
    assert_eq!(unreduced, [None, None, None]);
}

#[test]
fn rejected_tool_results_also_omit_their_calls_source_patch_and_reasoning() {
    // Arrange
    let call = |id: &str, name: &str, arguments: &str, reasoning: Option<&str>| {
        ToolCall::from_json(id.into(), name, arguments, reasoning.map(str::to_string))
            .expect("tool call")
    };
    let accepted = call("accepted", "bash", r#"{"command":"make"}"#, Some("plan"));
    let read = call("read", "read", r#"{"path":"src/lib.rs"}"#, None);
    let write = call(
        "write",
        "write",
        r#"{"path":"src/lib.rs","patch":"large patch"}"#,
        Some("batch plan"),
    );
    let bash = call("bash", "bash", r#"{"command":"cargo test"}"#, Some("plan"));
    let mut turn = [
        ModelMessage::User("input".into()),
        ModelMessage::AssistantToolCall(accepted.clone()),
        tool_result("accepted", "accepted result"),
        ModelMessage::AssistantToolCalls(vec![read.clone(), write]),
        tool_result("read", "read result"),
        tool_result("write", "write result"),
        ModelMessage::AssistantToolCall(bash),
        tool_result("bash", "bash result"),
    ];

    // Act
    let omitted = RejectedContent::ToolResults(3).omit(&mut turn);

    // Assert
    assert_eq!(omitted, [7, 5, 4, 6, 3]);
    assert_eq!(turn[1], ModelMessage::AssistantToolCall(accepted));
    assert_eq!(turn[2], tool_result("accepted", "accepted result"));
    let batch = match &turn[3] {
        ModelMessage::AssistantToolCalls(batch) => batch.as_slice(),
        _ => &[],
    };
    assert_eq!(batch.first(), Some(&read));
    let write = batch.get(1);
    assert!(
        write
            .and_then(ToolCall::write_arguments)
            .is_some_and(|arguments| arguments.path() == "src/lib.rs"
                && arguments.patch().starts_with("[Patch omitted:"))
    );
    assert!(
        write
            .and_then(ToolCall::reasoning_content)
            .is_some_and(|reasoning| reasoning.starts_with("[Reasoning omitted:"))
    );
    assert!(matches!(
        &turn[6],
        ModelMessage::AssistantToolCall(call) if call.id() == "bash"
            && matches!(
                call.arguments(),
                ToolCallArguments::Bash(arguments)
                    if arguments.command().starts_with("# Command omitted:")
            )
    ));
}

fn tool_result(call_id: &str, content: &str) -> ModelMessage {
    ModelMessage::ToolResult {
        call_id: call_id.into(),
        content: content.into(),
        name: "read".into(),
    }
}

#[test]
fn rejected_input_is_replaced_by_a_placeholder() {
    // Arrange
    let mut turn = [ModelMessage::User("offending input".into())];
    let mut empty: [ModelMessage; 0] = [];

    // Act
    let omitted = RejectedContent::Input.omit(&mut turn);
    let omitted_from_empty = RejectedContent::Input.omit(&mut empty);

    // Assert
    assert_eq!(omitted, [0]);
    assert!(matches!(
        &turn[0],
        ModelMessage::User(text) if text.starts_with("[Input omitted:")
    ));
    assert_eq!(omitted_from_empty, Vec::<usize>::new());
}

#[test]
fn rejected_tool_results_replace_only_the_latest_results() {
    // Arrange
    let input = ModelMessage::User("input".into());
    let call = ModelMessage::Assistant("call".into());
    let mut turn = [
        input.clone(),
        call.clone(),
        tool_result("accepted", "accepted result"),
        call.clone(),
        tool_result("first", "first result"),
        call.clone(),
        tool_result("second", "second result"),
    ];
    let mut short_turn = [input.clone(), call.clone(), tool_result("only", "result")];

    // Act
    let omitted = RejectedContent::ToolResults(2).omit(&mut turn);
    let omitted_from_short = RejectedContent::ToolResults(3).omit(&mut short_turn);

    // Assert
    assert_eq!(omitted, [6, 4]);
    assert_eq!(omitted_from_short, [2]);
    assert_eq!(turn[0], input);
    assert_eq!(turn[2], tool_result("accepted", "accepted result"));
    for position in [4, 6] {
        assert!(matches!(
            &turn[position],
            ModelMessage::ToolResult { content, .. }
                if content.starts_with("[Result omitted: the tool call ran")
        ));
    }
    assert_eq!(turn[5], call);
}

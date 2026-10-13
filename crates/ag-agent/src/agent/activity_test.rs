use std::sync::{Arc, Mutex};

use ag_contracts::{ActivityKind, ActivityStatus};
use ag_session::AgentKind;
use serde_json::json;

use super::{ActivityObserver, MAX_ACTIVITIES};

#[test]
fn codex_lifecycle_deduplicates_snapshots_and_settles_unfinished_calls() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer = ActivityObserver::new(move |event| {
        sink.lock()
            .expect("test fixture should succeed")
            .push(event);
    });
    let started = json!({"method":"item/started","params":{"item":{"id":"a","type":"commandExecution","command":"private"}}});
    let completed = json!({"method":"item/completed","params":{"item":{"id":"a","type":"commandExecution","exitCode":1}}});

    // Act
    observer.observe(AgentKind::Codex, &started);
    observer.observe(AgentKind::Codex, &started);
    observer.observe(AgentKind::Codex, &completed);
    observer.observe(AgentKind::Codex, &started);
    observer.codex_completed_turn(&json!({"params":{"turn":{"items":[completed["params"]["item"].clone(),{"id":"b","type":"webSearch"}]}}}));
    observer.observe(
        AgentKind::Codex,
        &json!({"method":"item/started","params":{"item":{"id":"c","type":"fileChange"}}}),
    );
    drop(observer);

    // Assert
    let events = events.lock().expect("test fixture should succeed");
    assert_eq!(events.len(), 5);
    assert_eq!(events[0].name, "shell");
    assert_eq!(events[1].status, ActivityStatus::Failed);
    assert_eq!(events[1].exit_code, Some(1));
    assert_eq!(events[2].status, ActivityStatus::Completed);
    assert_eq!(events[4].status, ActivityStatus::Interrupted);
    assert_eq!(events[4].kind, ActivityKind::FileChange);
    assert_eq!(events[0].attempt_id, events[4].attempt_id);
}

#[test]
fn claude_preserves_skill_parent_and_content() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer = ActivityObserver::new(move |event| {
        sink.lock()
            .expect("test fixture should succeed")
            .push(event);
    });

    // Act
    observer.observe_line(AgentKind::Claude, &json!({"type":"assistant","parent_tool_use_id":"parent","message":{"content":[{"type":"tool_use","id":"skill","name":"Skill","input":{"skill":"review","args":"secret"}},{"type":"tool_use","id":"read","name":"Read","input":{"file_path":"SKILL.md"}},{"type":"tool_use","id":"answer","name":"StructuredOutput"}]}}).to_string());
    observer.observe(AgentKind::Claude, &json!({"message":{"content":[{"type":"tool_result","tool_use_id":"skill","content":"private"},{"type":"tool_result","tool_use_id":"read","is_error":true}]}}));
    drop(observer);

    // Assert
    let events = events.lock().expect("test fixture should succeed");
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].name, "review");
    assert_eq!(events[0].kind, ActivityKind::Skill);
    assert_eq!(events[0].parent_id.as_deref(), Some("parent"));
    assert_eq!(events[1].kind, ActivityKind::Tool);
    assert_eq!(events[2].kind, ActivityKind::Skill);
    assert_eq!(events[2].status, ActivityStatus::Completed);
    assert_eq!(events[3].status, ActivityStatus::Failed);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(events[0].input.as_deref().expect("skill input"))
            .expect("JSON input"),
        json!({"args":"secret","skill":"review"})
    );
    assert_eq!(events[2].output.as_deref(), Some("private"));
}

#[test]
fn claude_excludes_structured_output_results_but_preserves_orphan_results() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer = ActivityObserver::new(move |event| {
        sink.lock().expect("events lock").push(event);
    });

    // Act
    observer.observe(AgentKind::Claude, &json!({"message":{"content":[{"type":"tool_use","id":"answer","name":"StructuredOutput"}]}}));
    for is_error in [false, true] {
        observer.observe(AgentKind::Claude, &json!({"message":{"content":[{"type":"tool_result","tool_use_id":"answer","is_error":is_error}]}}));
    }
    observer.observe(
        AgentKind::Claude,
        &json!({"message":{"content":[{"type":"tool_result","tool_use_id":"orphan"}]}}),
    );
    drop(observer);

    // Assert
    let events = events.lock().expect("events lock");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "orphan");
    assert_eq!(events[0].name, "tool");
    assert_eq!(events[0].status, ActivityStatus::Completed);
}

#[test]
fn claude_exclusions_are_bounded_without_leaking_results_at_capacity() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer = ActivityObserver::new(move |event| {
        sink.lock().expect("events lock").push(event);
    });

    // Act
    for id in [String::new(), "x".repeat(257), "invalid\n".to_string()]
        .into_iter()
        .chain((0..=MAX_ACTIVITIES).map(|index| index.to_string()))
    {
        observer.observe(
            AgentKind::Claude,
            &json!({"content":[{"type":"tool_use","id":id,"name":"StructuredOutput"}]}),
        );
        observer.observe(
            AgentKind::Claude,
            &json!({"content":[{"type":"tool_result","tool_use_id":id}]}),
        );
    }

    // Assert
    assert_eq!(observer.excluded_ids.len(), MAX_ACTIVITIES);
    assert!(observer.calls.is_empty());
    drop(observer);
    assert!(events.lock().expect("events lock").is_empty());
}

#[test]
fn activity_is_bounded_and_attempts_have_distinct_identities() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer = ActivityObserver::new(move |event| {
        sink.lock()
            .expect("test fixture should succeed")
            .push(event);
    });
    let other = ActivityObserver::new(|_| {});

    // Act
    for index in 0..=MAX_ACTIVITIES {
        observer.observe(AgentKind::Codex, &json!({"method":"item/completed","params":{"item":{"id":index.to_string(),"type":"mcpToolCall","server":"mcp","tool":"x".repeat(256)}}}));
    }

    // Assert
    assert_ne!(observer.attempt_id, other.attempt_id);
    assert_eq!(observer.calls.len(), MAX_ACTIVITIES);
    assert_eq!(
        events.lock().expect("test fixture should succeed").len(),
        MAX_ACTIVITIES
    );
    assert!(
        events
            .lock()
            .expect("test fixture should succeed")
            .iter()
            .all(|event| event.name.len() == 128)
    );
}

#[test]
fn malformed_non_tool_and_unknown_events_are_ignored() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut observer =
        ActivityObserver::new(move |event| sink.lock().expect("events lock").push(event));

    // Act
    for kind in [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Antigravity,
        AgentKind::Harness,
    ] {
        observer.observe_line(kind, "invalid");
        for event in [
            json!({}),
            json!({"method":"item/started"}),
            json!({"method":"item/started","params":{"item":{"type":"reasoning"}}}),
            json!({"method":"item/completed","params":{"item":{"type":"fileChange"}}}),
            json!({"method":"item/completed","params":{"item":{"type":"fileChange","id":""}}}),
            json!({"content":[{}, {"type":"tool_use"}, {"type":"tool_use","id":"x"}, {"type":"tool_result"}]}),
        ] {
            observer.observe(kind, &event);
        }
    }
    observer.codex_completed_turn(&json!({}));

    // Assert
    assert!(observer.calls.is_empty());
    assert!(events.lock().expect("events lock").is_empty());
}

#[test]
fn codex_terminal_variants_and_unknown_claude_results_are_retained() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for (index, item) in [
        json!({"type":"dynamicToolCall","success":false}),
        json!({"type":"mcpToolCall","error":{}}),
        json!({"type":"collabAgentToolCall","status":"cancelled"}),
        json!({"type":"imageGeneration","status":"declined"}),
        json!({"type":"dynamicToolCall","status":"denied"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut item = item;
        item["id"] = json!(index.to_string());
        observer.codex_item(&item, true);
    }
    observer.observe(AgentKind::Claude, &json!({"content":[{"type":"tool_result","tool_use_id":"orphan"},{"type":"tool_use","id":"unnamed-skill","name":"Skill"}]}));

    // Assert
    assert_eq!(observer.calls["0"].status, ActivityStatus::Failed);
    assert_eq!(observer.calls["1"].status, ActivityStatus::Failed);
    assert_eq!(observer.calls["2"].status, ActivityStatus::Interrupted);
    assert_eq!(observer.calls["3"].status, ActivityStatus::Failed);
    assert_eq!(observer.calls["4"].status, ActivityStatus::Failed);
    assert_eq!(observer.calls["orphan"].name, "tool");
    assert_eq!(observer.calls["unnamed-skill"].kind, ActivityKind::Skill);
}

#[test]
fn codex_streams_output_and_reconciles_the_final_snapshot() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    observer.codex_item(
        &json!({"type":"commandExecution","id":"cmd","command":"cargo test"}),
        false,
    );

    // Act
    for delta in ["building\n", "tests passed"] {
        observer.observe(AgentKind::Codex, &json!({"method":"item/commandExecution/outputDelta","params":{"itemId":"cmd","delta":delta}}));
    }

    // Assert
    assert_eq!(observer.calls["cmd"].input.as_deref(), Some("cargo test"));
    assert_eq!(
        observer.calls["cmd"].output.as_deref(),
        Some("building\ntests passed")
    );
    assert_eq!(observer.calls["cmd"].status, ActivityStatus::Running);

    // Act
    observer.codex_item(&json!({"type":"commandExecution","id":"cmd","aggregatedOutput":"final output","exitCode":0}), true);
    for params in [
        json!({}),
        json!({"itemId":"cmd"}),
        json!({"itemId":"missing","delta":"ignored"}),
        json!({"itemId":"cmd","delta":"late"}),
    ] {
        observer.observe(
            AgentKind::Codex,
            &json!({"method":"item/commandExecution/outputDelta","params":params}),
        );
    }

    // Assert
    assert_eq!(
        observer.calls["cmd"].output.as_deref(),
        Some("final output")
    );
    assert_eq!(observer.calls["cmd"].input.as_deref(), Some("cargo test"));
    assert_eq!(observer.calls["cmd"].exit_code, Some(0));
    assert_eq!(observer.calls.len(), 1);
}

#[test]
fn codex_tool_arguments_results_and_errors_are_projected() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    observer.codex_item(&json!({"type":"mcpToolCall","id":"mcp","server":"docs","tool":"search","arguments":{"query":"Rust"}}), false);
    observer.codex_item(&json!({"type":"mcpToolCall","id":"mcp","server":"docs","tool":"search","error":{"message":"offline"}}), true);
    observer.codex_item(&json!({"type":"dynamicToolCall","id":"dynamic","contentItems":[{"type":"text","text":"done"}]}), true);
    observer.codex_item(
        &json!({"type":"mcpToolCall","id":"result","result":{"content":"ok"}}),
        true,
    );
    observer.codex_item(
        &json!({"type":"webSearch","id":"search","query":"Rust"}),
        true,
    );

    // Assert
    assert_eq!(
        observer.calls["mcp"].input.as_deref(),
        Some(r#"{"query":"Rust"}"#)
    );
    assert_eq!(
        observer.calls["mcp"].output.as_deref(),
        Some(r#"{"message":"offline"}"#)
    );
    assert_eq!(observer.calls["dynamic"].output.as_deref(), Some("done"));
    assert_eq!(
        observer.calls["result"].output.as_deref(),
        Some(r#"{"content":"ok"}"#)
    );
    assert_eq!(observer.calls["search"].input.as_deref(), Some("Rust"));
}

#[test]
fn gemini_merges_partial_updates_and_preserves_completed_results() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"call","kind":"execute","name":"run_shell_command","status":"pending","rawInput":{"command":"pwd"}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call","content":[{"content":{"text":"working"}}]}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call","kind":"execute","title":"Finished command","status":"completed","rawOutput":{"exitCode":0,"stdout":"project"}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"other","kind":"read","status":"failed","title":"Read README.md","rawOutput":null,"content":[{"content":{"text":"missing"}}]}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"cancelled","status":"cancelled"}),
    ] {
        observer.observe(
            AgentKind::Gemini,
            &json!({"method":"session/update","params":{"update":update}}),
        );
    }

    // Assert
    assert_eq!(observer.calls["call"].name, "run_shell_command");
    assert_eq!(observer.calls["call"].kind, ActivityKind::Command);
    assert_eq!(observer.calls["call"].input.as_deref(), Some("pwd"));
    assert_eq!(
        observer.calls["call"].output.as_deref(),
        Some(r#"{"exitCode":0,"stdout":"project"}"#)
    );
    assert_eq!(observer.calls["call"].exit_code, Some(0));
    assert_eq!(observer.calls["call"].status, ActivityStatus::Completed);
    assert_eq!(
        observer.calls["other"].input.as_deref(),
        Some("Read README.md")
    );
    assert_eq!(observer.calls["other"].output.as_deref(), Some("missing"));
    assert_eq!(observer.calls["other"].status, ActivityStatus::Failed);
    assert_eq!(
        observer.calls["cancelled"].status,
        ActivityStatus::Interrupted
    );
}

#[test]
fn antigravity_projects_tool_steps_with_commands_output_and_failures() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for step in [
        json!({"step_type":"tool","step_index":1,"state":"ACTIVE","tool_name":"run_command","tool_info":{"parameters":{"CommandLine":"echo hi"}}}),
        json!({"step_type":"tool","step_index":1,"state":"DONE","tool_info":{"name":"run_command","output":"hi\n","exit_code":0}}),
        json!({"step_type":"tool","step_index":2,"state":"DONE","tool_info":{"error":{"message":"denied"}}}),
        json!({"step_type":"tool","step_index":3,"state":"ACTIVE","tool_name":"read"}),
    ] {
        observer.observe(
            AgentKind::Antigravity,
            &json!({"event":"step_update","step_update":step}),
        );
    }

    // Assert
    assert_eq!(observer.calls["1"].input.as_deref(), Some("echo hi"));
    assert_eq!(observer.calls["1"].output.as_deref(), Some("hi\n"));
    assert_eq!(observer.calls["1"].status, ActivityStatus::Completed);
    assert_eq!(observer.calls["1"].exit_code, Some(0));
    assert_eq!(observer.calls["2"].status, ActivityStatus::Failed);
    assert_eq!(
        observer.calls["2"].output.as_deref(),
        Some(r#"{"message":"denied"}"#)
    );
    assert_eq!(observer.calls["3"].status, ActivityStatus::Running);
}

#[test]
fn antigravity_emits_failed_status_for_completed_nonzero_exits_without_errors() {
    for (state, info, expected) in [
        ("DONE", json!({"exit_code":1}), ActivityStatus::Failed),
        (
            "DONE",
            json!({"exit_code":1,"error":null}),
            ActivityStatus::Failed,
        ),
        (
            "done",
            json!({"exit_code":-1,"error":null}),
            ActivityStatus::Failed,
        ),
        (
            "DONE",
            json!({"exit_code":0,"error":null}),
            ActivityStatus::Completed,
        ),
        ("DONE", json!({"error":null}), ActivityStatus::Completed),
        (
            "ACTIVE",
            json!({"exit_code":1,"error":null}),
            ActivityStatus::Running,
        ),
    ] {
        // Arrange
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut observer = ActivityObserver::new(move |event| {
            sink.lock().expect("events lock").push(event);
        });
        let mut info = info;
        info["name"] = json!("run_command");

        // Act
        observer.observe(
            AgentKind::Antigravity,
            &json!({"event":"step_update","step_update":{
                "step_type":"tool","step_index":1,"state":state,"tool_info":info
            }}),
        );

        // Assert
        let events = events.lock().expect("events lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, expected, "state={state}, info={info}");
        assert_eq!(events[0].kind, ActivityKind::Command);
        assert_eq!(events[0].exit_code, info["exit_code"].as_i64());
    }
}

#[test]
fn malformed_gemini_and_antigravity_updates_are_ignored() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for payload in [
        json!({"method":"session/update"}),
        json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk"}}}),
        json!({"method":"session/update","params":{"update":{"sessionUpdate":"tool_call"}}}),
        json!({"event":"step_update"}),
        json!({"event":"step_update","step_update":{"step_type":"agent_response"}}),
        json!({"event":"step_update","step_update":{"step_type":"tool"}}),
    ] {
        observer.observe(AgentKind::Gemini, &payload);
        observer.observe(AgentKind::Antigravity, &payload);
    }

    // Assert
    assert!(observer.calls.is_empty());
}

#[test]
fn gemini_structured_array_completion_replaces_progress_preview() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"call","status":"in_progress","content":[{"content":{"text":"working"}}]}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"call","status":"completed","rawOutput":[{"path":"README.md"}],"content":[{"content":{"text":"finished"}}]}),
    ] {
        observer.observe(
            AgentKind::Gemini,
            &json!({"method":"session/update","params":{"update":update}}),
        );
    }

    // Assert
    assert_eq!(observer.calls["call"].status, ActivityStatus::Completed);
    assert_eq!(
        observer.calls["call"].output.as_deref(),
        Some(r#"[{"path":"README.md"}]"#)
    );
}

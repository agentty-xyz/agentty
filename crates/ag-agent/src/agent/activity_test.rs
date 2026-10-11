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
fn claude_preserves_skill_and_parent_identity_without_capturing_content() {
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

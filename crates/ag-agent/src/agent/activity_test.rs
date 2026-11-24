//! Activity projection unit tests for Codex, Claude, and Antigravity tool
//! events.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{ActivityKind, ActivityStatus, DiffStat, LineRange};
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
    observer.observe(AgentKind::Claude, &json!({"type":"assistant","parent_tool_use_id":"parent","message":{"content":[{"type":"tool_use","id":"skill","name":"Skill","input":{"skill":"review","args":"secret"}},{"type":"tool_use","id":"read","name":"Read","input":{"file_path":"SKILL.md"}},{"type":"tool_use","id":"answer","name":"StructuredOutput"}]}}));
    observer.observe(AgentKind::Claude, &json!({"message":{"content":[{"type":"tool_result","tool_use_id":"skill","content":"private"},{"type":"tool_result","tool_use_id":"read","is_error":true}]}}));
    drop(observer);

    // Assert
    let events = events.lock().expect("test fixture should succeed");
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].name, "review");
    assert_eq!(events[0].kind, ActivityKind::Skill);
    assert_eq!(events[0].parent_id.as_deref(), Some("parent"));
    assert_eq!(events[1].kind, ActivityKind::Read);
    assert_eq!(events[1].facts.target.as_deref(), Some("SKILL.md"));
    assert_eq!(events[1].facts.lines, None);
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
fn claude_bash_is_classified_as_command_and_keeps_literal_input() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    let command = "{ cargo fmt; cargo test; }";

    // Act
    observer.observe(AgentKind::Claude, &json!({"type":"assistant", "message":{"content":[{"type":"tool_use", "id":"group", "name":"Bash", "input":{"command":command}}]}}));
    let running = observer.calls["group"].clone();
    observer.observe(AgentKind::Claude, &json!({"type":"user", "message":{"content":[{"type":"tool_result", "tool_use_id":"group", "is_error":true}]}}));
    let failed = &observer.calls["group"];

    // Assert
    assert_eq!(running.status, ActivityStatus::Running);
    assert_eq!(failed.status, ActivityStatus::Failed);
    for event in [&running, failed] {
        assert_eq!(event.kind, ActivityKind::Command);
        assert_eq!(event.name, "Bash");
        assert_eq!(event.input.as_deref(), Some(command));
    }
}

#[test]
fn claude_oversized_edits_and_writes_preserve_paths_through_failure_updates() {
    // Arrange, Act, Assert
    for name in ["Edit", "Write"] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut observer = ActivityObserver::new(move |event| {
            sink.lock().expect("events lock").push(event);
        });
        let input = if name == "Edit" {
            json!({"file_path":"src/example.rs", "old_string":"old", "new_string":"x".repeat(8192)})
        } else {
            json!({"file_path":"src/example.rs", "content":"x".repeat(8192)})
        };
        observer.observe(AgentKind::Claude, &json!({"type":"assistant", "message":{"content":[{"type":"tool_use", "id":"edit", "name":name, "input":input}]}}));
        observer.observe(AgentKind::Claude, &json!({"type":"user", "message":{"content":[{"type":"tool_result", "tool_use_id":"edit", "is_error":true, "content":"edit failed"}]}}));
        drop(observer);

        let events = events.lock().expect("events lock");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].status, ActivityStatus::Running);
        assert_eq!(events[1].status, ActivityStatus::Failed);
        for event in events.iter() {
            assert_eq!(event.name, name);
            assert_eq!(event.kind, ActivityKind::FileChange);
            assert_eq!(event.input.as_deref(), Some("src/example.rs"));
            assert_eq!(event.facts.target.as_deref(), Some("src/example.rs"));
            assert_eq!(
                event.facts.diff_stat,
                Some(DiffStat {
                    added: 1,
                    removed: u64::from(name == "Edit")
                })
            );
        }
        assert_eq!(events[1].output.as_deref(), Some("edit failed"));
    }
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
        AgentKind::Antigravity,
        AgentKind::Harness,
    ] {
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
    assert_eq!(observer.calls["mcp"].kind, ActivityKind::Mcp);
    assert_eq!(observer.calls["mcp"].name, "docs/search");
    assert_eq!(observer.calls["dynamic"].kind, ActivityKind::Tool);
}

#[test]
fn claude_mcp_tools_are_classified_by_server_and_tool() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    observer.observe(AgentKind::Claude, &json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"mcp","name":"mcp__posthog__query","input":{"query":"SELECT 1"}},{"type":"tool_use","id":"plain","name":"mcp__unqualified","input":{}}]}}));
    observer.observe(
        AgentKind::Claude,
        &json!({"message":{"content":[{"type":"tool_result","tool_use_id":"mcp","content":"ok"}]}}),
    );

    // Assert
    assert_eq!(observer.calls["mcp"].kind, ActivityKind::Mcp);
    assert_eq!(observer.calls["mcp"].name, "posthog/query");
    assert_eq!(observer.calls["mcp"].status, ActivityStatus::Completed);
    assert_eq!(observer.calls["plain"].kind, ActivityKind::Tool);
    assert_eq!(observer.calls["plain"].name, "mcp__unqualified");
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
fn malformed_antigravity_updates_are_ignored() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for payload in [
        json!({"event":"step_update"}),
        json!({"event":"step_update","step_update":{"step_type":"agent_response"}}),
        json!({"event":"step_update","step_update":{"step_type":"tool"}}),
    ] {
        observer.observe(AgentKind::Antigravity, &payload);
    }

    // Assert
    assert!(observer.calls.is_empty());
}

#[test]
fn claude_exploration_tools_report_kinds_targets_and_read_ranges() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    let blocks = json!([
        {"type":"tool_use","id":"read","name":"Read","input":{"file_path":"src/a.rs","offset":1129,"limit":100}},
        {"type":"tool_use","id":"grep","name":"Grep","input":{"pattern":"fn render","path":"src"}},
        {"type":"tool_use","id":"glob","name":"Glob","input":{"pattern":"**/*.rs"}},
        {"type":"tool_use","id":"ls","name":"LS","input":{"path":"crates"}},
        {"type":"tool_use","id":"multi","name":"MultiEdit","input":{"file_path":"src/b.rs","edits":[
            {"old_string":"a\nb","new_string":"c"},
            {"old_string":"d","new_string":"e\nf\ng"}
        ]}},
        {"type":"tool_use","id":"multi-empty","name":"MultiEdit","input":{"file_path":"src/c.rs"}}
    ]);

    // Act
    observer.observe(
        AgentKind::Claude,
        &json!({"type":"assistant","message":{"content":blocks}}),
    );

    // Assert
    let call = |id: &str| {
        let call = &observer.calls[id];

        (call.kind, call.facts.target.as_deref())
    };
    assert_eq!(call("read"), (ActivityKind::Read, Some("src/a.rs")));
    assert_eq!(
        observer.calls["read"].facts.lines,
        Some(LineRange {
            first: 1129,
            last: Some(1228)
        })
    );
    assert_eq!(call("grep"), (ActivityKind::Search, Some("fn render")));
    assert_eq!(call("glob"), (ActivityKind::List, Some("**/*.rs")));
    assert_eq!(call("ls"), (ActivityKind::List, Some("crates")));
    assert_eq!(call("multi"), (ActivityKind::FileChange, Some("src/b.rs")));
    assert_eq!(
        observer.calls["multi"].facts.diff_stat,
        Some(DiffStat {
            added: 4,
            removed: 3
        })
    );
    assert_eq!(observer.calls["multi-empty"].facts.diff_stat, None);
}

#[test]
fn codex_command_actions_report_exploration_kinds_and_durations() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    let items = [
        json!({"id":"read","type":"commandExecution","command":"sed -n 1,20p src/a.rs","durationMs":36000,"exitCode":0,
            "commandActions":[{"type":"read","command":"sed","name":"a.rs","path":"src/a.rs"}]}),
        json!({"id":"search","type":"commandExecution","command":"rg -n render src",
            "commandActions":[{"type":"search","command":"rg","query":"render","path":"src"}]}),
        json!({"id":"list","type":"commandExecution","command":"ls crates",
            "commandActions":[{"type":"listFiles","command":"ls","path":"crates"}]}),
        json!({"id":"list-root","type":"commandExecution","command":"ls",
            "commandActions":[{"type":"listFiles","command":"ls","path":null}]}),
        json!({"id":"mixed","type":"commandExecution","command":"cat a.rs | rg x",
            "commandActions":[{"type":"read","command":"cat","name":"a.rs","path":"a.rs"},{"type":"search","command":"rg","query":"x","path":null}]}),
        json!({"id":"build","type":"commandExecution","command":"cargo build\n--release",
            "commandActions":[{"type":"unknown","command":"cargo build"}]}),
    ];

    // Act
    for item in &items {
        observer.observe(
            AgentKind::Codex,
            &json!({"method":"item/completed","params":{"item":item}}),
        );
    }

    // Assert
    let call = |id: &str| {
        let call = &observer.calls[id];

        (call.kind, call.facts.target.as_deref())
    };
    assert_eq!(call("read"), (ActivityKind::Read, Some("src/a.rs")));
    assert_eq!(
        observer.calls["read"].facts.duration,
        Some(Duration::from_secs(36))
    );
    assert_eq!(call("search"), (ActivityKind::Search, Some("render")));
    assert_eq!(call("list"), (ActivityKind::List, Some("crates")));
    assert_eq!(call("list-root"), (ActivityKind::List, Some("ls")));
    assert_eq!(
        call("mixed"),
        (ActivityKind::Command, Some("cat a.rs | rg x"))
    );
    assert_eq!(call("build"), (ActivityKind::Command, Some("cargo build")));
    assert_eq!(observer.calls["build"].facts.duration, None);
}

#[test]
fn codex_output_deltas_keep_an_exploration_kind() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    let started = json!({"method":"item/started","params":{"item":{"id":"search","type":"commandExecution","command":"rg -n render src",
        "commandActions":[{"type":"search","command":"rg","query":"render","path":"src"}]}}});

    // Act
    observer.observe(AgentKind::Codex, &started);
    observer.observe(
        AgentKind::Codex,
        &json!({"method":"item/commandExecution/outputDelta","params":{"itemId":"search","delta":"src/a.rs:1: render"}}),
    );

    // Assert
    let call = &observer.calls["search"];
    assert_eq!(call.kind, ActivityKind::Search);
    assert_eq!(call.output.as_deref(), Some("src/a.rs:1: render"));
}

#[test]
fn codex_file_changes_report_line_counts_and_their_first_path() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});
    let changes = json!([
        {"path":"src/new.rs","kind":{"type":"add"},"diff":"one\ntwo\n"},
        {"path":"src/old.rs","kind":{"type":"delete"},"diff":"gone\n"},
        {"path":"src/lib.rs","kind":{"type":"update","move_path":null},
            "diff":"--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n+more\n context\n"}
    ]);

    // Act
    observer.observe(
        AgentKind::Codex,
        &json!({"method":"item/completed","params":{"item":{"id":"patch","type":"fileChange","status":"completed","changes":changes}}}),
    );
    observer.observe(
        AgentKind::Codex,
        &json!({"method":"item/started","params":{"item":{"id":"bare","type":"fileChange"}}}),
    );

    // Assert
    let patch = &observer.calls["patch"];
    assert_eq!(patch.kind, ActivityKind::FileChange);
    assert_eq!(patch.facts.target.as_deref(), Some("src/new.rs"));
    assert_eq!(
        patch.facts.diff_stat,
        Some(DiffStat {
            added: 4,
            removed: 2
        })
    );
    assert_eq!(observer.calls["bare"].facts.diff_stat, None);
    assert_eq!(observer.calls["bare"].facts.target, None);
}

#[test]
fn antigravity_exploration_tools_report_their_kinds() {
    // Arrange
    let mut observer = ActivityObserver::new(|_| {});

    // Act
    for (index, name) in ["view_file", "grep_search", "list_dir"].iter().enumerate() {
        observer.observe(
            AgentKind::Antigravity,
            &json!({"event":"step_update","step_update":{"step_type":"tool","step_index":index,"state":"ACTIVE","tool_name":name}}),
        );
    }

    // Assert
    assert_eq!(observer.calls["0"].kind, ActivityKind::Read);
    assert_eq!(observer.calls["1"].kind, ActivityKind::Search);
    assert_eq!(observer.calls["2"].kind, ActivityKind::List);
}

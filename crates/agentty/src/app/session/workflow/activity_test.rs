use std::time::SystemTime;

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus};
use tokio::sync::mpsc;

use super::{ActivityState, TurnActivity};
use crate::app::AppEvent;

#[test]
fn summary_counts_invocations_not_updates_and_separates_attempts_and_skills() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = ActivityEvent {
        attempt_id: "one".into(),
        exit_code: None,
        id: "call".into(),
        input: None,
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Running,
    };

    // Act
    activity.observe(event.clone());
    activity.observe(ActivityEvent {
        status: ActivityStatus::Completed,
        ..event.clone()
    });
    activity.observe(ActivityEvent {
        attempt_id: "retry".into(),
        status: ActivityStatus::Failed,
        ..event.clone()
    });
    activity.observe(ActivityEvent {
        id: "skill".into(),
        kind: ActivityKind::Skill,
        name: "review".into(),
        ..event
    });

    // Assert
    assert_eq!(
        activity.summary(),
        "Tools: Read ×2 (1 failed)\nSkills: review ×1 (1 interrupted)\n✗ Read\n"
    );
    assert_eq!(TurnActivity::default().summary(), "");
}

#[test]
fn summary_retains_failed_command_count_after_live_rows_clear() {
    // Arrange
    let mut activity = TurnActivity::default();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let session_id = "session".into();
    let event = ActivityEvent {
        attempt_id: "antigravity-attempt".into(),
        exit_code: Some(1),
        id: "1".into(),
        input: None,
        kind: ActivityKind::Command,
        name: "run_command".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Failed,
    };

    // Act
    activity.observe_live(event, &sender, &session_id);
    activity.finish_live(&sender, &session_id);
    activity.clear_live(&sender, &session_id);
    let last_event = std::iter::from_fn(|| receiver.try_recv().ok()).last();

    // Assert
    assert_eq!(
        last_event,
        Some(AppEvent::SessionActivityUpdated {
            activity: None,
            session_id,
        })
    );
    assert_eq!(
        activity.summary(),
        "Tools: run_command ×1 (1 failed)\n✗ run_command (exit 1)\n"
    );
}

#[test]
fn summary_bounds_unique_calls_but_accepts_terminal_updates() {
    // Arrange
    let mut activity = TurnActivity::default();
    let mut event = ActivityEvent {
        attempt_id: "one".into(),
        exit_code: None,
        id: String::new(),
        input: None,
        kind: ActivityKind::Command,
        name: "shell".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Interrupted,
    };

    // Act
    for index in 0..2049 {
        event.id = index.to_string();
        activity.observe(event.clone());
    }
    event.id = "0".into();
    event.status = ActivityStatus::Completed;
    activity.observe(event);

    // Assert
    assert_eq!(
        activity.summary(),
        "Tools: shell ×2048 (2047 interrupted)\n"
    );
}

#[test]
fn live_rows_keep_invocation_order_update_status_and_clear() {
    // Arrange
    let mut activity = TurnActivity::default();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let session_id = "session".into();
    let event = ActivityEvent {
        attempt_id: "one".into(),
        exit_code: None,
        id: "z".into(),
        input: None,
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Running,
    };

    // Act
    activity.clear_live(&sender, &session_id);
    activity.observe_live(event.clone(), &sender, &session_id);
    activity.observe_live(
        ActivityEvent {
            id: "a".into(),
            kind: ActivityKind::Skill,
            name: "review".into(),
            ..event.clone()
        },
        &sender,
        &session_id,
    );
    activity.observe_live(
        ActivityEvent {
            status: ActivityStatus::Completed,
            ..event.clone()
        },
        &sender,
        &session_id,
    );
    activity.observe_live(
        ActivityEvent {
            attempt_id: "retry".into(),
            status: ActivityStatus::Failed,
            ..event
        },
        &sender,
        &session_id,
    );
    activity.finish_live(&sender, &session_id);
    activity.finish_live(&sender, &session_id);
    activity.clear_live(&sender, &session_id);
    let events = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();

    // Assert
    assert_eq!(events.len(), 2);
    let AppEvent::SessionActivityReady {
        activity: snapshot,
        session_id: actual_id,
    } = &events[0]
    else {
        unreachable!("expected activity notification");
    };
    assert_eq!(actual_id, &session_id);
    assert_eq!(
        snapshot.snapshot(),
        "Tools: Read ×2\nSkills: review ×1\nStatus: 1 succeeded · 1 failed · 0 running · 1 \
         interrupted\n\n✗ Read\n! skill review (interrupted)\n✓ Read\n"
    );
    assert_eq!(
        events[1],
        AppEvent::SessionActivityUpdated {
            activity: None,
            session_id
        }
    );
}

#[test]
fn compact_failures_show_exit_codes_literal_input_and_two_output_lines() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: Some(1),
        id: "call".into(),
        input: Some("rg '**example**' README.md".into()),
        kind: ActivityKind::Command,
        name: "shell".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: Some("line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n".into()),
        parent_id: None,
        status: ActivityStatus::Failed,
    };

    // Act
    activity.observe(event.clone());
    let text = activity.live.snapshot();
    activity.observe(ActivityEvent {
        input: Some(String::new()),
        output: None,
        ..event
    });

    // Assert
    assert_eq!(
        text,
        "Tools: shell ×1\nStatus: 0 succeeded · 1 failed · 0 running\n\n✗ shell (exit 1) · rg \
         '**example**' README.md\n  line 1\n  line 2 …\n"
    );
    assert_eq!(
        activity.live.snapshot(),
        "Tools: shell ×1\nStatus: 0 succeeded · 1 failed · 0 running\n\n✗ shell (exit 1)\n"
    );
    assert_eq!(
        activity.summary(),
        "Tools: shell ×1 (1 failed)\n✗ shell (exit 1)\n"
    );
}

#[test]
fn live_activity_bursts_bound_pending_snapshots_and_rearm_after_consumption() {
    // Arrange
    let mut activity = TurnActivity::default();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let session_id = "session".into();
    let mut event = ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: None,
        id: "call".into(),
        input: None,
        kind: ActivityKind::Tool,
        name: "Read".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: Some("x".repeat(4096)),
        parent_id: None,
        status: ActivityStatus::Running,
    };

    // Act: retain many previews, then update them while the foreground is busy.
    for index in 0..5000 {
        event.id = (index % 100).to_string();
        activity.observe_live(event.clone(), &sender, &session_id);
    }
    event.status = ActivityStatus::Failed;
    event.output = Some("final result".into());
    activity.observe_live(event.clone(), &sender, &session_id);
    activity.finish_live(&sender, &session_id);

    // Assert: a single notification reads the latest state, including
    // terminals.
    assert_eq!(receiver.len(), 1);
    let AppEvent::SessionActivityReady {
        activity: snapshot, ..
    } = receiver.try_recv().expect("notification")
    else {
        unreachable!("expected activity notification");
    };
    assert_eq!(snapshot, activity.live);
    assert_ne!(snapshot, TurnActivity::default().live);
    let text = snapshot.snapshot();
    assert!(text.starts_with(
        "Tools: Read ×100\nStatus: 0 succeeded · 1 failed · 0 running · 99 interrupted\n"
    ));
    assert_eq!(text.matches("! Read (interrupted)").count(), 3);
    assert!(!text.contains("final result"));
    assert!(activity.summary().ends_with("✗ Read\n  final result\n"));
    assert!(text.ends_with("+97 earlier failed or interrupted calls\n"));

    // Act: reading rearms the mailbox; the next turn follows the clear.
    activity.observe_live(event.clone(), &sender, &session_id);
    assert_eq!(receiver.len(), 1);
    assert!(matches!(
        receiver.try_recv().expect("rearmed"),
        AppEvent::SessionActivityReady { .. }
    ));
    snapshot.snapshot();
    activity.clear_live(&sender, &session_id);
    let mut next_turn = TurnActivity::default();
    next_turn.observe_live(event, &sender, &session_id);

    // Assert: clear remains ahead of the next notification, and reads rearm it.
    assert_eq!(receiver.len(), 2);
    assert!(matches!(
        receiver.try_recv().expect("clear"),
        AppEvent::SessionActivityUpdated { activity: None, .. }
    ));
    assert!(matches!(
        receiver.try_recv().expect("rearmed"),
        AppEvent::SessionActivityReady { .. }
    ));
    drop(receiver);
    snapshot.snapshot();
    activity.live.notify(&sender, &session_id);
    assert!(!activity.live.state.lock().expect("state").pending);
}

#[test]
fn compact_rows_hide_successful_output_and_flatten_multiline_commands() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: None,
        id: "call".into(),
        input: Some("  cargo\n test\t --workspace  ".into()),
        kind: ActivityKind::Command,
        name: "shell".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: Some("verbose successful output\nmore output".into()),
        parent_id: None,
        status: ActivityStatus::Running,
    };

    // Act, Assert
    for (status, exit_code, expected) in [
        (
            ActivityStatus::Running,
            None,
            "Status: 0 succeeded · 0 failed · 1 running\n\n… shell · cargo test --workspace\n",
        ),
        (
            ActivityStatus::Completed,
            Some(0),
            "Status: 1 succeeded · 0 failed · 0 running\n\n✓ shell · cargo test --workspace\n",
        ),
        (
            ActivityStatus::Interrupted,
            None,
            "Status: 0 succeeded · 0 failed · 0 running · 1 interrupted\n\n! shell (interrupted) \
             · cargo test --workspace\n",
        ),
        (
            ActivityStatus::Completed,
            Some(2),
            "Status: 0 succeeded · 1 failed · 0 running\n\n✗ shell (exit 2) · cargo test \
             --workspace\n  verbose successful output\n  more output\n",
        ),
    ] {
        activity.observe(ActivityEvent {
            status,
            exit_code,
            ..event.clone()
        });
        assert_eq!(
            activity.live.snapshot(),
            format!("Tools: shell ×1\n{expected}")
        );
    }
    activity.observe(ActivityEvent {
        kind: ActivityKind::Skill,
        name: "review".into(),
        input: Some(r#"{"skill":"review"}"#.into()),
        status: ActivityStatus::Completed,
        ..event
    });
    assert_eq!(
        activity.live.snapshot(),
        "Skills: review ×1\nStatus: 1 succeeded · 0 failed · 0 running\n\n✓ skill review\n"
    );
}

#[test]
fn compact_failures_skip_empty_lines_and_mark_only_omitted_output() {
    // Arrange, Act, Assert
    for (output, expected) in [
        ("", ""),
        (" \n\t", ""),
        ("\n first error \n\n", "  first error\n"),
        ("one\n\ntwo\n\n", "  one\n  two\n"),
        ("one\ntwo\nthree", "  one\n  two …\n"),
    ] {
        let mut text = String::new();
        ActivityState::append_failure_preview(&mut text, output);
        assert_eq!(text, expected);
    }
}

#[test]
fn rolling_activity_tracks_completion_order_and_bounds_parallel_rows() {
    // Arrange
    let mut activity = TurnActivity::default();
    let event = activity_event();
    assert_eq!(activity.live.snapshot(), "");

    // Act
    for index in 0..6 {
        activity.observe(ActivityEvent {
            id: index.to_string(),
            input: Some(format!("command-{index}")),
            ..event.clone()
        });
    }
    let parallel = activity.live.snapshot();
    for index in [3, 1, 5, 0] {
        activity.observe(ActivityEvent {
            id: index.to_string(),
            input: Some(format!("command-{index}")),
            status: ActivityStatus::Completed,
            ..event.clone()
        });
    }
    // A repeated terminal snapshot must not replace the last completed call.
    activity.observe(ActivityEvent {
        id: "5".into(),
        status: ActivityStatus::Completed,
        ..event.clone()
    });
    let rolling = activity.live.snapshot();
    activity.observe(ActivityEvent {
        id: "2".into(),
        status: ActivityStatus::Failed,
        output: Some("a retained failure".into()),
        ..event.clone()
    });
    activity.observe(ActivityEvent {
        id: "4".into(),
        status: ActivityStatus::Completed,
        ..event
    });

    // Assert
    assert_eq!(parallel.matches("… shell").count(), 3);
    assert!(parallel.ends_with("+3 more running\n"));
    assert!(rolling.contains("Status: 4 succeeded · 0 failed · 2 running"));
    assert!(rolling.contains("✓ shell · command-0\n… shell · command-2\n… shell · command-4"));
    assert_eq!(rolling.matches("✓ shell").count(), 1);
    let settled = activity.live.snapshot();
    assert!(settled.contains("Status: 5 succeeded · 1 failed · 0 running"));
    assert!(settled.contains("✗ shell\n  a retained failure\n✓ shell\n"));
    assert!(
        activity
            .summary()
            .ends_with("✗ shell\n  a retained failure\n")
    );
}

#[test]
fn rolling_activity_bounds_issues_by_transition_order_and_saves_failures() {
    for final_status in [ActivityStatus::Failed, ActivityStatus::Interrupted] {
        // Arrange
        let mut activity = TurnActivity::default();
        let event = activity_event();
        for index in 0..4 {
            activity.observe(ActivityEvent {
                id: index.to_string(),
                ..event.clone()
            });
        }

        // Act: B, C, D settle first, then the earlier invocation A.
        for index in [1, 2, 3, 0] {
            activity.observe(ActivityEvent {
                id: index.to_string(),
                input: Some(format!("command-{index}")),
                status: if index == 0 {
                    final_status
                } else {
                    ActivityStatus::Failed
                },
                output: Some(format!("error-{index}")),
                ..event.clone()
            });
        }
        // A result preview update must not promote the older failure B.
        activity.observe(ActivityEvent {
            id: "1".into(),
            input: Some("command-1".into()),
            status: ActivityStatus::Failed,
            output: Some("updated error-1".into()),
            ..event
        });
        let live = activity.live.snapshot();
        let summary = activity.summary();

        // Assert
        assert!(!live.contains("command-1"));
        assert!(!live.contains("error-1"));
        assert!(live.find("command-2") < live.find("command-3"));
        assert!(live.find("command-3") < live.find("command-0"));
        assert!(live.ends_with("+1 earlier failed or interrupted calls\n"));
        assert!(summary.contains("updated error-1"));
        if final_status == ActivityStatus::Failed {
            assert_eq!(live.matches("✗ shell").count(), 3);
            assert!(live.contains("error-0"));
            assert_eq!(summary.matches("✗ shell").count(), 4);
        } else {
            assert!(live.contains("! shell (interrupted) · command-0"));
            assert_eq!(summary.matches("✗ shell").count(), 3);
        }
    }
}

#[test]
fn readable_inputs_extract_useful_fields_and_hide_payloads() {
    // Arrange, Act, Assert
    for (input, expected) in [
        ("cargo\n test", Some("cargo test")),
        ("true", Some("true")),
        ("42", Some("42")),
        (r#"{"file_path":"src/\u001bmain.rs"}"#, Some("src/main.rs")),
        (
            r#"{"command":"cargo test","description":"hidden"}"#,
            Some("cargo test"),
        ),
        (r#"{"CommandLine":"pwd"}"#, Some("pwd")),
        (
            r#"{"file_path":"src/main.rs","content":"hidden"}"#,
            Some("src/main.rs"),
        ),
        (r#"{"path":"README.md"}"#, Some("README.md")),
        (r#"{"TargetFile":"src/main.rs"}"#, Some("src/main.rs")),
        (r#"{"target_file":"src/main.rs"}"#, Some("src/main.rs")),
        (r#"{"query":"search term"}"#, Some("search term")),
        (r#"{"pattern":"fn main"}"#, Some("fn main")),
        (r#""quoted input""#, Some("quoted input")),
        (r#"{"content":"hidden"}"#, None),
        (r#"{"file_path":"truncated"#, None),
        (r#"[{"content":"hidden"}]"#, None),
    ] {
        assert_eq!(
            ActivityState::input_preview(input, ActivityKind::Tool).as_deref(),
            expected,
            "{input}"
        );
    }
}

#[test]
fn command_previews_preserve_shell_syntax_in_live_and_saved_rows() {
    for (input, expected) in [
        ("{ cargo fmt; cargo test; }", "{ cargo fmt; cargo test; }"),
        (
            "{\n cargo fmt;\n cargo test;\n}",
            "{ cargo fmt; cargo test; }",
        ),
        (r#""cargo""#, r#""cargo""#),
    ] {
        // Arrange
        let mut activity = TurnActivity::default();
        let event = ActivityEvent {
            input: Some(input.into()),
            ..activity_event()
        };

        // Act
        activity.observe(event.clone());
        let running = activity.live.snapshot();
        activity.observe(ActivityEvent {
            status: ActivityStatus::Failed,
            ..event
        });

        // Assert
        assert!(running.contains(&format!("… shell · {expected}\n")));
        let failure = format!("✗ shell · {expected}\n");
        assert!(activity.live.snapshot().contains(&failure));
        assert!(activity.summary().ends_with(&failure));
    }
}

fn activity_event() -> ActivityEvent {
    ActivityEvent {
        attempt_id: "attempt".into(),
        exit_code: None,
        id: "call".into(),
        input: None,
        kind: ActivityKind::Command,
        name: "shell".into(),
        observed_at: SystemTime::UNIX_EPOCH,
        output: None,
        parent_id: None,
        status: ActivityStatus::Running,
    }
}

use ag_session::{
    AgentKind, AgentModel, AgentSelection, CreateSessionMode, ForgeKind, ReasoningLevel,
    ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionDiffFile,
    SessionDiffFileStatus, SessionId, SessionListEntry, SessionListStats, SessionStatus,
    SessionSummary,
};
use serde_json::json;

use crate::protocol::{
    ClientFrame, MAX_DIFF_RESPONSE_BYTES, RemoteDiffFile, RemoteMessage, RemoteMessageKind,
    RemoteQuestion, RemoteQuestionAnswer, RemoteRequest, RemoteResponse, RemoteResult,
    RemoteReviewReadiness, RemoteSession, RemoteSessionMode, RemoteSessionSummary, ServerFrame,
};
use crate::test_support::{host_fixture, session_fixture};

#[test]
fn client_frames_use_snake_case_type_and_method_tags() {
    // Arrange
    let wire = json!([
        {"type": "hello", "protocol_version": 1, "token": "secret"},
        {"type": "pair", "protocol_version": 1, "code": "ABCD-EFGH", "device_name": "Phone"},
        {"type": "request", "id": 1, "request": {"method": "list_sessions"}},
        {"type": "request", "id": 2, "request": {"method": "get_session", "session_id": "s"}},
        {"type": "request", "id": 3, "request": {
            "method": "send_message", "session_id": "s", "message": "hi"
        }},
        {"type": "request", "id": 4, "request": {
            "method": "answer_questions",
            "session_id": "s",
            "answers": [{"question": "Which?", "answer": "main"}]
        }},
        {"type": "request", "id": 5, "request": {"method": "cancel_session", "session_id": "s"}},
        {"type": "request", "id": 6, "request": {"method": "merge_session", "session_id": "s"}},
        {"type": "request", "id": 7, "request": {"method": "stop_turn", "session_id": "s"}},
        {"type": "request", "id": 8, "request": {"method": "sync_session", "session_id": "s"}}
    ]);

    // Act
    let frames: Vec<ClientFrame> =
        serde_json::from_value(wire.clone()).expect("client frames should parse");
    let encoded = serde_json::to_value(&frames).expect("client frames should encode");

    // Assert
    assert_eq!(
        frames,
        [
            ClientFrame::Hello {
                protocol_version: 1,
                token: "secret".to_string(),
            },
            ClientFrame::Pair {
                code: "ABCD-EFGH".to_string(),
                device_name: "Phone".to_string(),
                protocol_version: 1,
            },
            ClientFrame::Request {
                id: 1,
                request: RemoteRequest::ListSessions,
            },
            ClientFrame::Request {
                id: 2,
                request: RemoteRequest::GetSession {
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 3,
                request: RemoteRequest::SendMessage {
                    message: "hi".to_string(),
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 4,
                request: RemoteRequest::AnswerQuestions {
                    answers: vec![RemoteQuestionAnswer {
                        answer: "main".to_string(),
                        question: "Which?".to_string(),
                    }],
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 5,
                request: RemoteRequest::CancelSession {
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 6,
                request: RemoteRequest::MergeSession {
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 7,
                request: RemoteRequest::StopTurn {
                    session_id: "s".to_string(),
                },
            },
            ClientFrame::Request {
                id: 8,
                request: RemoteRequest::SyncSession {
                    session_id: "s".to_string(),
                },
            },
        ]
    );
    assert_eq!(encoded, wire);
}

#[test]
fn unknown_methods_parse_as_unsupported_requests() {
    // Arrange
    let with_fields = json!({"type": "request", "id": 1, "request": {
        "method": "fork_session", "session_id": "s"
    }});
    let bare = json!({"type": "request", "id": 2, "request": {"method": "list_projects"}});

    // Act
    let with_fields = serde_json::from_value::<ClientFrame>(with_fields);
    let bare = serde_json::from_value::<ClientFrame>(bare);

    // Assert
    assert_eq!(
        with_fields.expect("unknown method should parse"),
        ClientFrame::Request {
            id: 1,
            request: RemoteRequest::Unsupported,
        }
    );
    assert_eq!(
        bare.expect("unknown method should parse"),
        ClientFrame::Request {
            id: 2,
            request: RemoteRequest::Unsupported,
        }
    );
}

#[test]
fn known_methods_with_malformed_fields_stay_invalid() {
    // Arrange
    let missing_field = json!({"type": "request", "id": 1, "request": {"method": "get_session"}});
    let missing_method = json!({"type": "request", "id": 2, "request": {"session_id": "s"}});

    // Act
    let missing_field = serde_json::from_value::<ClientFrame>(missing_field);
    let missing_method = serde_json::from_value::<ClientFrame>(missing_method);

    // Assert
    assert!(missing_field.is_err());
    assert!(missing_method.is_err());
}

#[test]
fn authentication_frames_list_capabilities_and_accept_frames_without_them() {
    // Arrange
    let listed = ServerFrame::Welcome {
        capabilities: vec!["stop_turn".to_string()],
        device_id: "d".to_string(),
        protocol_version: 1,
        revision: 3,
    };
    let old_paired =
        json!({"type": "paired", "device_id": "d", "protocol_version": 1, "revision": 3});

    // Act
    let encoded = serde_json::to_value(&listed).expect("welcome should encode");
    let decoded: ServerFrame =
        serde_json::from_value(encoded.clone()).expect("welcome should decode");
    let old_paired: ServerFrame =
        serde_json::from_value(old_paired).expect("old paired frame should decode");

    // Assert
    assert_eq!(
        encoded,
        json!({
            "type": "welcome",
            "capabilities": ["stop_turn"],
            "device_id": "d",
            "protocol_version": 1,
            "revision": 3
        })
    );
    assert_eq!(decoded, listed);
    assert_eq!(
        old_paired,
        ServerFrame::Paired {
            capabilities: Vec::new(),
            device_id: "d".to_string(),
            protocol_version: 1,
            revision: 3,
            token: String::new(),
        }
    );
}

#[test]
fn server_frames_encode_results_as_ok_or_error_objects() {
    // Arrange
    let frames = vec![
        ServerFrame::Welcome {
            capabilities: Vec::new(),
            device_id: "d".to_string(),
            protocol_version: 1,
            revision: 3,
        },
        ServerFrame::Paired {
            capabilities: Vec::new(),
            device_id: "d".to_string(),
            protocol_version: 1,
            revision: 3,
            token: "t".to_string(),
        },
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::Accepted),
        },
        ServerFrame::Response {
            id: 2,
            result: RemoteResult::Error("Session not found".to_string()),
        },
        ServerFrame::Response {
            id: 3,
            result: RemoteResult::Ok(RemoteResponse::Session { session: None }),
        },
        ServerFrame::Response {
            id: 4,
            result: RemoteResult::Ok(RemoteResponse::Sessions {
                sessions: Vec::new(),
            }),
        },
        ServerFrame::Changed { revision: 4 },
        ServerFrame::Rejected {
            reason: "no".to_string(),
        },
        ServerFrame::AwaitingApproval {
            confirmation: "ABCD".to_string(),
        },
    ];

    // Act
    let encoded = serde_json::to_value(&frames).expect("server frames should encode");
    let decoded: Vec<ServerFrame> =
        serde_json::from_value(encoded.clone()).expect("server frames should decode");

    // Assert
    assert_eq!(
        encoded,
        json!([
            {"type": "welcome", "device_id": "d", "protocol_version": 1, "revision": 3},
            {"type": "paired", "device_id": "d", "protocol_version": 1, "revision": 3, "token": "t"},
            {"type": "response", "id": 1, "result": {"ok": {"kind": "accepted"}}},
            {"type": "response", "id": 2, "result": {"error": "Session not found"}},
            {"type": "response", "id": 3, "result": {"ok": {"kind": "session", "session": null}}},
            {"type": "response", "id": 4, "result": {"ok": {"kind": "sessions", "sessions": []}}},
            {"type": "changed", "revision": 4},
            {"type": "rejected", "reason": "no"},
            {"type": "awaiting_approval", "confirmation": "ABCD"}
        ])
    );
    assert_eq!(decoded, frames);
}

#[test]
fn session_creation_frames_default_to_regular_mode() {
    // Arrange
    let requests = json!([
        {"method": "get_host"},
        {"method": "create_session"},
        {"method": "create_session", "mode": "harness"}
    ]);
    let responses = vec![
        RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            project: "agentty".to_string(),
            session_modes: vec![RemoteSessionMode::Regular, RemoteSessionMode::Harness],
        },
        RemoteResponse::Created {
            session_id: "s".to_string(),
        },
    ];

    // Act
    let decoded: Vec<RemoteRequest> =
        serde_json::from_value(requests).expect("requests should parse");
    let encoded = serde_json::to_value(&responses).expect("responses should encode");

    // Assert
    assert_eq!(
        decoded,
        [
            RemoteRequest::GetHost,
            RemoteRequest::CreateSession {
                mode: RemoteSessionMode::Regular,
            },
            RemoteRequest::CreateSession {
                mode: RemoteSessionMode::Harness,
            },
        ]
    );
    assert_eq!(
        encoded,
        json!([
            {
                "kind": "host",
                "base_branch": "main",
                "default_model": "gpt-6.1-sol",
                "project": "agentty",
                "session_modes": ["regular", "harness"]
            },
            {"kind": "created", "session_id": "s"}
        ])
    );
}

#[test]
fn host_context_converts_modes_remote_devices_may_request() {
    // Arrange
    let mut host = host_fixture();
    host.session_modes = vec![
        CreateSessionMode::Draft,
        CreateSessionMode::Harness,
        CreateSessionMode::Regular,
    ];

    // Act
    let response = RemoteResponse::from(host);

    // Assert
    assert_eq!(
        response,
        RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            project: "agentty".to_string(),
            session_modes: vec![RemoteSessionMode::Harness, RemoteSessionMode::Regular],
        }
    );
    assert_eq!(
        RemoteSessionMode::Harness.session_mode(),
        CreateSessionMode::Harness
    );
}

#[test]
fn session_summary_converts_status_to_display_string() {
    // Arrange
    let summary = SessionSummary {
        id: SessionId::from("session-1"),
        status: SessionStatus::InProgress,
        title: None,
        updated_at: 5,
    };

    // Act
    let remote = RemoteSessionSummary::from(SessionListEntry::from(summary));
    let encoded = serde_json::to_value(&remote).expect("summary should encode");

    // Assert
    assert_eq!(
        remote,
        RemoteSessionSummary {
            id: "session-1".to_string(),
            status: "InProgress".to_string(),
            title: None,
            updated_at: 5,
            ..RemoteSessionSummary::default()
        }
    );
    assert_eq!(
        encoded,
        json!({"id": "session-1", "status": "InProgress", "title": null, "updated_at": 5})
    );
}

/// Builds a list entry for `session-1` with stats and, optionally, a linked
/// review request whose forge status is `status_summary`.
fn entry_with_stats(status_summary: Option<&str>) -> SessionListEntry {
    SessionListEntry {
        stats: Some(SessionListStats {
            added_lines: 12,
            deleted_lines: 3,
            has_merge_conflict: true,
            in_progress_started_at: Some(40),
            in_progress_total_seconds: 90,
            model: AgentModel::ClaudeOpus55,
            reasoning_level: ReasoningLevel::Max,
            review_request: status_summary.map(|status_summary| ReviewRequestSummary {
                display_id: "#12".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some(status_summary.to_string()),
                target_branch: "main".to_string(),
                title: "Build it".to_string(),
                web_url: "https://example.com/pull/12".to_string(),
            }),
            size_label: "S".to_string(),
        }),
        summary: SessionSummary {
            id: SessionId::from("session-1"),
            status: SessionStatus::Review,
            title: Some("Build it".to_string()),
            updated_at: 5,
        },
    }
}

#[test]
fn session_list_stats_carry_the_host_size_label_and_review_readiness() {
    // Arrange
    let ready = entry_with_stats(Some("Mergeable, PR ready"));
    let bypass = entry_with_stats(Some("Review required, PR ready with bypass"));
    let blocked = entry_with_stats(Some("Checks failing"));
    let unlinked = entry_with_stats(None);

    // Act
    let ready = RemoteSessionSummary::from(ready);
    let encoded = serde_json::to_value(&ready).expect("summary should encode");
    let bypass = RemoteSessionSummary::from(bypass);
    let blocked = RemoteSessionSummary::from(blocked);
    let unlinked = RemoteSessionSummary::from(unlinked);

    // Assert
    assert_eq!(
        encoded,
        json!({
            "added_lines": 12,
            "deleted_lines": 3,
            "has_merge_conflict": true,
            "id": "session-1",
            "in_progress_started_at": 40,
            "in_progress_total_seconds": 90,
            "model": "claude-opus-5-5",
            "model_short_name": AgentModel::ClaudeOpus55.short_name(),
            "reasoning": "max",
            "review_ready": "ready",
            "review_state": "Open",
            "size": "S",
            "status": "Review",
            "title": "Build it",
            "updated_at": 5
        })
    );
    assert_eq!(
        bypass.review_ready,
        Some(RemoteReviewReadiness::ReadyWithBypass)
    );
    assert_eq!(blocked.review_ready, None);
    assert_eq!(blocked.review_state.as_deref(), Some("Open"));
    assert_eq!(unlinked.review_state, None);
    assert_eq!(
        serde_json::from_value::<RemoteSessionSummary>(encoded).expect("summary should decode"),
        ready
    );
}

#[test]
fn session_conversion_keeps_chat_messages_and_drops_agent_prompts() {
    // Arrange
    let session = session_fixture(&SessionId::from("session-1"));

    // Act
    let remote = RemoteSession::from(session);

    // Assert
    assert_eq!(
        remote,
        RemoteSession {
            agent: Some("codex".to_string()),
            base_branch: Some("main".to_string()),
            branch: Some("wt/session-".to_string()),
            created_at: Some(10),
            id: "session-1".to_string(),
            is_draft: false,
            model: Some("gpt-6.1-sol".to_string()),
            published_upstream_ref: None,
            reasoning: Some("high".to_string()),
            response_style: Some("balanced".to_string()),
            review_request: None,
            speed_mode: Some("normal".to_string()),
            messages: vec![
                RemoteMessage {
                    content: "build it".to_string(),
                    kind: RemoteMessageKind::User,
                },
                RemoteMessage {
                    content: "ran tests".to_string(),
                    kind: RemoteMessageKind::Activity,
                },
                RemoteMessage {
                    content: "done".to_string(),
                    kind: RemoteMessageKind::Assistant,
                },
                RemoteMessage {
                    content: "committed".to_string(),
                    kind: RemoteMessageKind::Notice,
                },
            ],
            questions: vec![RemoteQuestion {
                options: vec!["main".to_string(), "develop".to_string()],
                text: "Which branch?".to_string(),
            }],
            queued_messages: vec!["next".to_string()],
            status: "Question".to_string(),
            title: Some("Build it".to_string()),
            updated_at: 20,
        }
    );
}

#[test]
fn session_conversion_maps_the_review_request_and_hides_an_absent_speed_control() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.published_upstream_ref = Some("origin/wt/session-".to_string());
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 30,
        summary: ReviewRequestSummary {
            display_id: "#12".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("PR ready".to_string()),
            target_branch: "main".to_string(),
            title: "Build it".to_string(),
            web_url: "https://example.com/pull/12".to_string(),
        },
    });
    session.settings.agent = AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini4Neon);
    session.settings.is_draft = true;

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");

    // Assert
    assert_eq!(remote.agent.as_deref(), Some("antigravity"));
    assert_eq!(remote.speed_mode, None);
    assert!(remote.is_draft);
    assert_eq!(
        remote.published_upstream_ref.as_deref(),
        Some("origin/wt/session-")
    );
    assert_eq!(
        encoded["review_request"],
        json!({
            "display_id": "#12",
            "source_branch": "wt/session-",
            "state": "Open",
            "status_summary": "PR ready",
            "target_branch": "main",
            "title": "Build it",
            "web_url": "https://example.com/pull/12"
        })
    );
    assert!(encoded.get("speed_mode").is_none());
}

#[test]
fn session_frames_from_an_older_server_parse_without_details() {
    // Arrange
    let old = json!({
        "id": "s",
        "messages": [],
        "questions": [],
        "queued_messages": [],
        "status": "Review",
        "title": null,
        "updated_at": 1
    });

    // Act
    let session: RemoteSession = serde_json::from_value(old.clone()).expect("session should parse");
    let encoded = serde_json::to_value(&session).expect("session should encode");

    // Assert
    assert_eq!(session.agent, None);
    assert_eq!(session.review_request, None);
    assert!(!session.is_draft);
    assert_eq!(encoded, old);
}

/// Returns one changed file at `path` with `status` and patch `text`.
fn diff_file(path: &str, status: SessionDiffFileStatus, text: &str) -> SessionDiffFile {
    SessionDiffFile {
        added_lines: 2,
        deleted_lines: 1,
        old_path: (status == SessionDiffFileStatus::Renamed).then(|| "old.rs".to_string()),
        patch: text.to_string(),
        path: path.to_string(),
        status,
    }
}

#[test]
fn diff_frames_encode_the_file_list_and_one_patch() {
    // Arrange
    let request = json!({"method": "get_session_diff", "session_id": "s", "path": "a.rs"});
    let files = vec![
        diff_file("a.rs", SessionDiffFileStatus::Added, "+a\n"),
        diff_file("b.rs", SessionDiffFileStatus::Deleted, "-b\n"),
        diff_file("c.rs", SessionDiffFileStatus::Modified, "+c\n"),
        diff_file("new.rs", SessionDiffFileStatus::Renamed, ""),
    ];

    // Act
    let request: RemoteRequest = serde_json::from_value(request).expect("request should parse");
    let listed = RemoteResponse::from_diff(&files, None).expect("list should build");
    let opened = RemoteResponse::from_diff(&files, Some("a.rs")).expect("patch should build");
    let missing = RemoteResponse::from_diff(&files, Some("z.rs"));

    // Assert
    assert_eq!(
        request,
        RemoteRequest::GetSessionDiff {
            path: Some("a.rs".to_string()),
            session_id: "s".to_string(),
        }
    );
    assert_eq!(
        serde_json::to_value(&listed).expect("list should encode"),
        json!({"kind": "diff", "files": [
            {"added_lines": 2, "deleted_lines": 1, "path": "a.rs", "status": "added"},
            {"added_lines": 2, "deleted_lines": 1, "path": "b.rs", "status": "deleted"},
            {"added_lines": 2, "deleted_lines": 1, "path": "c.rs", "status": "modified"},
            {
                "added_lines": 2,
                "deleted_lines": 1,
                "old_path": "old.rs",
                "path": "new.rs",
                "status": "renamed"
            }
        ]})
    );
    assert_eq!(
        serde_json::to_value(&opened).expect("patch should encode"),
        json!({"kind": "diff", "files": [
            {"added_lines": 2, "deleted_lines": 1, "path": "a.rs", "status": "added"}
        ], "patch": "+a\n"})
    );
    assert_eq!(missing, None);
}

#[test]
fn large_patches_are_cut_at_a_line_boundary_within_the_encoded_budget() {
    // Arrange
    let line = format!("+{}\n", "\u{1}".repeat(99));
    let patch = line.repeat(MAX_DIFF_RESPONSE_BYTES / 100);
    let unbroken = "x".repeat(MAX_DIFF_RESPONSE_BYTES + 1);
    let files = vec![
        diff_file("big.rs", SessionDiffFileStatus::Modified, &patch),
        diff_file("long.rs", SessionDiffFileStatus::Modified, &unbroken),
    ];

    // Act
    let big = RemoteResponse::from_diff(&files, Some("big.rs")).expect("patch should build");
    let long = RemoteResponse::from_diff(&files, Some("long.rs")).expect("patch should build");

    // Assert
    let (_, big_patch, big_truncated) = diff_parts(big);
    let big_patch = big_patch.expect("patch should be present");
    assert!(big_truncated);
    assert!(big_patch.len() < patch.len());
    assert!(big_patch.ends_with('\n'));
    assert!(
        serde_json::to_string(&big_patch)
            .expect("patch should encode")
            .len()
            <= MAX_DIFF_RESPONSE_BYTES + 2
    );
    let (_, long_patch, long_truncated) = diff_parts(long);
    assert!(long_truncated);
    assert_eq!(
        long_patch.map(|long_patch| long_patch.len()),
        Some(MAX_DIFF_RESPONSE_BYTES)
    );
}

/// Returns the files, patch, and truncation flag of a `diff` response, or
/// nothing for any other response.
fn diff_parts(response: RemoteResponse) -> (Vec<RemoteDiffFile>, Option<String>, bool) {
    match response {
        RemoteResponse::Diff {
            files,
            patch,
            truncated,
        } => (files, patch, truncated),
        _ => (Vec::new(), None, false),
    }
}

#[test]
fn long_file_lists_stop_within_the_encoded_budget() {
    // Arrange
    let path = format!("dir/\"{}\"", "\\".repeat(1000));
    let files: Vec<SessionDiffFile> = (0..200)
        .map(|_| diff_file(&path, SessionDiffFileStatus::Modified, ""))
        .collect();

    // Act
    let listed = RemoteResponse::from_diff(&files, None).expect("list should build");

    // Assert
    let encoded = serde_json::to_string(&listed).expect("list should encode");
    let (listed_files, _, truncated) = diff_parts(listed);
    assert!(truncated);
    assert!(!listed_files.is_empty() && listed_files.len() < files.len());
    assert!(encoded.len() <= MAX_DIFF_RESPONSE_BYTES);
}

use ag_session::{
    AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata, CreateSessionMode, ForgeKind,
    HostProjectStatus, HostProjectSync, HostProjectSyncPhase, HostSessionMode, ReasoningLevel,
    ResponseStyle, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionDiffFile,
    SessionDiffFileStatus, SessionFileEntry, SessionFocusedReview, SessionId, SessionListEntry,
    SessionListStats, SessionStatus, SessionSummary, SpeedMode,
};
use serde_json::json;

use crate::protocol::{
    ClientFrame, MAX_FILE_RESPONSE_BYTES, RemoteDiffFile, RemoteFocusedReview,
    RemoteFocusedReviewState, RemoteMessage, RemoteMessageKind, RemoteModeModel, RemoteModelOption,
    RemoteProject, RemoteQuestion, RemoteQuestionAnswer, RemoteRequest, RemoteResponse,
    RemoteResult, RemoteReviewReadiness, RemoteReviewRequest, RemoteSession, RemoteSessionMode,
    RemoteSessionSummary, ServerFrame,
};
use crate::test_support::{host_fixture, project_status_fixture, session_fixture};

#[path = "protocol_test/graph_test.rs"]
mod graph;

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
        {"type": "request", "id": 8, "request": {"method": "sync_session", "session_id": "s"}},
        {"type": "request", "id": 9, "request": {"method": "publish_session", "session_id": "s"}},
        {"type": "request", "id": 10, "request": {"method": "sync_project"}}
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
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 3,
                request: RemoteRequest::SendMessage {
                    message: "hi".to_string(),
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 4,
                request: RemoteRequest::AnswerQuestions {
                    answers: vec![RemoteQuestionAnswer {
                        answer: "main".to_string(),
                        question: "Which?".to_string(),
                    }],
                    graph_question_target: None,
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 5,
                request: RemoteRequest::CancelSession {
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 6,
                request: RemoteRequest::MergeSession {
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 7,
                request: RemoteRequest::StopTurn {
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 8,
                request: RemoteRequest::SyncSession {
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 9,
                request: RemoteRequest::PublishSession {
                    session_id: "s".into(),
                },
            },
            ClientFrame::Request {
                id: 10,
                request: RemoteRequest::SyncProject,
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
                project: None,
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
fn review_request_response_names_the_forge_and_omits_a_missing_status() {
    // Arrange
    let review_request = ReviewRequest {
        last_refreshed_at: 30,
        summary: ReviewRequestSummary {
            display_id: "!4".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "agentty/s".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Build it".to_string(),
            web_url: "https://gitlab.com/example/project/-/merge_requests/4".to_string(),
        },
    };

    // Act
    let response = RemoteResponse::ReviewRequest {
        review_request: RemoteReviewRequest::from(review_request.summary),
    };
    let encoded = serde_json::to_value(&response).expect("response should encode");

    // Assert
    assert_eq!(
        encoded,
        json!({
            "kind": "review_request",
            "review_request": {
                "display_id": "!4",
                "forge": "GitLab",
                "source_branch": "agentty/s",
                "state": "Merged",
                "target_branch": "main",
                "title": "Build it",
                "web_url": "https://gitlab.com/example/project/-/merge_requests/4"
            }
        })
    );
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
            mode_models: vec![RemoteModeModel {
                agent: "codex".to_string(),
                mode: RemoteSessionMode::Regular,
                model: "gpt-6.1-sol".to_string(),
                name: "gpt-6.1-sol".to_string(),
                reasoning: None,
                response_style: None,
                speed_mode: None,
            }],
            models: Vec::new(),
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
                "mode_models": [{
                    "agent": "codex",
                    "mode": "regular",
                    "model": "gpt-6.1-sol",
                    "name": "gpt-6.1-sol"
                }],
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
    let harness = AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3);
    let mut host = host_fixture();
    host.session_modes = vec![
        HostSessionMode {
            agent: ag_session::test_support::codex_selection(),
            mode: CreateSessionMode::Draft,
            reasoning_level: ReasoningLevel::High,
            response_style: ResponseStyle::Concise,
            speed_mode: SpeedMode::Fast,
        },
        HostSessionMode {
            agent: harness,
            mode: CreateSessionMode::Harness,
            reasoning_level: ReasoningLevel::Low,
            response_style: ResponseStyle::Detailed,
            speed_mode: SpeedMode::Normal,
        },
        HostSessionMode {
            agent: ag_session::test_support::codex_selection(),
            mode: CreateSessionMode::Regular,
            reasoning_level: ReasoningLevel::High,
            response_style: ResponseStyle::Concise,
            speed_mode: SpeedMode::Fast,
        },
    ];

    // Act
    let response = RemoteResponse::from(host);

    // Assert
    assert_eq!(
        response,
        RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            mode_models: vec![
                RemoteModeModel {
                    agent: harness.kind().name().to_string(),
                    mode: RemoteSessionMode::Harness,
                    model: AgentModel::KimiK3.as_str().to_string(),
                    name: AgentModel::KimiK3.name().to_string(),
                    reasoning: Some("low".to_string()),
                    response_style: Some("detailed".to_string()),
                    // The harness has no speed control.
                    speed_mode: None,
                },
                RemoteModeModel {
                    agent: AgentKind::Codex.name().to_string(),
                    mode: RemoteSessionMode::Regular,
                    model: ag_session::test_support::CODEX_MODEL_ID.to_string(),
                    name: ag_session::test_support::CODEX_MODEL.name().to_string(),
                    reasoning: Some("high".to_string()),
                    response_style: Some("concise".to_string()),
                    speed_mode: Some("fast".to_string()),
                },
            ],
            models: vec![RemoteModelOption::from(
                ag_session::test_support::codex_selection()
            )],
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

/// Builds a list entry for `session-1` with stats, three commits ahead of
/// and one behind its base and, optionally, published two commits behind
/// its upstream with a linked review request whose forge status is
/// `status_summary`.
fn entry_with_stats(status_summary: Option<&str>) -> SessionListEntry {
    SessionListEntry {
        stats: Some(SessionListStats {
            added_lines: 12,
            base_ahead_behind: Some((3, 1)),
            deleted_lines: 3,
            has_merge_conflict: true,
            in_progress_started_at: Some(40),
            in_progress_total_seconds: 90,
            is_published: status_summary.is_some(),
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
            upstream_ahead_behind: status_summary.map(|_| (0, 2)),
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
fn session_list_stats_carry_the_size_label_branch_distances_and_review_request() {
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
            "base_ahead": 3,
            "base_behind": 1,
            "deleted_lines": 3,
            "has_merge_conflict": true,
            "id": "session-1",
            "in_progress_started_at": 40,
            "in_progress_total_seconds": 90,
            "is_published": true,
            "model": "claude-opus-5-5",
            "model_short_name": AgentModel::ClaudeOpus55.short_name(),
            "reasoning": "max",
            "review_display_id": "#12",
            "review_ready": "ready",
            "review_state": "Open",
            "size": "S",
            "status": "Review",
            "title": "Build it",
            "updated_at": 5,
            "upstream_ahead": 0,
            "upstream_behind": 2
        })
    );
    assert_eq!(
        bypass.review_ready,
        Some(RemoteReviewReadiness::ReadyWithBypass)
    );
    assert_eq!(blocked.review_ready, None);
    assert_eq!(blocked.review_state.as_deref(), Some("Open"));
    assert_eq!(unlinked.review_display_id, None);
    assert_eq!(unlinked.review_state, None);
    let unlinked = serde_json::to_value(&unlinked).expect("summary should encode");
    assert!(unlinked.get("is_published").is_none());
    assert!(unlinked.get("upstream_ahead").is_none());
    assert_eq!(unlinked["base_ahead"], 3);
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
            draft_prompt: None,
            focused_review: None,
            graph: None,
            graph_question_target: None,
            graph_questions: Vec::new(),
            id: "session-1".to_string(),
            is_draft: false,
            model: Some("gpt-6.1-sol".to_string()),
            model_id: Some("gpt-6.1-sol".to_string()),
            progress: None,
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
            queued_actions: vec!["sync after this turn".to_string()],
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
    session.progress = Some("Working... Reading files".to_string());
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
    session.draft_prompt = Some("staged prompt".to_string());
    session.queued_actions = Vec::new();

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");

    // Assert
    assert_eq!(remote.agent.as_deref(), Some("antigravity"));
    assert_eq!(encoded["progress"], "Working... Reading files");
    assert_eq!(remote.speed_mode, None);
    assert!(remote.is_draft);
    assert_eq!(encoded["draft_prompt"], "staged prompt");
    assert!(encoded.get("queued_actions").is_none());
    assert_eq!(
        remote.published_upstream_ref.as_deref(),
        Some("origin/wt/session-")
    );
    assert_eq!(
        encoded["review_request"],
        json!({
            "display_id": "#12",
            "forge": "GitHub",
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
    assert_eq!(session.draft_prompt, None);
    assert_eq!(session.queued_actions, Vec::<String>::new());
    assert_eq!(session.focused_review, None);
    assert_eq!(encoded, old);
}

#[test]
fn session_conversion_carries_a_ready_focused_review_as_markdown() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.focused_review = Some(SessionFocusedReview::Ready {
        text: "### Suggestions\n- Add a test".to_string(),
    });

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");

    // Assert
    assert_eq!(
        remote.focused_review,
        Some(RemoteFocusedReview {
            state: RemoteFocusedReviewState::Ready,
            text: "### Suggestions\n- Add a test".to_string(),
        })
    );
    assert_eq!(
        encoded["focused_review"],
        json!({"state": "ready", "text": "### Suggestions\n- Add a test"})
    );
}

#[test]
fn session_conversion_carries_a_failed_focused_review_as_its_message() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.focused_review = Some(SessionFocusedReview::Failed {
        message: "Review assist unavailable: timeout".to_string(),
    });

    // Act
    let remote = RemoteSession::from(session);
    let encoded = serde_json::to_value(&remote).expect("session should encode");
    let decoded: RemoteSession =
        serde_json::from_value(encoded.clone()).expect("session should decode");

    // Assert
    assert_eq!(
        encoded["focused_review"],
        json!({"state": "failed", "text": "Review assist unavailable: timeout"})
    );
    assert_eq!(decoded, remote);
}

#[test]
fn session_frames_omit_an_absent_focused_review() {
    // Arrange
    let session = session_fixture(&SessionId::from("session-1"));

    // Act
    let encoded =
        serde_json::to_value(RemoteSession::from(session)).expect("session should encode");

    // Assert
    assert!(encoded.get("focused_review").is_none());
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
            session_id: "s".into(),
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
    let patch = line.repeat(MAX_FILE_RESPONSE_BYTES / 100);
    let unbroken = "x".repeat(MAX_FILE_RESPONSE_BYTES + 1);
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
            <= MAX_FILE_RESPONSE_BYTES + 2
    );
    let (_, long_patch, long_truncated) = diff_parts(long);
    assert!(long_truncated);
    assert_eq!(
        long_patch.map(|long_patch| long_patch.len()),
        Some(MAX_FILE_RESPONSE_BYTES)
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
    assert!(encoded.len() <= MAX_FILE_RESPONSE_BYTES);
}

#[test]
fn session_conversion_reports_the_model_name_and_id_a_settings_change_takes() {
    // Arrange
    let mut session = session_fixture(&SessionId::from("session-1"));
    let served = "openai-compatible/qwen3:latest"
        .parse::<AgentModel>()
        .expect("served model should parse");
    session.settings.agent = AgentSelection::new(AgentKind::Harness, served);

    // Act
    let remote = RemoteSession::from(session);
    let option = RemoteModelOption::from(AgentSelection::new(AgentKind::Harness, served));

    // Assert
    assert_eq!(remote.model.as_deref(), Some("qwen3:latest"));
    assert_eq!(
        remote.model_id.as_deref(),
        Some("openai-compatible/qwen3:latest")
    );
    assert_eq!(remote.model.as_deref(), Some(option.name.as_str()));
    assert_eq!(remote.model_id.as_deref(), Some(option.model.as_str()));
}

#[test]
fn settings_requests_name_only_the_settings_that_change() {
    // Arrange
    let wire = json!([
        {"method": "set_session_settings", "session_id": "s", "reasoning": "low"},
        {
            "method": "set_session_settings",
            "session_id": "s",
            "agent": "harness",
            "model": "gpt-6.1-sol",
            "response_style": "concise",
            "speed_mode": "fast"
        }
    ]);

    // Act
    let requests: Vec<RemoteRequest> =
        serde_json::from_value(wire.clone()).expect("settings requests should parse");
    let encoded = serde_json::to_value(&requests).expect("settings requests should encode");

    // Assert
    assert_eq!(
        requests,
        [
            RemoteRequest::SetSessionSettings {
                agent: None,
                model: None,
                reasoning: Some("low".to_string()),
                response_style: None,
                session_id: "s".into(),
                speed_mode: None,
            },
            RemoteRequest::SetSessionSettings {
                agent: Some("harness".to_string()),
                model: Some("gpt-6.1-sol".to_string()),
                reasoning: None,
                response_style: Some("concise".to_string()),
                session_id: "s".into(),
                speed_mode: Some("fast".to_string()),
            },
        ]
    );
    assert_eq!(encoded, wire);
}

#[test]
fn model_options_describe_the_model_and_its_fast_mode() {
    // Arrange
    let selection = ag_session::test_support::codex_selection();

    // Act
    let option = RemoteModelOption::from(selection);

    // Assert
    assert_eq!(option.agent, "codex");
    assert_eq!(option.model, selection.model().as_str());
    assert_eq!(option.supports_fast, selection.supports_fast_mode());
    assert_ne!(option.name, "");
    assert_ne!(option.description, "");
}

/// Returns one worktree entry at `path`.
fn file_entry(is_dir: bool, path: &str) -> SessionFileEntry {
    SessionFileEntry {
        is_dir,
        path: path.to_string(),
    }
}

#[test]
fn session_file_frames_encode_entries_in_host_order() {
    // Arrange
    let request = json!({"method": "list_session_files", "session_id": "s"});
    let project_request = json!({"method": "list_project_files"});
    let entries = vec![
        file_entry(true, "src"),
        file_entry(false, "README.md"),
        file_entry(false, "src/lib.rs"),
    ];

    // Act
    let request: RemoteRequest = serde_json::from_value(request).expect("request should parse");
    let project_request: RemoteRequest =
        serde_json::from_value(project_request).expect("project request should parse");
    let listed = RemoteResponse::from_session_files(&entries);

    // Assert
    assert_eq!(
        request,
        RemoteRequest::ListSessionFiles {
            session_id: "s".into(),
        }
    );
    assert_eq!(project_request, RemoteRequest::ListProjectFiles);
    assert_eq!(
        serde_json::to_value(&listed).expect("list should encode"),
        json!({"kind": "session_files", "files": [
            {"is_dir": true, "path": "src"},
            {"is_dir": false, "path": "README.md"},
            {"is_dir": false, "path": "src/lib.rs"}
        ]})
    );
}

#[test]
fn long_session_file_lists_stop_within_the_encoded_budget() {
    // Arrange
    let path = format!("dir/\"{}\"", "\\".repeat(1000));
    let entries: Vec<SessionFileEntry> = (0..200).map(|_| file_entry(false, &path)).collect();

    // Act
    let listed = RemoteResponse::from_session_files(&entries);

    // Assert
    let encoded = serde_json::to_string(&listed).expect("list should encode");
    let (files, truncated) = match listed {
        RemoteResponse::SessionFiles { files, truncated } => (files, truncated),
        _ => (Vec::new(), false),
    };
    assert!(truncated);
    assert!(!files.is_empty() && files.len() < entries.len());
    assert!(encoded.len() <= MAX_FILE_RESPONSE_BYTES);
}

/// The project a host reports while `phase` is its latest sync.
fn project_with_sync(phase: HostProjectSyncPhase) -> HostProjectStatus {
    HostProjectStatus {
        sync: Some(HostProjectSync {
            branch: "main".to_string(),
            phase,
        }),
        ..project_status_fixture()
    }
}

#[test]
fn project_frames_carry_the_branch_counts_and_each_sync_phase() {
    // Arrange
    let idle = HostProjectStatus {
        ahead_behind: None,
        branch: None,
        sync: None,
        ..project_status_fixture()
    };
    let projects = [
        idle,
        project_with_sync(HostProjectSyncPhase::Running),
        project_status_fixture(),
        project_with_sync(HostProjectSyncPhase::Complete {
            pulled_commits: Some(4),
            pushed_commits: None,
            resolved_conflict_count: 0,
        }),
        project_with_sync(HostProjectSyncPhase::Complete {
            pulled_commits: None,
            pushed_commits: Some(1),
            resolved_conflict_count: 2,
        }),
        project_with_sync(HostProjectSyncPhase::Blocked {
            message: "a merge is active".to_string(),
        }),
        project_with_sync(HostProjectSyncPhase::Failed {
            message: "push rejected".to_string(),
        }),
    ];

    // Act
    let remote: Vec<RemoteProject> = projects.into_iter().map(RemoteProject::from).collect();
    let encoded = serde_json::to_value(&remote).expect("projects should encode");
    let decoded: Vec<RemoteProject> =
        serde_json::from_value(encoded.clone()).expect("projects should decode");

    // Assert
    assert_eq!(
        encoded,
        json!([
            {"name": "agentty"},
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {"branch": "main", "phase": "running"}
            },
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {"branch": "main", "conflicts": 3, "phase": "resolving_conflicts"}
            },
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {"branch": "main", "phase": "complete", "pulled_commits": 4}
            },
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {
                    "branch": "main", "conflicts": 2, "phase": "complete", "pushed_commits": 1
                }
            },
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {"branch": "main", "message": "a merge is active", "phase": "blocked"}
            },
            {
                "ahead": 1, "behind": 2, "branch": "main", "name": "agentty",
                "sync": {"branch": "main", "message": "push rejected", "phase": "failed"}
            }
        ])
    );
    assert_eq!(decoded, remote);
}

#[test]
fn session_lists_name_the_project_only_when_the_host_reports_it() {
    // Arrange
    let listed = RemoteResponse::Sessions {
        project: Some(RemoteProject::from(HostProjectStatus {
            sync: None,
            ..project_status_fixture()
        })),
        sessions: Vec::new(),
    };
    let from_older_server = json!({"kind": "sessions", "sessions": []});

    // Act
    let encoded = serde_json::to_value(&listed).expect("list should encode");
    let decoded: RemoteResponse =
        serde_json::from_value(from_older_server).expect("list should decode");

    // Assert
    assert_eq!(
        encoded,
        json!({
            "kind": "sessions",
            "project": {"ahead": 1, "behind": 2, "branch": "main", "name": "agentty"},
            "sessions": []
        })
    );
    assert_eq!(
        decoded,
        RemoteResponse::Sessions {
            project: None,
            sessions: Vec::new(),
        }
    );
}

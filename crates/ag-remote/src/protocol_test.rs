use ag_session::{SessionId, SessionStatus, SessionSummary};
use serde_json::json;

use crate::protocol::{
    ClientFrame, RemoteMessage, RemoteMessageKind, RemoteQuestion, RemoteQuestionAnswer,
    RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionSummary, ServerFrame,
};
use crate::test_support::session_fixture;

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
        {"type": "request", "id": 5, "request": {"method": "cancel_session", "session_id": "s"}}
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
        ]
    );
    assert_eq!(encoded, wire);
}

#[test]
fn unknown_methods_parse_as_unsupported_requests() {
    // Arrange
    let with_fields = json!({"type": "request", "id": 1, "request": {
        "method": "merge_session", "session_id": "s"
    }});
    let bare = json!({"type": "request", "id": 2, "request": {"method": "get_host"}});

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
fn session_summary_converts_status_to_display_string() {
    // Arrange
    let summary = SessionSummary {
        id: SessionId::from("session-1"),
        status: SessionStatus::InProgress,
        title: None,
        updated_at: 5,
    };

    // Act
    let remote = RemoteSessionSummary::from(summary);

    // Assert
    assert_eq!(
        remote,
        RemoteSessionSummary {
            id: "session-1".to_string(),
            status: "InProgress".to_string(),
            title: None,
            updated_at: 5,
        }
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
            id: "session-1".to_string(),
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

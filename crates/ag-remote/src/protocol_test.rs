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
fn client_frames_reject_excluded_methods() {
    // Arrange
    let wire = json!({"type": "request", "id": 1, "request": {
        "method": "merge_session", "session_id": "s"
    }});

    // Act
    let parsed = serde_json::from_value::<ClientFrame>(wire);

    // Assert
    assert!(parsed.is_err());
}

#[test]
fn server_frames_encode_results_as_ok_or_error_objects() {
    // Arrange
    let frames = vec![
        ServerFrame::Welcome {
            device_id: "d".to_string(),
            protocol_version: 1,
            revision: 3,
        },
        ServerFrame::Paired {
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
            {"type": "rejected", "reason": "no"}
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

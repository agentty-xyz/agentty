//! Request round trips and change pushes on authenticated connections.

use std::time::Duration;

use ag_remote::protocol::{
    ClientFrame, RemoteDiffFile, RemoteDiffFileStatus, RemoteFileEntry, RemoteFocusedReview,
    RemoteFocusedReviewState, RemoteGraphAction, RemoteGraphActionRequest, RemoteGraphApproval,
    RemoteGraphProposalPhase, RemoteGraphStatus, RemoteImage, RemoteMessage, RemoteMessageKind,
    RemoteModeModel, RemoteModelOption, RemoteProject, RemoteProjectEntry, RemoteProjectSync,
    RemoteProjectSyncPhase, RemoteQuestion, RemoteQuestionAnswer, RemoteRequest, RemoteResponse,
    RemoteResult, RemoteReviewRequest, RemoteSession, RemoteSessionGraph, RemoteSessionMode,
    RemoteSessionSummary, RequestedProjectId, RequestedSessionId, ServerFrame,
};
use ag_remote::test_support::{MemoryDeviceStore, RecordingBackend};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    Client, TestResult, TestServer, assert_rejected, hello_frame, next_frame, send,
};

/// Sends one request and returns its result.
async fn call(client: &mut Client, id: u64, request: RemoteRequest) -> TestResult<RemoteResult> {
    send(client, &ClientFrame::Request { id, request }).await?;
    match next_frame(client).await? {
        ServerFrame::Response {
            id: response_id,
            result,
        } if response_id == id => Ok(result),
        frame => Err(format!("expected response {id}, got {frame:?}").into()),
    }
}

/// Decodes the next text message as a server frame without a client-side
/// timeout, so a paused clock can advance to server deadlines.
async fn next_untimed_frame(client: &mut Client) -> TestResult<ServerFrame> {
    let Some(Ok(Message::Text(text))) = client.next().await else {
        return Err("expected a text frame".into());
    };

    Ok(serde_json::from_str(&text)?)
}

/// Builds the listing entry of the session `id`.
fn summary(id: &str, status: &str, title: &str, updated_at: i64) -> RemoteSessionSummary {
    RemoteSessionSummary {
        id: id.to_string(),
        status: status.to_string(),
        title: Some(title.to_string()),
        updated_at,
        ..RemoteSessionSummary::default()
    }
}

/// Builds the test backend's project as a listing describes it, with `sync`
/// as its latest sync.
fn project(sync: Option<RemoteProjectSync>) -> RemoteProject {
    RemoteProject {
        ahead: Some(0),
        behind: Some(2),
        branch: Some("main".to_string()),
        id: None,
        name: "agentty".to_string(),
        sync,
    }
}

/// Returns the snapshot of the test backend's `session-1`, with its
/// settings, branches, and chat-visible transcript.
fn expected_session() -> RemoteSession {
    RemoteSession {
        agent: Some("codex".to_string()),
        base_branch: Some("main".to_string()),
        branch: Some("wt/session-".to_string()),
        created_at: Some(10),
        draft_prompt: None,
        focused_review: Some(RemoteFocusedReview {
            state: RemoteFocusedReviewState::Ready,
            text: "### Suggestions\n- Add a test".to_string(),
        }),
        graph: None,
        graph_question_target: None,
        graph_questions: Vec::new(),
        id: "session-1".to_string(),
        is_draft: false,
        messages: vec![
            RemoteMessage {
                content: "build it".to_string(),
                kind: RemoteMessageKind::User,
            },
            RemoteMessage {
                content: "which branch?".to_string(),
                kind: RemoteMessageKind::Assistant,
            },
        ],
        model: Some("gpt-6.1-sol".to_string()),
        model_id: Some("gpt-6.1-sol".to_string()),
        progress: None,
        published_upstream_ref: None,
        questions: vec![RemoteQuestion {
            options: vec!["main".to_string()],
            text: "Which branch?".to_string(),
        }],
        queued_actions: Vec::new(),
        queued_messages: Vec::new(),
        reasoning: Some("high".to_string()),
        response_style: Some("balanced".to_string()),
        review_request: None,
        speed_mode: Some("normal".to_string()),
        status: "Question".to_string(),
        title: Some("Build it".to_string()),
        updated_at: 20,
    }
}

#[tokio::test]
async fn each_request_reaches_the_session_service() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let listed = call(
        &mut client,
        1,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    let loaded = call(
        &mut client,
        2,
        RemoteRequest::GetSession {
            session_id: "session-1".into(),
        },
    )
    .await?;
    let unlisted = call(
        &mut client,
        3,
        RemoteRequest::GetSession {
            session_id: "other-project-session".into(),
        },
    )
    .await?;
    let sent = call(
        &mut client,
        4,
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "continue".to_string(),
            session_id: "session-1".into(),
        },
    )
    .await?;
    let answered = call(
        &mut client,
        5,
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
            graph_question_target: None,
            session_id: "session-1".into(),
        },
    )
    .await?;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Sessions {
            project: Some(project(None)),
            sessions: vec![
                summary("session-1", "Review", "Build it", 20),
                RemoteSessionSummary {
                    added_lines: Some(64),
                    base_ahead: Some(2),
                    base_behind: Some(0),
                    deleted_lines: Some(9),
                    in_progress_started_at: Some(100),
                    in_progress_total_seconds: Some(12),
                    model: Some("gpt-6.1-sol".to_string()),
                    model_short_name: Some("gpt-6.1-sol".to_string()),
                    reasoning: Some("high".to_string()),
                    size: Some("M".to_string()),
                    ..summary("session-2", "InProgress", "Plan it", 30)
                },
            ],
        })
    );
    assert_eq!(
        loaded,
        RemoteResult::Ok(RemoteResponse::Session {
            session: Some(Box::new(expected_session())),
        })
    );
    assert_eq!(
        unlisted,
        RemoteResult::Ok(RemoteResponse::Session { session: None })
    );
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(answered, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        server.backend.calls(),
        [
            "list",
            "list",
            "get:session-1",
            "list",
            "list",
            "send:session-1:continue",
            "list",
            "answer:session-1:Which branch?=main"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn session_graphs_and_their_summary_reach_only_listed_sessions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let graphs = |session_id: &str, graph_id: Option<&str>| RemoteRequest::GetSessionGraphs {
        graph_id: graph_id.map(str::to_string),
        known: None,
        node_id: None,
        session_id: session_id.into(),
    };

    // Act
    let snapshot = call(
        &mut client,
        1,
        RemoteRequest::GetSession {
            session_id: "session-2".into(),
        },
    )
    .await?;
    let shown = call(&mut client, 2, graphs("session-2", None)).await?;
    let foreign = call(&mut client, 3, graphs("session-2", Some("other-graph"))).await?;
    let unlisted = call(&mut client, 4, graphs("other-project-session", None)).await?;

    // Assert
    let RemoteResult::Ok(RemoteResponse::Session {
        session: Some(session),
    }) = snapshot
    else {
        return Err(format!("expected a session, got {snapshot:?}").into());
    };
    assert_eq!(
        session.graph,
        Some(RemoteSessionGraph {
            awaiting: vec![RemoteGraphApproval {
                node_id: "inspect".to_string(),
                revisable: false,
                text: "inspect awaits your approval; reply in chat to accept it".to_string(),
            }],
            done: 1,
            label: "Blocked".to_string(),
            phase: RemoteGraphProposalPhase::Approved,
            status: Some(RemoteGraphStatus::Blocked),
            total: 2,
            version: 1,
        })
    );
    let RemoteResult::Ok(RemoteResponse::Graphs {
        graph: Some(graph),
        graphs: entries,
        proposal: Some(proposal),
        ..
    }) = shown
    else {
        return Err(format!("expected graphs, got {shown:?}").into());
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(proposal.preview, "## Graph proposal v1");
    assert_eq!(graph.node.as_deref(), Some("inspect"));
    assert_eq!(
        graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.label.as_str()))
            .collect::<Vec<_>>(),
        [("inspect", "Awaiting your approval"), ("fix", "Waiting")]
    );
    assert_eq!(graph.outputs[0].text, "### Findings\n- One issue");
    assert_eq!(
        foreign,
        RemoteResult::Error("Graph does not belong to this session".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        server.backend.calls(),
        [
            "list",
            "get:session-2",
            "list",
            "graphs:session-2:-:-",
            "list",
            "graphs:session-2:other-graph:-",
            "list"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn session_graphs_answer_unchanged_for_the_version_the_device_shows() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let graphs = |known: Option<&str>| RemoteRequest::GetSessionGraphs {
        graph_id: None,
        known: known.map(str::to_string),
        node_id: None,
        session_id: "session-2".into(),
    };

    // Act
    let first = call(&mut client, 1, graphs(None)).await?;
    let RemoteResult::Ok(RemoteResponse::Graphs { version, .. }) = &first else {
        return Err(format!("expected graphs, got {first:?}").into());
    };
    let again = call(&mut client, 2, graphs(Some(version))).await?;
    let stale = call(&mut client, 3, graphs(Some("stale"))).await?;

    // Assert
    assert_ne!(version, "");
    assert_eq!(
        again,
        RemoteResult::Ok(RemoteResponse::Graphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
            unchanged: true,
            version: version.clone(),
        })
    );
    assert_eq!(stale, first);

    Ok(())
}

#[tokio::test]
async fn graph_actions_offered_in_the_graphs_response_reach_the_host_bound_to_its_snapshot()
-> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let accept = |session_id: &str, snapshot: &str| {
        RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::Accept,
            additional_calls: None,
            budget_id: None,
            graph_id: "graph".to_string(),
            message: None,
            node_id: Some("inspect".to_string()),
            output_id: Some("result".to_string()),
            session_id: session_id.into(),
            snapshot: snapshot.to_string(),
        })
    };
    let unknown = r#"{"type":"request","id":5,"request":{"method":"act_on_session_graph","action":"rewrite_history","graph_id":"graph","session_id":"session-2","snapshot":"5eed"}}"#;

    // Act
    let shown = call(
        &mut client,
        1,
        RemoteRequest::GetSessionGraphs {
            graph_id: None,
            known: None,
            node_id: None,
            session_id: "session-2".into(),
        },
    )
    .await?;
    let accepted = call(&mut client, 2, accept("session-2", "5eed")).await?;
    let stale = call(&mut client, 3, accept("session-2", "old")).await?;
    let unlisted = call(&mut client, 4, accept("other-project-session", "5eed")).await?;
    client.send(Message::Text(unknown.into())).await?;
    let refused = next_frame(&mut client).await?;
    let listed = call(
        &mut client,
        6,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    let RemoteResult::Ok(RemoteResponse::Graphs {
        graph: Some(graph), ..
    }) = shown
    else {
        return Err(format!("expected graphs, got {shown:?}").into());
    };
    assert_eq!(graph.actions, [RemoteGraphAction::Cancel]);
    assert_eq!(graph.outputs[0].actions, [RemoteGraphAction::Accept]);
    assert_eq!(accepted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        stale,
        RemoteResult::Error(
            "Graph changed; refresh the view and inspect it before acting again.".to_string()
        )
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        refused,
        ServerFrame::Response {
            id: 5,
            result: RemoteResult::Error("Unsupported graph action".to_string()),
        }
    );
    assert!(matches!(listed, RemoteResult::Ok(_)));
    let accept_call =
        "graph_action:session-2:graph:{}:Accept { node_id: \"inspect\", output_id: \"result\" }";
    assert_eq!(
        server.backend.calls(),
        [
            "list".to_string(),
            "graphs:session-2:-:-".to_string(),
            "list".to_string(),
            accept_call.replace("{}", "5eed"),
            "list".to_string(),
            accept_call.replace("{}", "old"),
            "list".to_string(),
            "list".to_string(),
            "list".to_string(),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn graph_proposals_reach_listed_sessions_under_an_identity_the_server_mints() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let propose = |session_id: &str, message: &str| RemoteRequest::GenerateGraphProposal {
        message: message.to_string(),
        session_id: session_id.into(),
    };

    // Act
    let accepted = call(&mut client, 1, propose("session-1", "Split the parser")).await?;
    let refused = call(&mut client, 2, propose("session-1", "busy")).await?;
    let unlisted = call(&mut client, 3, propose("other-project-session", "Split")).await?;

    // Assert
    assert_eq!(accepted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        refused,
        RemoteResult::Error("Graph generation requires a prepared idle session".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        server.backend.calls(),
        [
            "list",
            "graph_proposal:session-1:Split the parser",
            "list",
            "graph_proposal:session-1:busy",
            "list",
        ]
    );

    Ok(())
}

#[tokio::test]
async fn session_diffs_reach_only_listed_sessions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let diff = |session_id: &str, path: Option<&str>| RemoteRequest::GetSessionDiff {
        path: path.map(str::to_string),
        session_id: session_id.into(),
    };

    // Act
    let opened = call(&mut client, 1, diff("session-1", Some("a.rs"))).await?;
    let unlisted = call(&mut client, 2, diff("other-project-session", None)).await?;

    // Assert
    assert_eq!(
        opened,
        RemoteResult::Ok(RemoteResponse::Diff {
            files: vec![RemoteDiffFile {
                added_lines: 1,
                deleted_lines: 0,
                old_path: None,
                path: "a.rs".to_string(),
                status: RemoteDiffFileStatus::Added,
            }],
            patch: Some("diff --git a/a.rs b/a.rs\n+new\n".to_string()),
            truncated: false,
        })
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(server.backend.calls(), ["list", "diff:session-1", "list"]);

    Ok(())
}

#[tokio::test]
async fn session_file_listings_reach_only_listed_sessions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let files = |session_id: &str| RemoteRequest::ListSessionFiles {
        session_id: session_id.into(),
    };

    // Act
    let listed = call(&mut client, 1, files("session-1")).await?;
    let unlisted = call(&mut client, 2, files("other-project-session")).await?;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::SessionFiles {
            files: vec![
                RemoteFileEntry {
                    is_dir: true,
                    path: "src".to_string(),
                },
                RemoteFileEntry {
                    is_dir: false,
                    path: "src/lib.rs".to_string(),
                },
            ],
            truncated: false,
        })
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(server.backend.calls(), ["list", "files:session-1", "list"]);

    Ok(())
}

#[tokio::test]
async fn project_file_listings_name_the_project_checkout() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let listed = call(
        &mut client,
        1,
        RemoteRequest::ListProjectFiles { project_id: None },
    )
    .await?;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::SessionFiles {
            files: vec![RemoteFileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            }],
            truncated: false,
        })
    );
    assert_eq!(server.backend.calls(), ["project_files"]);

    Ok(())
}

#[tokio::test]
async fn host_errors_return_in_the_response_and_keep_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let busy = call(
        &mut client,
        1,
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "busy".to_string(),
            session_id: "session-1".into(),
        },
    )
    .await?;
    let canceled = call(
        &mut client,
        2,
        RemoteRequest::CancelSession {
            session_id: "session-1".into(),
        },
    )
    .await?;
    let listed = call(
        &mut client,
        3,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    assert_eq!(busy, RemoteResult::Error("Session is busy".to_string()));
    assert_eq!(
        canceled,
        RemoteResult::Error("Session not found".to_string())
    );
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

/// Sends request `id`: a message the backend holds until it is released.
async fn send_held_message(client: &mut Client, id: u64) -> TestResult {
    let request = RemoteRequest::SendMessage {
        images: Vec::new(),
        message: "wait".to_string(),
        session_id: "session-1".into(),
    };

    send(client, &ClientFrame::Request { id, request }).await
}

#[tokio::test]
async fn a_slow_request_delays_neither_other_requests_nor_change_pushes() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    send_held_message(&mut client, 1).await?;
    let listed = call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    server.revisions_tx.send_replace(5);
    let changed = next_frame(&mut client).await?;
    server.backend.release();
    let held = next_frame(&mut client).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(changed, ServerFrame::Changed { revision: 5 });
    assert_eq!(
        held,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::Accepted),
        }
    );

    Ok(())
}

#[tokio::test]
async fn a_pending_publish_blocks_neither_reads_nor_later_changes_to_its_session() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let publish = RemoteRequest::PublishSession {
        session_id: "session-1".into(),
    };

    // Act
    send(
        &mut client,
        &ClientFrame::Request {
            id: 1,
            request: publish,
        },
    )
    .await?;
    let listed = call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    let unlisted = call(
        &mut client,
        3,
        RemoteRequest::PublishSession {
            session_id: "other-project-session".into(),
        },
    )
    .await?;
    let messaged = call(
        &mut client,
        4,
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "hello".to_string(),
            session_id: "session-1".into(),
        },
    )
    .await?;
    let canceled = call(
        &mut client,
        5,
        RemoteRequest::CancelSession {
            session_id: "session-1".into(),
        },
    )
    .await?;
    let stopped = call(
        &mut client,
        6,
        RemoteRequest::StopTurn {
            session_id: "session-1".into(),
        },
    )
    .await?;
    let calls_before_release = server.backend.calls();
    server.backend.release();
    let published = next_frame(&mut client).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(messaged, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(stopped, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        canceled,
        RemoteResult::Error("Session not found".to_string())
    );
    for reached in ["send:session-1:hello", "cancel:session-1", "stop:session-1"] {
        assert!(calls_before_release.iter().any(|call| call == reached));
    }
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        published,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::ReviewRequest {
                review_request: RemoteReviewRequest {
                    display_id: "#7".to_string(),
                    forge: Some("GitHub".to_string()),
                    source_branch: "agentty/session-1".to_string(),
                    state: "Open".to_string(),
                    status_summary: None,
                    target_branch: "main".to_string(),
                    title: "Build it".to_string(),
                    web_url: "https://github.com/example/project/pull/7".to_string(),
                },
            }),
        }
    );
    assert!(
        server
            .backend
            .calls()
            .iter()
            .any(|call| call == "review:session-1")
    );

    Ok(())
}

#[tokio::test]
async fn a_request_beyond_eight_in_progress_is_refused() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    for id in 1..=8 {
        send_held_message(&mut client, id).await?;
    }

    // Act
    let refused = call(
        &mut client,
        9,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    server.backend.release();
    let mut held_ids = Vec::new();
    for _ in 1..=8 {
        match next_frame(&mut client).await? {
            ServerFrame::Response {
                id,
                result: RemoteResult::Ok(RemoteResponse::Accepted),
            } => held_ids.push(id),
            frame => return Err(format!("expected an accepted response, got {frame:?}").into()),
        }
    }
    held_ids.sort_unstable();
    let listed = call(
        &mut client,
        10,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    assert_eq!(
        refused,
        RemoteResult::Error("Too many requests in progress".to_string())
    );
    assert_eq!(held_ids, (1..=8).collect::<Vec<u64>>());
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

/// Takes the next `count` frames as responses and returns them sorted by
/// request id.
async fn responses(client: &mut Client, count: usize) -> TestResult<Vec<(u64, RemoteResult)>> {
    let mut responses = Vec::new();
    for _ in 0..count {
        match next_frame(client).await? {
            ServerFrame::Response { id, result } => responses.push((id, result)),
            frame => return Err(format!("expected a response, got {frame:?}").into()),
        }
    }
    responses.sort_unstable_by_key(|(id, _)| *id);

    Ok(responses)
}

#[tokio::test]
async fn mutations_of_one_session_reach_the_host_in_arrival_order() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    send_held_message(&mut client, 1).await?;
    let cancel = RemoteRequest::CancelSession {
        session_id: "session-1".into(),
    };
    send(
        &mut client,
        &ClientFrame::Request {
            id: 2,
            request: cancel,
        },
    )
    .await?;
    let listed = call(
        &mut client,
        3,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    let mutations_while_held = host_mutations(&server);
    server.backend.release();
    let replies = responses(&mut client, 2).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(mutations_while_held, Vec::<String>::new());
    assert_eq!(
        replies,
        [
            (1, RemoteResult::Ok(RemoteResponse::Accepted)),
            (2, RemoteResult::Error("Session not found".to_string())),
        ]
    );
    assert_eq!(
        host_mutations(&server),
        ["send:session-1:wait", "cancel:session-1"]
    );

    Ok(())
}

/// Returns the recorded backend calls other than the session listings that
/// run concurrently with them.
fn host_mutations(server: &TestServer) -> Vec<String> {
    server
        .backend
        .calls()
        .into_iter()
        .filter(|call| call != "list")
        .collect()
}

#[tokio::test]
async fn a_request_that_panics_gets_an_error_and_releases_its_session() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let message = |id, message: &str| ClientFrame::Request {
        id,
        request: RemoteRequest::SendMessage {
            images: Vec::new(),
            message: message.to_string(),
            session_id: "session-1".into(),
        },
    };

    // Act
    send(&mut client, &message(1, "panic")).await?;
    send(&mut client, &message(2, "next")).await?;
    let replies = responses(&mut client, 2).await?;

    // Assert
    assert_eq!(
        replies,
        [
            (1, RemoteResult::Error("Request failed".to_string())),
            (2, RemoteResult::Ok(RemoteResponse::Accepted)),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn shutdown_waits_for_requests_in_progress() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    send_held_message(&mut client, 1).await?;
    // A reply to a later request shows the held one has started.
    call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    let TestServer {
        backend,
        devices: _devices,
        handle,
        revisions_tx: _revisions_tx,
    } = server;

    // Act
    let mut shutdown = tokio::spawn(handle.shutdown());
    let finished_while_held = tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
        .await
        .is_ok();
    backend.release();
    shutdown.await?;

    // Assert
    assert!(!finished_while_held);
    assert_eq!(backend.calls(), ["list", "list", "send:session-1:wait"]);

    Ok(())
}

#[tokio::test]
async fn mutations_of_unlisted_sessions_never_reach_the_host() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let session_id = RequestedSessionId::from("other-project-session");

    // Act
    let sent = call(
        &mut client,
        1,
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "continue".to_string(),
            session_id: session_id.clone(),
        },
    )
    .await?;
    let answered = call(
        &mut client,
        2,
        RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            graph_question_target: None,
            session_id: session_id.clone(),
        },
    )
    .await?;
    let canceled = call(
        &mut client,
        3,
        RemoteRequest::CancelSession {
            session_id: session_id.clone(),
        },
    )
    .await?;
    let stopped = call(
        &mut client,
        4,
        RemoteRequest::StopTurn {
            session_id: session_id.clone(),
        },
    )
    .await?;
    let synced = call(&mut client, 5, RemoteRequest::SyncSession { session_id }).await?;

    // Assert
    let not_found = RemoteResult::Error("Session not found".to_string());
    assert_eq!(sent, not_found);
    assert_eq!(answered, not_found);
    assert_eq!(canceled, not_found);
    assert_eq!(stopped, not_found);
    assert_eq!(synced, not_found);
    assert_eq!(server.backend.calls(), vec!["list"; 5]);

    Ok(())
}

#[tokio::test]
async fn settings_reach_the_host_and_a_bad_value_never_does() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let settings = |reasoning: &str| RemoteRequest::SetSessionSettings {
        agent: None,
        model: None,
        reasoning: Some(reasoning.to_string()),
        response_style: None,
        session_id: "session-1".into(),
        speed_mode: None,
    };

    // Act
    let changed = call(&mut client, 1, settings("low")).await?;
    let refused = call(&mut client, 2, settings("huge")).await?;

    // Assert
    assert_eq!(changed, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        refused,
        RemoteResult::Error("unknown reasoning level: huge".to_string())
    );
    // The session is checked before the values, so the refused change
    // reaches the listing but never the settings.
    assert_eq!(
        server.backend.calls(),
        ["list", "settings:session-1:low", "list"]
    );

    Ok(())
}

#[tokio::test]
async fn sync_session_reaches_the_host_and_returns_its_refusal() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let synced = call(
        &mut client,
        1,
        RemoteRequest::SyncSession {
            session_id: "session-1".into(),
        },
    )
    .await?;

    // Assert
    assert_eq!(
        synced,
        RemoteResult::Error("Session cannot sync in status `Done`".to_string())
    );
    assert_eq!(server.backend.calls(), ["list", "sync:session-1"]);

    Ok(())
}

#[tokio::test]
async fn sync_project_reaches_the_host_and_the_listing_reports_its_progress() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let synced = call(
        &mut client,
        1,
        RemoteRequest::SyncProject { project_id: None },
    )
    .await?;
    let listed = call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    let listed_project = match listed {
        RemoteResult::Ok(RemoteResponse::Sessions { project, .. }) => project,
        _ => None,
    };
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        listed_project,
        Some(project(Some(RemoteProjectSync {
            branch: "main".to_string(),
            conflicts: None,
            message: None,
            phase: RemoteProjectSyncPhase::Running,
            pulled_commits: None,
            pushed_commits: None,
        })))
    );
    assert_eq!(server.backend.calls(), ["sync_project", "list"]);

    Ok(())
}

#[tokio::test]
async fn stop_turn_reaches_the_host_for_a_listed_session() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let stopped = call(
        &mut client,
        1,
        RemoteRequest::StopTurn {
            session_id: "session-1".into(),
        },
    )
    .await?;

    // Assert
    assert_eq!(stopped, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(server.backend.calls(), ["list", "stop:session-1"]);

    Ok(())
}

#[tokio::test]
async fn merge_reaches_the_host_only_for_listed_sessions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let refused = call(
        &mut client,
        1,
        RemoteRequest::MergeSession {
            session_id: "session-1".into(),
        },
    )
    .await?;
    let unlisted = call(
        &mut client,
        2,
        RemoteRequest::MergeSession {
            session_id: "other-project-session".into(),
        },
    )
    .await?;

    // Assert
    assert_eq!(
        refused,
        RemoteResult::Error("Session is not ready to merge".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(server.backend.calls(), ["list", "merge:session-1", "list"]);

    Ok(())
}

#[tokio::test]
async fn new_sessions_go_to_the_active_project_in_a_served_mode() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let host = call(&mut client, 1, RemoteRequest::GetHost { project_id: None }).await?;
    let created = call(
        &mut client,
        2,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: None,
        },
    )
    .await?;
    let harness = call(
        &mut client,
        3,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Harness,
            project_id: None,
        },
    )
    .await?;

    // Assert
    assert_eq!(
        host,
        RemoteResult::Ok(RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            mode_models: vec![RemoteModeModel {
                agent: "codex".to_string(),
                mode: RemoteSessionMode::Regular,
                model: "gpt-6.1-sol".to_string(),
                name: "gpt-6.1-sol".to_string(),
                reasoning: Some("high".to_string()),
                response_style: Some("balanced".to_string()),
                speed_mode: Some("normal".to_string()),
            }],
            models: vec![RemoteModelOption::from(
                ag_session::test_support::codex_selection()
            )],
            project: "agentty".to_string(),
            project_id: Some("1".to_string()),
            session_modes: vec![RemoteSessionMode::Regular],
        })
    );
    assert_eq!(
        created,
        RemoteResult::Ok(RemoteResponse::Created {
            session_id: "session-2".to_string(),
        })
    );
    assert_eq!(
        harness,
        RemoteResult::Error("Session mode is not available".to_string())
    );
    assert_eq!(
        server.backend.calls(),
        ["host", "host", "create:Regular:1", "host"]
    );

    Ok(())
}

#[tokio::test]
async fn pings_are_ignored_between_requests() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    client.send(Message::Ping(vec![1].into())).await?;
    client.send(Message::Pong(vec![2].into())).await?;
    let listed = call(
        &mut client,
        1,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

#[tokio::test]
async fn protocol_violations_after_authentication_close_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut repeated_hello, _device_id, token) = server.pair("Phone").await?;
    let (mut invalid, _welcome) = server.hello(&token).await?;
    let (mut binary, _welcome) = server.hello(&token).await?;

    // Act
    send(&mut repeated_hello, &hello_frame(&token)).await?;
    invalid.send(Message::Text("[]".into())).await?;
    binary.send(Message::Binary(vec![0].into())).await?;

    // Assert
    assert_rejected(&mut repeated_hello, "Connection is already authenticated").await?;
    assert_rejected(&mut invalid, "Invalid frame").await?;
    assert_rejected(&mut binary, "Binary frames are not supported").await
}

#[tokio::test]
async fn unknown_methods_get_an_error_and_keep_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let unknown = r#"{"type":"request","id":1,"request":{"method":"rename_session","session_id":"session-1"}}"#;

    // Act
    client.send(Message::Text(unknown.into())).await?;
    let refused = next_frame(&mut client).await?;
    let listed = call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    assert_eq!(
        refused,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Error("Unsupported request".to_string()),
        }
    );
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(server.backend.calls(), ["list"]);

    Ok(())
}

#[tokio::test]
async fn bursts_of_host_changes_push_one_changed_frame() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    // Pausing only after pairing keeps the clock from skipping past the
    // pairing round trips while loopback I/O is in flight.
    tokio::time::pause();
    let started = tokio::time::Instant::now();

    // Act
    server.revisions_tx.send_replace(2);
    server.revisions_tx.send_replace(3);
    let first = next_untimed_frame(&mut client).await?;
    let first_elapsed = started.elapsed();
    server.revisions_tx.send_replace(4);
    let second = next_untimed_frame(&mut client).await?;

    // Assert
    assert_eq!(first, ServerFrame::Changed { revision: 3 });
    assert!(first_elapsed >= Duration::from_millis(250));
    assert_eq!(second, ServerFrame::Changed { revision: 4 });

    Ok(())
}

#[tokio::test]
async fn requests_keep_working_after_the_host_drops_revisions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let TestServer {
        backend: _backend,
        devices: _devices,
        handle: _handle,
        revisions_tx,
    } = server;

    // Act
    drop(revisions_tx);
    let listed = call(
        &mut client,
        1,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

/// Standard base64 of a PNG file of `bytes` bytes, larger than a frame
/// without images may be.
fn large_png(bytes: usize) -> String {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.resize(bytes, 0);

    STANDARD.encode(png)
}

#[tokio::test]
async fn a_message_with_megabytes_of_images_fits_one_frame() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let request = RemoteRequest::SendMessage {
        images: vec![
            RemoteImage {
                data: large_png(5 << 20),
            },
            RemoteImage {
                data: large_png(1 << 20),
            },
        ],
        message: "Look at [Image #1]".to_string(),
        session_id: "session-1".into(),
    };

    // Act
    let sent = call(&mut client, 1, request).await?;

    // Assert
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        server.backend.calls().last().map(String::as_str),
        Some("send:session-1:Look at [Image #1]:png=5242880,png=1048576")
    );

    Ok(())
}

/// Starts a server whose host lists the test backend's two projects, and
/// pairs a device with it.
async fn paired_with_projects() -> TestResult<(TestServer, Client, String)> {
    let server = TestServer::start_with(
        MemoryDeviceStore::default(),
        RecordingBackend::with_projects(),
    )
    .await?;
    let (client, _device_id, token) = server.pair("Phone").await?;

    Ok((server, client, token))
}

/// The requests about a project, each naming `project_id`.
fn project_requests(project_id: &str) -> Vec<RemoteRequest> {
    let named = || Some(RequestedProjectId::from(project_id));

    vec![
        RemoteRequest::GetHost {
            project_id: named(),
        },
        RemoteRequest::ListProjectFiles {
            project_id: named(),
        },
        RemoteRequest::ListSessions {
            project_id: named(),
        },
        RemoteRequest::SyncProject {
            project_id: named(),
        },
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: named(),
        },
    ]
}

/// Sends each of `requests` in turn and returns their results.
async fn call_each(
    client: &mut Client,
    requests: Vec<RemoteRequest>,
) -> TestResult<Vec<RemoteResult>> {
    let mut results = Vec::new();
    for (id, request) in (1..).zip(requests) {
        results.push(call(client, id, request).await?);
    }

    Ok(results)
}

#[tokio::test]
async fn projects_are_listed_and_named_only_through_the_projects_capability() -> TestResult {
    // Arrange
    let without = TestServer::start().await?;
    let (mut plain, _device_id, _token) = without.pair("Phone").await?;
    let (with, mut client, token) = paired_with_projects().await?;

    // Act
    let mut refused = call_each(&mut plain, project_requests("1")).await?;
    refused.push(call(&mut plain, 9, RemoteRequest::ListProjects).await?);
    let (_again, welcome) = with.hello(&token).await?;
    let listed = call(&mut client, 1, RemoteRequest::ListProjects).await?;

    // Assert: a server whose host serves only its active project refuses a
    // request that names one instead of answering for the active project.
    assert_eq!(
        refused,
        vec![RemoteResult::Error("Unsupported request".to_string()); 6]
    );
    assert_eq!(without.backend.calls(), Vec::<String>::new());
    let ServerFrame::Welcome { capabilities, .. } = welcome else {
        return Err(format!("expected a welcome, got {welcome:?}").into());
    };
    assert_eq!(capabilities.last().map(String::as_str), Some("projects"));
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Projects {
            projects: vec![
                RemoteProjectEntry {
                    active_sessions: 2,
                    id: "1".to_string(),
                    is_active: true,
                    name: "agentty".to_string(),
                },
                RemoteProjectEntry {
                    active_sessions: 1,
                    id: "2".to_string(),
                    is_active: false,
                    name: "website".to_string(),
                },
            ],
        })
    );
    assert_eq!(with.backend.calls(), ["projects"]);

    Ok(())
}

#[tokio::test]
async fn a_named_project_the_host_does_not_list_never_reaches_it() -> TestResult {
    // Arrange
    let (server, mut client, _token) = paired_with_projects().await?;
    let mut requests = project_requests("9");
    requests.extend(project_requests("website"));

    // Act
    let results = call_each(&mut client, requests).await?;

    // Assert: only the listing ran, once for each well-formed id.
    assert_eq!(
        results,
        vec![RemoteResult::Error("Project not found".to_string()); 10]
    );
    assert_eq!(server.backend.calls(), vec!["projects"; 5]);

    Ok(())
}

#[tokio::test]
async fn requests_that_name_a_listed_project_reach_that_project() -> TestResult {
    // Arrange: the terminal has `agentty` open.
    let (server, mut client, _token) = paired_with_projects().await?;

    // Act
    let results = call_each(&mut client, project_requests("2")).await?;
    let listed = call(
        &mut client,
        9,
        RemoteRequest::ListSessions {
            project_id: Some("2".into()),
        },
    )
    .await?;

    // Assert
    let website = |sync| RemoteProject {
        ahead: None,
        behind: None,
        branch: Some("site".to_string()),
        id: Some("2".to_string()),
        name: "website".to_string(),
        sync,
    };
    let RemoteResult::Ok(RemoteResponse::Host {
        base_branch,
        project,
        project_id,
        ..
    }) = &results[0]
    else {
        return Err(format!("expected the host, got {:?}", results[0]).into());
    };
    assert_eq!(
        (
            base_branch.as_str(),
            project.as_str(),
            project_id.as_deref()
        ),
        ("site", "website", Some("2"))
    );
    assert_eq!(
        results[1..],
        [
            RemoteResult::Ok(RemoteResponse::SessionFiles {
                files: vec![RemoteFileEntry {
                    is_dir: false,
                    path: "index.html".to_string(),
                }],
                truncated: false,
            }),
            RemoteResult::Ok(RemoteResponse::Sessions {
                // The host has not compared `website` with its upstream.
                project: Some(website(None)),
                sessions: vec![summary("session-3", "Review", "Write the docs", 25)],
            }),
            RemoteResult::Ok(RemoteResponse::Accepted),
            RemoteResult::Ok(RemoteResponse::Created {
                session_id: "session-3".to_string(),
            }),
        ]
    );
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Sessions {
            project: Some(website(Some(RemoteProjectSync {
                branch: "site".to_string(),
                conflicts: None,
                message: None,
                phase: RemoteProjectSyncPhase::Running,
                pulled_commits: None,
                pushed_commits: None,
            }))),
            sessions: vec![summary("session-3", "Review", "Write the docs", 25)],
        })
    );
    // Each request checked the listing before it reached the project, and
    // the new session went to the project its host description named.
    assert_eq!(
        server.backend.calls(),
        [
            "projects",
            "host:2",
            "projects",
            "project_files:2",
            "projects",
            "list:2",
            "projects",
            "sync_project:2",
            "projects",
            "host:2",
            "create:Regular:2",
            "projects",
            "list:2",
        ]
    );

    Ok(())
}

#[tokio::test]
async fn requests_without_a_project_follow_the_terminal_s_active_project() -> TestResult {
    // Arrange
    let (server, mut client, _token) = paired_with_projects().await?;

    // Act
    let before = call(
        &mut client,
        1,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    server.backend.switch_to(2);
    let after = call(
        &mut client,
        2,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await?;
    let created = call(
        &mut client,
        3,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: None,
        },
    )
    .await?;
    let synced = call(
        &mut client,
        4,
        RemoteRequest::SyncProject { project_id: None },
    )
    .await?;

    // Assert: a listing names the project it read.
    let listed_project = |result: &RemoteResult| match result {
        RemoteResult::Ok(RemoteResponse::Sessions { project, sessions }) => Some((
            project.as_ref().and_then(|project| project.id.clone()),
            sessions
                .iter()
                .map(|session| session.id.clone())
                .collect::<Vec<_>>(),
        )),
        _ => None,
    };
    assert_eq!(
        listed_project(&before),
        Some((
            Some("1".to_string()),
            vec!["session-1".to_string(), "session-2".to_string()]
        ))
    );
    assert_eq!(
        listed_project(&after),
        Some((Some("2".to_string()), vec!["session-3".to_string()]))
    );
    assert_eq!(
        created,
        RemoteResult::Ok(RemoteResponse::Created {
            session_id: "session-3".to_string(),
        })
    );
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        server.backend.calls(),
        [
            "list:1",
            "list:2",
            "host:2",
            "create:Regular:2",
            "sync_project:2"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn session_requests_ask_the_host_once_whether_it_lists_the_session() -> TestResult {
    // Arrange: `session-3` belongs to `website`, which the terminal does
    // not have open.
    let (server, mut client, _token) = paired_with_projects().await?;

    // Act
    let loaded = call(
        &mut client,
        1,
        RemoteRequest::GetSession {
            session_id: "session-3".into(),
        },
    )
    .await?;
    let sent = call(
        &mut client,
        2,
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "continue".to_string(),
            session_id: "session-3".into(),
        },
    )
    .await?;
    let unlisted = call(
        &mut client,
        3,
        RemoteRequest::StopTurn {
            session_id: "other-project-session".into(),
        },
    )
    .await?;

    // Assert
    let RemoteResult::Ok(RemoteResponse::Session {
        session: Some(session),
    }) = loaded
    else {
        return Err(format!("expected session-3, got {loaded:?}").into());
    };
    assert_eq!(session.id, "session-3");
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        server.backend.calls(),
        [
            "listed:session-3",
            "get:session-3",
            "listed:session-3",
            "send:session-3:continue",
            "listed:other-project-session",
        ]
    );

    Ok(())
}

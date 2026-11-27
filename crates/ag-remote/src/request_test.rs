use std::sync::Arc;

use ag_relay::RelayLimits;
use ag_session::{SessionDiffFile, SessionDiffFileStatus, SessionFileEntry, SessionOperation};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::{capabilities, execute};
use crate::connection::{MessageOutcome, handle_incoming};
use crate::protocol::{
    ClientFrame, RemoteDiffFile, RemoteDiffFileStatus, RemoteFileEntry, RemoteGraphAction,
    RemoteGraphActionRequest, RemoteImage, RemoteModeModel, RemoteModelOption, RemoteProject,
    RemoteProjectSync, RemoteProjectSyncPhase, RemoteQuestionAnswer, RemoteRequest, RemoteResponse,
    RemoteResult, RemoteReviewRequestMergeOutcome, RemoteSession, RemoteSessionMode,
    RemoteSessionSummary,
};
use crate::server::{MAX_MESSAGE_SIZE, ServerState};
use crate::source_support::{
    FailingEntropy, MemoryDeviceStore, RecordingBackend, ReplayEntropy, StateFixture,
};
use crate::transport::Incoming;

#[path = "request_test/policy_test.rs"]
mod policy;

fn request_message(request: RemoteRequest) -> Incoming {
    let frame = ClientFrame::Request { id: 9, request };

    Incoming::Text(serde_json::to_string(&frame).expect("frame should encode"))
}

/// Sends `request` as request `9` through the frame parser and returns the
/// result of running it.
async fn reply_result(state: &ServerState, request: RemoteRequest) -> RemoteResult {
    let request = match handle_incoming(request_message(request)) {
        MessageOutcome::Request { id: 9, request } => Some(request),
        _ => None,
    }
    .expect("expected request 9");

    execute(state.sessions(), state.entropy.as_ref(), request).await
}

#[tokio::test]
async fn reads_and_original_mutations_reach_the_host_for_listed_sessions() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let requests = vec![
        RemoteRequest::ListSessions { project_id: None },
        RemoteRequest::GetSession {
            session_id: "session-1".into(),
        },
        RemoteRequest::GetSession {
            session_id: "missing".into(),
        },
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "hi".to_string(),
            session_id: "session-1".into(),
        },
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
            graph_question_target: None,
            session_id: "session-1".into(),
        },
        RemoteRequest::CancelSession {
            session_id: "session-1".into(),
        },
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(reply_result(&fixture.state, request).await);
    }

    // Assert
    let session = crate::source_support::session_fixture(&"session-1".into());
    assert_eq!(
        results,
        [
            RemoteResult::Ok(RemoteResponse::Sessions {
                project: None,
                sessions: vec![RemoteSessionSummary {
                    id: "session-1".to_string(),
                    status: "InProgress".to_string(),
                    title: Some("Build it".to_string()),
                    updated_at: 20,
                    ..RemoteSessionSummary::default()
                }],
            }),
            RemoteResult::Ok(RemoteResponse::Session {
                session: Some(Box::new(RemoteSession::from(session))),
            }),
            RemoteResult::Ok(RemoteResponse::Session { session: None }),
            RemoteResult::Ok(RemoteResponse::Accepted),
            RemoteResult::Ok(RemoteResponse::Accepted),
            RemoteResult::Ok(RemoteResponse::Accepted),
        ]
    );
    assert_eq!(
        fixture.backend.calls(),
        [
            "list",
            "list",
            "get:session-1",
            "list",
            "list",
            "send:session-1:hi",
            "list",
            "answer:session-1:Which branch?=main:0",
            "list",
            "cancel:session-1"
        ]
    );
}

#[tokio::test]
async fn original_mutations_of_unlisted_sessions_reach_only_the_listing() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let requests = vec![
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "hi".to_string(),
            session_id: "other-project".into(),
        },
        RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            graph_question_target: None,
            session_id: "other-project".into(),
        },
        RemoteRequest::CancelSession {
            session_id: "other-project".into(),
        },
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(reply_result(&fixture.state, request).await);
    }

    // Assert
    let not_found = RemoteResult::Error("Session not found".to_string());
    assert_eq!(results, [not_found.clone(), not_found.clone(), not_found]);
    assert_eq!(fixture.backend.calls(), ["list", "list", "list"]);
}

#[tokio::test]
async fn host_errors_return_in_the_response() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::failing(),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let listed = reply_result(
        &fixture.state,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await;
    let loaded = reply_result(
        &fixture.state,
        RemoteRequest::GetSession {
            session_id: "session-1".into(),
        },
    )
    .await;

    // Assert
    assert_eq!(listed, RemoteResult::Error("backend failed".to_string()));
    assert_eq!(loaded, RemoteResult::Error("backend failed".to_string()));
    assert_eq!(fixture.backend.calls(), ["list", "list"]);
}

#[tokio::test]
async fn merge_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::MergeSession],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let merge = |session_id: &str| RemoteRequest::MergeSession {
        session_id: session_id.into(),
    };

    // Act
    let merged = reply_result(&serving.state, merge("session-1")).await;
    let unlisted = reply_result(&serving.state, merge("other-project")).await;
    let unsupported = reply_result(&not_serving.state, merge("session-1")).await;

    // Assert
    assert_eq!(merged, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "merge:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

/// Returns one modified file whose patch is `text`.
fn diff_file(path: &str, text: &str) -> SessionDiffFile {
    SessionDiffFile {
        added_lines: 1,
        deleted_lines: 1,
        old_path: None,
        patch: text.to_string(),
        path: path.to_string(),
        status: SessionDiffFileStatus::Modified,
    }
}

/// Builds a diff request for `session_id`, naming `path` when given.
fn diff_request(session_id: &str, path: Option<&str>) -> RemoteRequest {
    RemoteRequest::GetSessionDiff {
        path: path.map(str::to_string),
        session_id: session_id.into(),
    }
}

#[tokio::test]
async fn session_diff_lists_files_and_returns_one_patch_for_listed_sessions() {
    // Arrange
    let patch = "diff --git a/a.rs b/a.rs\n-old\n+new\n";
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_diff(Some(vec![diff_file("a.rs", patch)])),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let expected_file = RemoteDiffFile {
        added_lines: 1,
        deleted_lines: 1,
        old_path: None,
        path: "a.rs".to_string(),
        status: RemoteDiffFileStatus::Modified,
    };

    // Act
    let listed = reply_result(&serving.state, diff_request("session-1", None)).await;
    let opened = reply_result(&serving.state, diff_request("session-1", Some("a.rs"))).await;
    let absent = reply_result(&serving.state, diff_request("session-1", Some("b.rs"))).await;
    let unlisted = reply_result(&serving.state, diff_request("other-project", None)).await;
    let unsupported = reply_result(&not_serving.state, diff_request("session-1", None)).await;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Diff {
            files: vec![expected_file.clone()],
            patch: None,
            truncated: false,
        })
    );
    assert_eq!(
        opened,
        RemoteResult::Ok(RemoteResponse::Diff {
            files: vec![expected_file],
            patch: Some(patch.to_string()),
            truncated: false,
        })
    );
    assert_eq!(
        absent,
        RemoteResult::Error("File is not in the session diff".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "diff:session-1",
            "list",
            "diff:session-1",
            "list",
            "diff:session-1",
            "list"
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn session_graphs_are_read_for_listed_sessions_with_the_selection_asked_for() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_graphs(graphs.clone()),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let graphs_request =
        |session_id: &str, graph_id: Option<&str>| RemoteRequest::GetSessionGraphs {
            graph_id: graph_id.map(str::to_string),
            known: None,
            node_id: graph_id.map(|_| "fix".to_string()),
            session_id: session_id.into(),
        };

    // Act
    let shown = reply_result(&serving.state, graphs_request("session-1", None)).await;
    let selected = reply_result(&serving.state, graphs_request("session-1", Some("graph"))).await;
    let unlisted = reply_result(&serving.state, graphs_request("other-project", None)).await;
    let unsupported = reply_result(&not_serving.state, graphs_request("session-1", None)).await;

    // Assert
    let expected = RemoteResult::Ok(RemoteResponse::from_graphs(graphs));
    assert_eq!(shown, expected);
    assert_eq!(selected, expected);
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "graphs:session-1:-:-",
            "list",
            "graphs:session-1:graph:fix",
            "list"
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

/// `action` on node `fix` of `graph` in `session_id`, bound to the snapshot
/// `5eed`.
fn graph_action(session_id: &str, action: RemoteGraphAction) -> RemoteRequest {
    RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
        action,
        additional_calls: None,
        budget_id: None,
        graph_id: "graph".to_string(),
        message: None,
        node_id: Some("fix".to_string()),
        output_id: None,
        session_id: session_id.into(),
        snapshot: "5eed".to_string(),
    })
}

#[tokio::test]
async fn graph_actions_reach_only_listed_sessions_of_hosts_that_serve_them() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::GraphActions],
    );
    let reading_only =
        StateFixture::supporting(MemoryDeviceStore::default(), &[SessionOperation::Graphs]);
    let retry = |session_id: &str| graph_action(session_id, RemoteGraphAction::Retry);
    let extend = |budget_id: Option<&str>| {
        RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::ExtendBudget,
            additional_calls: Some(5),
            budget_id: budget_id.map(str::to_string),
            graph_id: "graph".to_string(),
            message: None,
            node_id: None,
            output_id: None,
            session_id: "session-1".into(),
            snapshot: "5eed".to_string(),
        })
    };

    // Act
    let retried = reply_result(&serving.state, retry("session-1")).await;
    let unlisted = reply_result(&serving.state, retry("other-project")).await;
    let unknown = reply_result(
        &serving.state,
        graph_action("session-1", RemoteGraphAction::Unsupported),
    )
    .await;
    let extended = reply_result(&serving.state, extend(Some("calls"))).await;
    let unnamed = reply_result(&serving.state, extend(None)).await;
    let unsupported = reply_result(&reading_only.state, retry("session-1")).await;

    // Assert
    assert_eq!(retried, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(extended, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unnamed,
        RemoteResult::Error("Graph action needs `budget_id`".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unknown,
        RemoteResult::Error("Unsupported graph action".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "graph_action:session-1:graph:5eed:Retry { node_id: \"fix\" }",
            "list",
            "list",
            "list",
            "graph_action:session-1:graph:5eed:ExtendBudget { additional_calls: 5, budget_id: \
             \"calls\" }",
            "list"
        ]
    );
    assert_eq!(reading_only.backend.calls(), Vec::<String>::new());
}

/// Asks for a graph proposal for `message` in `session_id`.
fn graph_proposal(session_id: &str, message: &str) -> RemoteRequest {
    RemoteRequest::GenerateGraphProposal {
        message: message.to_string(),
        session_id: session_id.into(),
    }
}

#[tokio::test]
async fn graph_proposals_reach_only_listed_sessions_of_hosts_that_serve_them() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::GenerateGraphProposal],
    );
    let acting_only = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::GraphActions],
    );
    let no_entropy = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::supporting(&[SessionOperation::GenerateGraphProposal]),
        Arc::new(FailingEntropy),
    );
    let split = || graph_proposal("session-1", "Split the parser");

    // Act
    let generated = reply_result(&serving.state, split()).await;
    let unlisted = reply_result(
        &serving.state,
        graph_proposal("other-project", "Split the parser"),
    )
    .await;
    let unsupported = reply_result(&acting_only.state, split()).await;
    let without_identity = reply_result(&no_entropy.state, split()).await;

    // Assert
    assert_eq!(generated, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        without_identity,
        RemoteResult::Error("Cannot generate a graph proposal identity".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list".to_string(),
            format!(
                "graph_proposal:session-1:remote-{}:Split the parser",
                "0".repeat(32)
            ),
            "list".to_string(),
        ]
    );
    assert_eq!(acting_only.backend.calls(), Vec::<String>::new());
    assert_eq!(no_entropy.backend.calls(), ["list"]);
}

#[tokio::test]
async fn graph_answers_reach_the_host_with_the_graph_work_they_answer() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let older = r#"{"type":"request","id":9,"request":{"method":"answer_questions","answers":[{"answer":"Local","question":"Where?"}],"session_id":"session-1"}}"#;

    // Act
    let targeted = reply_result(
        &fixture.state,
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "Local".to_string(),
                question: "Where?".to_string(),
            }],
            graph_question_target: Some("c0ffee".to_string()),
            session_id: "session-1".into(),
        },
    )
    .await;
    let request = match handle_incoming(Incoming::Text(older.to_string())) {
        MessageOutcome::Request { request, .. } => Some(request),
        _ => None,
    }
    .expect("an older answer frame should parse");
    let untargeted = execute(
        fixture.state.sessions(),
        fixture.state.entropy.as_ref(),
        request,
    )
    .await;

    // Assert
    assert_eq!(targeted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(untargeted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        fixture.backend.calls(),
        [
            "list",
            "answer:session-1:Where?=Local:0@c0ffee",
            "list",
            "answer:session-1:Where?=Local:0"
        ]
    );
}

#[tokio::test]
async fn session_graphs_answer_unchanged_only_for_the_version_the_device_shows() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_graphs(graphs.clone()),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let full = RemoteResponse::from_graphs(graphs);
    let version = match &full {
        RemoteResponse::Graphs { version, .. } => version.clone(),
        _ => String::new(),
    };
    let graphs_request = |known: Option<&str>| RemoteRequest::GetSessionGraphs {
        graph_id: None,
        known: known.map(str::to_string),
        node_id: None,
        session_id: "session-1".into(),
    };

    // Act
    let unchanged = reply_result(&fixture.state, graphs_request(Some(&version))).await;
    let stale = reply_result(&fixture.state, graphs_request(Some("stale"))).await;
    let fresh = reply_result(&fixture.state, graphs_request(None)).await;

    // Assert
    assert_ne!(version, "");
    assert_eq!(
        unchanged,
        RemoteResult::Ok(RemoteResponse::Graphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
            unchanged: true,
            version,
        })
    );
    assert_eq!(stale, RemoteResult::Ok(full.clone()));
    assert_eq!(fresh, RemoteResult::Ok(full));
}

#[tokio::test]
async fn session_diff_of_a_session_the_host_no_longer_loads_is_not_found() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_diff(None),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let result = reply_result(&fixture.state, diff_request("session-1", None)).await;

    // Assert
    assert_eq!(result, RemoteResult::Error("Session not found".to_string()));
}

/// Builds a file-listing request for `session_id`.
fn files_request(session_id: &str) -> RemoteRequest {
    RemoteRequest::ListSessionFiles {
        session_id: session_id.into(),
    }
}

#[tokio::test]
async fn session_files_list_the_worktree_of_listed_sessions_only() {
    // Arrange
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_files(Some(vec![
            SessionFileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            SessionFileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ])),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());

    // Act
    let listed = reply_result(&serving.state, files_request("session-1")).await;
    let unlisted = reply_result(&serving.state, files_request("other-project")).await;
    let unsupported = reply_result(&not_serving.state, files_request("session-1")).await;

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
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "files:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn session_files_of_a_session_the_host_no_longer_loads_are_not_found() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_files(None),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let result = reply_result(&fixture.state, files_request("session-1")).await;

    // Assert
    assert_eq!(result, RemoteResult::Error("Session not found".to_string()));
}

#[tokio::test]
async fn project_files_list_the_project_checkout_only_when_the_host_serves_them() {
    // Arrange
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_project_files(vec![SessionFileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        }]),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());

    // Act
    let listed = reply_result(
        &serving.state,
        RemoteRequest::ListProjectFiles { project_id: None },
    )
    .await;
    let unsupported = reply_result(
        &not_serving.state,
        RemoteRequest::ListProjectFiles { project_id: None },
    )
    .await;

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
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["project_files"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn get_host_describes_the_project_and_only_remote_modes() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::HostContext],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());

    // Act
    let host = reply_result(&serving.state, RemoteRequest::GetHost { project_id: None }).await;
    let unsupported = reply_result(
        &not_serving.state,
        RemoteRequest::GetHost { project_id: None },
    )
    .await;

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
                speed_mode: Some("fast".to_string()),
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
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["host"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn create_session_uses_the_active_project_and_a_served_mode() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::CreateSession],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let create = |mode| RemoteRequest::CreateSession {
        mode,
        project_id: None,
    };

    // Act
    let created = reply_result(&serving.state, create(RemoteSessionMode::Regular)).await;
    let harness = reply_result(&serving.state, create(RemoteSessionMode::Harness)).await;
    let unsupported = reply_result(&not_serving.state, create(RemoteSessionMode::Regular)).await;

    // Assert
    assert_eq!(
        created,
        RemoteResult::Ok(RemoteResponse::Created {
            session_id: "created".to_string(),
        })
    );
    assert_eq!(
        harness,
        RemoteResult::Error("Session mode is not available".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        ["host", "create:Regular:1", "host"]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn stop_turn_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving =
        StateFixture::supporting(MemoryDeviceStore::default(), &[SessionOperation::StopTurn]);
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let stop = |session_id: &str| RemoteRequest::StopTurn {
        session_id: session_id.into(),
    };

    // Act
    let stopped = reply_result(&serving.state, stop("session-1")).await;
    let unlisted = reply_result(&serving.state, stop("other-project")).await;
    let unsupported = reply_result(&not_serving.state, stop("session-1")).await;

    // Assert
    assert_eq!(stopped, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "stop:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

/// Standard base64 of a file that starts as a PNG does.
const PNG_BASE64: &str = "iVBORw0KGgo=";
/// Standard base64 of a file that starts as a JPEG does.
const JPEG_BASE64: &str = "/9j/4A==";

/// A `send_message` request to `session-1` with one image per item of
/// `images`, each standard base64.
fn image_message(images: &[&str]) -> RemoteRequest {
    RemoteRequest::SendMessage {
        images: images
            .iter()
            .map(|data| RemoteImage {
                data: (*data).to_string(),
            })
            .collect(),
        message: "Look at [Image #1]".to_string(),
        session_id: "session-1".into(),
    }
}

#[tokio::test]
async fn message_images_reach_the_host_decoded_only_when_it_serves_them() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::MessageImages],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());

    // Act
    let sent = reply_result(&serving.state, image_message(&[PNG_BASE64, JPEG_BASE64])).await;
    let unsupported = reply_result(&not_serving.state, image_message(&[PNG_BASE64])).await;

    // Assert
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        ["list", "send:session-1:Look at [Image #1]:png,jpg"]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn message_images_beyond_the_limits_or_of_other_formats_never_reach_the_host() {
    // Arrange
    let fixture = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::MessageImages],
    );
    let large_png = STANDARD.encode([&[0x89, b'P', b'N', b'G'][..], &vec![0; 6 << 20]].concat());
    let gif = STANDARD.encode(b"GIF89a");
    let requests = [
        image_message(&[PNG_BASE64; 9]),
        image_message(&[large_png.as_str()]),
        image_message(&["not base64"]),
        image_message(&[gif.as_str()]),
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(reply_result(&fixture.state, request).await);
    }

    // Assert
    assert_eq!(
        results,
        [
            RemoteResult::Error("A message carries at most 8 images".to_string()),
            RemoteResult::Error("A message's images are at most 6 MB together".to_string()),
            RemoteResult::Error("Image data is not valid base64".to_string()),
            RemoteResult::Error("Only PNG and JPEG images can be attached".to_string()),
        ]
    );
    assert_eq!(fixture.backend.calls(), ["list"; 4]);
}

#[test]
fn the_default_relay_budget_carries_several_full_size_messages_a_minute() {
    // Arrange
    let budget = RelayLimits::default().max_bytes_per_minute;

    // Act
    let full_size_messages = budget / MAX_MESSAGE_SIZE;

    // Assert
    assert!(full_size_messages >= 4);
}

#[tokio::test]
async fn sync_session_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::RebaseSession],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let sync = |session_id: &str| RemoteRequest::SyncSession {
        session_id: session_id.into(),
    };

    // Act
    let synced = reply_result(&serving.state, sync("session-1")).await;
    let unlisted = reply_result(&serving.state, sync("other-project")).await;
    let unsupported = reply_result(&not_serving.state, sync("session-1")).await;

    // Assert
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "sync:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn settings_reach_the_host_only_as_a_valid_change_to_a_listed_session() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::SessionSettings],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let codex_model = ag_session::test_support::CODEX_MODEL.as_str();
    let settings =
        |session_id: &str, model: &str, reasoning: &str| RemoteRequest::SetSessionSettings {
            agent: None,
            model: Some(model.to_string()),
            reasoning: Some(reasoning.to_string()),
            response_style: Some("concise".to_string()),
            session_id: session_id.into(),
            speed_mode: Some("fast".to_string()),
        };
    let on_agent = |agent: &str| RemoteRequest::SetSessionSettings {
        agent: Some(agent.to_string()),
        model: Some(codex_model.to_string()),
        reasoning: None,
        response_style: None,
        session_id: "session-1".into(),
        speed_mode: None,
    };
    let reasoning_only = RemoteRequest::SetSessionSettings {
        agent: None,
        model: None,
        reasoning: Some("low".to_string()),
        response_style: None,
        session_id: "session-1".into(),
        speed_mode: None,
    };

    // Act
    let changed = reply_result(&serving.state, settings("session-1", codex_model, "max")).await;
    let partial = reply_result(&serving.state, reasoning_only).await;
    let other_agent = reply_result(&serving.state, on_agent("harness")).await;
    let unknown_agent = reply_result(&serving.state, on_agent("vi")).await;
    let unknown_model = reply_result(&serving.state, settings("session-1", "vim", "max")).await;
    let unknown_level =
        reply_result(&serving.state, settings("session-1", codex_model, "huge")).await;
    let unlisted = reply_result(
        &serving.state,
        settings("other-project", codex_model, "max"),
    )
    .await;
    let unlisted_and_invalid =
        reply_result(&serving.state, settings("other-project", "vim", "max")).await;
    let unsupported = reply_result(
        &not_serving.state,
        settings("session-1", codex_model, "max"),
    )
    .await;

    // Assert
    assert_eq!(changed, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(partial, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(other_agent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unknown_agent,
        RemoteResult::Error("unknown agent kind: vi".to_string())
    );
    assert_eq!(
        unknown_model,
        RemoteResult::Error("unknown model: vim".to_string())
    );
    assert_eq!(
        unknown_level,
        RemoteResult::Error("unknown reasoning level: huge".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    // The listed check comes before field validation, so an unlisted
    // session answers alike whatever settings it carries.
    assert_eq!(
        unlisted_and_invalid,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list".to_string(),
            format!("settings:session-1:-:{codex_model}:max:concise:fast"),
            "list".to_string(),
            "settings:session-1:-:-:low:-:-".to_string(),
            "list".to_string(),
            format!("settings:session-1:harness:{codex_model}:-:-:-"),
            "list".to_string(),
            "list".to_string(),
            "list".to_string(),
            "list".to_string(),
            "list".to_string(),
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn publish_returns_host_refusals_and_needs_the_host_to_serve_it() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::PublishSession],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());
    let publish = || RemoteRequest::PublishSession {
        session_id: "session-1".into(),
    };

    // Act
    let refused = reply_result(&serving.state, publish()).await;
    let unsupported = reply_result(&not_serving.state, publish()).await;

    // Assert
    assert_eq!(refused, RemoteResult::Error("unsupported".to_string()));
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "review:session-1"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn review_request_merge_answers_the_forge_outcome_and_passes_bypass_through() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::MergeReviewRequest],
    );
    let merge = |bypass| RemoteRequest::MergeReviewRequest {
        bypass,
        session_id: "session-1".into(),
    };

    // Act
    let bypassed = reply_result(&serving.state, merge(true)).await;
    let refused = reply_result(&serving.state, merge(false)).await;

    // Assert
    assert_eq!(
        bypassed,
        RemoteResult::Ok(RemoteResponse::ReviewRequestMerge {
            merge_commit: Some("7a1c0de".to_string()),
            outcome: RemoteReviewRequestMergeOutcome::Merged,
            reason: None,
            review_request: None,
        })
    );
    assert_eq!(
        refused,
        RemoteResult::Ok(RemoteResponse::ReviewRequestMerge {
            merge_commit: None,
            outcome: RemoteReviewRequestMergeOutcome::Refused,
            reason: Some(
                "Pull request #7 is no longer ready to merge (Checks pending)".to_string()
            ),
            review_request: None,
        })
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "merge_review_request:session-1:true",
            "list",
            "merge_review_request:session-1:false",
        ]
    );
}

#[tokio::test]
async fn review_request_merge_never_reaches_a_host_that_does_not_serve_it() {
    // Arrange
    let not_serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[
            SessionOperation::MergeSession,
            SessionOperation::PublishSession,
        ],
    );

    // Act
    let unsupported = reply_result(
        &not_serving.state,
        RemoteRequest::MergeReviewRequest {
            bypass: true,
            session_id: "session-1".into(),
        },
    )
    .await;
    let offered = capabilities(not_serving.state.sessions());

    // Assert
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(offered, ["merge", "publish"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn sync_project_reaches_the_host_only_when_the_host_serves_it() {
    // Arrange
    let serving = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::SyncProject],
    );
    let not_serving = StateFixture::with_devices(MemoryDeviceStore::default());

    // Act
    let synced = reply_result(
        &serving.state,
        RemoteRequest::SyncProject { project_id: None },
    )
    .await;
    let unsupported = reply_result(
        &not_serving.state,
        RemoteRequest::SyncProject { project_id: None },
    )
    .await;

    // Assert
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["sync_project"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn list_sessions_describes_the_project_when_the_host_reports_it() {
    // Arrange
    let reporting = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::ProjectStatus],
    );

    // Act
    let listed = reply_result(
        &reporting.state,
        RemoteRequest::ListSessions { project_id: None },
    )
    .await;

    // Assert
    let project = match listed {
        RemoteResult::Ok(RemoteResponse::Sessions { project, .. }) => project,
        _ => None,
    };
    assert_eq!(
        project,
        Some(RemoteProject {
            ahead: Some(1),
            behind: Some(2),
            branch: Some("main".to_string()),
            id: None,
            name: "agentty".to_string(),
            sync: Some(RemoteProjectSync {
                branch: "main".to_string(),
                conflicts: Some(3),
                message: None,
                phase: RemoteProjectSyncPhase::ResolvingConflicts,
                pulled_commits: None,
                pushed_commits: None,
            }),
        })
    );
    assert_eq!(reporting.backend.calls(), ["list", "project"]);
}

#[test]
fn projects_are_offered_only_by_a_host_that_also_reports_project_status() {
    // Arrange
    let without_status =
        StateFixture::supporting(MemoryDeviceStore::default(), &[SessionOperation::Projects]);
    let with_status = StateFixture::supporting(
        MemoryDeviceStore::default(),
        &[SessionOperation::Projects, SessionOperation::ProjectStatus],
    );

    // Act
    let offered_without = capabilities(without_status.state.sessions());
    let offered_with = capabilities(with_status.state.sessions());

    // Assert: a page tells a project's listing apart only by its status.
    assert_eq!(offered_without, Vec::<String>::new());
    assert_eq!(offered_with, ["projects"]);
}

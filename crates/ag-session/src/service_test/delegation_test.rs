//! Session service delegation tests for backend reads and mutating operations.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ag_protocol::TurnPrompt;
use async_trait::async_trait;

use super::support::{FakeBackend, FakeBackendState, review_request_fixture, session_fixture};
use crate::error::SessionError;
use crate::model::{ReviewRequest, Session, SessionId};
use crate::service::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, HostProjectStatus, HostProjectSync, HostProjectSyncPhase,
    MessageImage, MessageImageFormat, QuestionAnswer, SessionBackend, SessionOperation,
    SessionService, SessionSettingsChange,
};

/// Smallest prefix recognized as a PNG file.
const PNG_PREFIX: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];
/// Smallest prefix recognized as a JPEG file.
const JPEG_PREFIX: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
use crate::{
    GenerateGraphProposalRequest, ReasoningLevel, SessionGraphAction, SessionGraphActionRequest,
    SessionGraphs, SessionGraphsRequest,
};

#[tokio::test]
async fn service_delegates_create_and_get() {
    // Arrange
    let expected_session = session_fixture();
    let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
        create_results: VecDeque::from([Ok(SessionId::from("session-1"))]),
        get_result: Some(Ok(Some(expected_session.clone()))),
        ..FakeBackendState::default()
    }));
    let service = SessionService::new(backend.clone());

    // Act
    let session_id = service
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode: CreateSessionMode::Regular,
            project_id: 7,
        })
        .await
        .expect("session should be created");
    let loaded_session = service
        .get_session(&session_id)
        .await
        .expect("session should load");

    // Assert
    assert_eq!(loaded_session, Some(expected_session));
    assert_eq!(backend.calls(), ["create:Regular"]);
}

#[tokio::test]
async fn service_delegates_mutating_operations_through_clones() {
    // Arrange
    let expected_review_request = review_request_fixture();
    let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
        review_result: Some(Ok(expected_review_request.clone())),
        unit_results: VecDeque::from([Ok(()), Ok(()), Ok(()), Ok(()), Ok(())]),
        ..FakeBackendState::default()
    }));
    let session_id = SessionId::from("session-1");
    let service = SessionService::new(backend.clone());
    let cloned_service = service.clone();
    let answers = AnswerQuestionsRequest {
        answers: vec![QuestionAnswer {
            answer: "main".to_string(),
            question: "Which branch?".to_string(),
        }],
        attachments: Vec::new(),
        graph_question_target: None,
    };

    // Act
    service
        .send_message(&session_id, "continue")
        .await
        .expect("message should be sent");
    service
        .submit_durable_message(
            &session_id,
            DurableMessageRequest {
                message: "roll up".to_string(),
                operation_id: "rollup-7".to_string(),
                visibility: DurableMessageVisibility::Hidden,
            },
        )
        .await
        .expect("coordinator message should be submitted");
    cloned_service
        .answer_questions(&session_id, answers)
        .await
        .expect("questions should be answered");
    service
        .cancel_session(&session_id)
        .await
        .expect("cancel should be requested");
    cloned_service
        .merge_session(&session_id)
        .await
        .expect("merge should be requested");
    let review_request = service
        .create_review_request(&session_id)
        .await
        .expect("review request should be created");

    // Assert
    assert_eq!(review_request, expected_review_request);
    assert_eq!(
        backend.calls(),
        [
            "send:session-1:continue",
            "submit-coordinator:session-1:rollup-7:roll up",
            "answer:session-1:1",
            "cancel:session-1",
            "merge:session-1",
            "review:session-1"
        ]
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_listing() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let listed = service.list_sessions().await;

    // Assert
    assert_eq!(
        listed,
        Err(SessionError::Operation(
            "Host does not support session listing".to_string()
        ))
    );
}

#[tokio::test]
async fn hosts_without_listing_report_the_listing_error_for_entries() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let listed = service.list_session_entries().await;

    // Assert
    assert_eq!(
        listed,
        Err(SessionError::Operation(
            "Host does not support session listing".to_string()
        ))
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_diffs() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let diff = service
        .get_session_diff(&SessionId::from("session-1"))
        .await;
    let supports_diff = service.supports(SessionOperation::SessionDiff);

    // Assert
    assert_eq!(
        diff,
        Err(SessionError::Operation(
            "Host does not serve session diffs".to_string()
        ))
    );
    assert!(!supports_diff);
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_files() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let files = service
        .list_session_files(&SessionId::from("session-1"))
        .await;
    let supports_files = service.supports(SessionOperation::SessionFiles);

    // Assert
    assert_eq!(
        files,
        Err(SessionError::Operation(
            "Host does not serve session files".to_string()
        ))
    );
    assert!(!supports_files);
}

#[tokio::test]
async fn existing_hosts_report_unsupported_project_files() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let files = service.list_project_files().await;
    let supports_files = service.supports(SessionOperation::ProjectFiles);

    // Assert
    assert_eq!(
        files,
        Err(SessionError::Operation(
            "Host does not serve project files".to_string()
        ))
    );
    assert!(!supports_files);
}

#[test]
fn existing_hosts_serve_no_optional_operations() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let supports_merge = service.supports(SessionOperation::MergeSession);
    let supports_stop = service.supports(SessionOperation::StopTurn);

    // Assert
    assert!(!supports_merge);
    assert!(!supports_stop);
}

#[test]
fn message_images_are_recognized_by_their_signature() {
    // Arrange
    let png = PNG_PREFIX.to_vec();
    let jpeg = JPEG_PREFIX.to_vec();

    // Act
    let png_image = MessageImage::new(png.clone()).expect("PNG image");
    let jpeg_image = MessageImage::new(jpeg.clone()).expect("JPEG image");

    // Assert
    assert_eq!(png_image.format(), MessageImageFormat::Png);
    assert_eq!(png_image.format().extension(), "png");
    assert_eq!(png_image.data(), png.as_slice());
    assert_eq!(jpeg_image.format(), MessageImageFormat::Jpeg);
    assert_eq!(jpeg_image.format().extension(), "jpg");
    assert_eq!(jpeg_image.into_data(), jpeg);
}

#[test]
fn message_images_refuse_other_files() {
    // Arrange
    let gif = b"GIF89a".to_vec();

    // Act
    let refused = MessageImage::new(gif);

    // Assert
    assert_eq!(
        refused,
        Err(SessionError::Operation(
            "Only PNG and JPEG images can be attached".to_string()
        ))
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_message_images() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let service = SessionService::new(backend.clone());
    let image = MessageImage::new(PNG_PREFIX.to_vec()).expect("PNG image");

    // Act
    let supported = service.supports(SessionOperation::MessageImages);
    let sent = service
        .send_message_with_images(&SessionId::from("session-1"), "Look", vec![image])
        .await;

    // Assert
    assert!(!supported);
    assert_eq!(
        sent,
        Err(SessionError::Operation(
            "Host does not support images in messages".to_string()
        ))
    );
    assert_eq!(backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn service_delegates_message_images_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());
    let image = MessageImage::new(PNG_PREFIX.to_vec()).expect("PNG image");

    // Act
    let supported = service.supports(SessionOperation::MessageImages);
    let sent = service
        .send_message_with_images(&SessionId::from("session-1"), "Look", vec![image.clone()])
        .await;

    // Assert
    assert!(supported);
    assert_eq!(sent, Ok(()));
    assert_eq!(
        backend
            .image_messages
            .lock()
            .expect("image messages")
            .as_slice(),
        [(
            SessionId::from("session-1"),
            "Look".to_string(),
            vec![image]
        )]
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_turn_stop() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supported = service.supports(SessionOperation::StopTurn);
    let stopped = service.stop_turn(&SessionId::from("session-1")).await;

    // Assert
    assert!(!supported);
    assert_eq!(
        stopped,
        Err(SessionError::Operation(
            "Host does not support stopping a turn".to_string()
        ))
    );
    assert_eq!(backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn service_delegates_turn_stop_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supported = service.supports(SessionOperation::StopTurn);
    let stopped = service.stop_turn(&SessionId::from("session-1")).await;

    // Assert
    assert!(supported);
    assert_eq!(stopped, Ok(()));
    assert_eq!(
        backend.stopped.lock().expect("stopped sessions").as_slice(),
        [SessionId::from("session-1")]
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_sync() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supported = service.supports(SessionOperation::RebaseSession);
    let synced = service.rebase_session(&SessionId::from("session-1")).await;

    // Assert
    assert!(!supported);
    assert_eq!(
        synced,
        Err(SessionError::Operation(
            "Host does not support syncing a session branch".to_string()
        ))
    );
    assert_eq!(backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn service_delegates_session_sync_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supported = service.supports(SessionOperation::RebaseSession);
    let synced = service.rebase_session(&SessionId::from("session-1")).await;

    // Assert
    assert!(supported);
    assert_eq!(synced, Ok(()));
    assert_eq!(
        backend.synced.lock().expect("synced sessions").as_slice(),
        [SessionId::from("session-1")]
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_settings_changes() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supported = service.supports(SessionOperation::SessionSettings);
    let changed = service
        .update_session_settings(
            &SessionId::from("session-1"),
            SessionSettingsChange::default(),
        )
        .await;

    // Assert
    assert!(!supported);
    assert_eq!(
        changed,
        Err(SessionError::Operation(
            "Host does not support changing session settings".to_string()
        ))
    );
    assert_eq!(backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn service_delegates_settings_changes_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());
    let change = SessionSettingsChange {
        reasoning_level: Some(ReasoningLevel::Low),
        ..SessionSettingsChange::default()
    };

    // Act
    let supported = service.supports(SessionOperation::SessionSettings);
    let changed = service
        .update_session_settings(&SessionId::from("session-1"), change)
        .await;

    // Assert
    assert!(supported);
    assert_eq!(changed, Ok(()));
    assert_eq!(
        backend
            .settings_changes
            .lock()
            .expect("settings changes")
            .as_slice(),
        [(SessionId::from("session-1"), change)]
    );
}

/// Host that serves the optional operations and records each call.
#[derive(Default)]
struct OptionalBackend {
    /// Graph proposals asked for, in order.
    generations: Mutex<Vec<GenerateGraphProposalRequest>>,
    /// Graph actions asked for, in order.
    graph_actions: Mutex<Vec<SessionGraphActionRequest>>,
    /// Graph reads asked for, in order.
    graph_reads: Mutex<Vec<SessionGraphsRequest>>,
    /// Messages sent with images, in order.
    image_messages: Mutex<Vec<(SessionId, String, Vec<MessageImage>)>>,
    inner: FakeBackend,
    /// Whether a project sync was asked for and still runs.
    is_project_syncing: Mutex<bool>,
    settings_changes: Mutex<Vec<(SessionId, SessionSettingsChange)>>,
    stopped: Mutex<Vec<SessionId>>,
    synced: Mutex<Vec<SessionId>>,
}

#[async_trait]
impl SessionBackend for OptionalBackend {
    fn supports(&self, operation: SessionOperation) -> bool {
        matches!(
            operation,
            SessionOperation::StopTurn
                | SessionOperation::RebaseSession
                | SessionOperation::SessionSettings
                | SessionOperation::ProjectStatus
                | SessionOperation::SyncProject
                | SessionOperation::Graphs
                | SessionOperation::GraphActions
                | SessionOperation::GenerateGraphProposal
                | SessionOperation::MessageImages
        )
    }

    async fn send_message_with_images(
        &self,
        session_id: &SessionId,
        message: String,
        images: Vec<MessageImage>,
    ) -> Result<(), SessionError> {
        self.image_messages.lock().expect("image messages").push((
            session_id.clone(),
            message,
            images,
        ));

        Ok(())
    }

    async fn generate_graph_proposal(
        &self,
        request: GenerateGraphProposalRequest,
    ) -> Result<(), SessionError> {
        self.generations.lock().expect("generations").push(request);

        Ok(())
    }

    async fn act_on_session_graph(
        &self,
        request: SessionGraphActionRequest,
    ) -> Result<(), SessionError> {
        self.graph_actions
            .lock()
            .expect("graph actions")
            .push(request);

        Ok(())
    }

    // Every session has no graph and no proposal.
    async fn session_graphs(
        &self,
        request: SessionGraphsRequest,
    ) -> Result<SessionGraphs, SessionError> {
        self.graph_reads.lock().expect("graph reads").push(request);

        Ok(SessionGraphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
        })
    }

    async fn project_status(&self) -> Result<HostProjectStatus, SessionError> {
        let is_syncing = *self.is_project_syncing.lock().expect("project sync");

        Ok(HostProjectStatus {
            ahead_behind: Some((1, 2)),
            branch: Some("main".to_string()),
            project_name: "agentty".to_string(),
            sync: is_syncing.then(|| HostProjectSync {
                branch: "main".to_string(),
                phase: HostProjectSyncPhase::Running,
            }),
        })
    }

    async fn sync_project(&self) -> Result<(), SessionError> {
        *self.is_project_syncing.lock().expect("project sync") = true;

        Ok(())
    }

    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        self.settings_changes
            .lock()
            .expect("settings changes")
            .push((session_id.clone(), change));

        Ok(())
    }

    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.synced
            .lock()
            .expect("synced sessions")
            .push(session_id.clone());

        Ok(())
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.inner.create_session(request).await
    }

    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        self.inner.get_session(session_id).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        self.inner.send_message(session_id, message).await
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.inner.submit_durable_message(session_id, request).await
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        self.inner.answer_questions(session_id, request).await
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.inner.cancel_session(session_id).await
    }

    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.stopped
            .lock()
            .expect("stopped sessions")
            .push(session_id.clone());

        Ok(())
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.inner.merge_session(session_id).await
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.inner.create_review_request(session_id).await
    }
}

#[tokio::test]
async fn existing_hosts_report_unsupported_host_context() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let host = service.host_context().await;
    let supports_host = service.supports(SessionOperation::HostContext);

    // Assert
    assert_eq!(
        host,
        Err(SessionError::Operation(
            "Host does not describe its project".to_string()
        ))
    );
    assert!(!supports_host);
}

#[tokio::test]
async fn existing_hosts_report_unsupported_project_status_and_sync() {
    // Arrange
    let backend = Arc::new(FakeBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supports_status = service.supports(SessionOperation::ProjectStatus);
    let supports_sync = service.supports(SessionOperation::SyncProject);
    let status = service.project_status().await;
    let synced = service.sync_project().await;

    // Assert
    assert!(!supports_status);
    assert!(!supports_sync);
    assert_eq!(
        status,
        Err(SessionError::Operation(
            "Host does not report its project status".to_string()
        ))
    );
    assert_eq!(
        synced,
        Err(SessionError::Operation(
            "Host does not support syncing its project".to_string()
        ))
    );
    assert_eq!(backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn service_delegates_project_status_and_sync_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let supports_status = service.supports(SessionOperation::ProjectStatus);
    let supports_sync = service.supports(SessionOperation::SyncProject);
    let idle_status = service.project_status().await;
    let synced = service.sync_project().await;
    let syncing_status = service.project_status().await;

    // Assert
    assert!(supports_status);
    assert!(supports_sync);
    assert_eq!(synced, Ok(()));
    let idle_project = HostProjectStatus {
        ahead_behind: Some((1, 2)),
        branch: Some("main".to_string()),
        project_name: "agentty".to_string(),
        sync: None,
    };
    assert_eq!(idle_status, Ok(idle_project.clone()));
    assert_eq!(
        syncing_status,
        Ok(HostProjectStatus {
            sync: Some(HostProjectSync {
                branch: "main".to_string(),
                phase: HostProjectSyncPhase::Running,
            }),
            ..idle_project
        })
    );
}

#[tokio::test]
async fn existing_hosts_report_unsupported_session_graphs() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let supported = service.supports(SessionOperation::Graphs);
    let graphs = service
        .session_graphs(SessionGraphsRequest {
            graph_id: None,
            node_id: None,
            session_id: SessionId::from("session-1"),
        })
        .await;

    // Assert
    assert!(!supported);
    assert_eq!(
        graphs,
        Err(SessionError::Operation(
            "Host does not support execution graphs".to_string()
        ))
    );
}

#[tokio::test]
async fn service_delegates_session_graphs_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());
    let request = SessionGraphsRequest {
        graph_id: Some("graph".to_string()),
        node_id: Some("review".to_string()),
        session_id: SessionId::from("session-1"),
    };

    // Act
    let supported = service.supports(SessionOperation::Graphs);
    let graphs = service.session_graphs(request.clone()).await;

    // Assert
    assert!(supported);
    assert_eq!(
        graphs,
        Ok(SessionGraphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
        })
    );
    assert_eq!(
        backend.graph_reads.lock().expect("graph reads").as_slice(),
        [request]
    );
}

/// An action on `graph` of `session-1` bound to the snapshot `5eed`.
fn graph_action_request(action: SessionGraphAction) -> SessionGraphActionRequest {
    SessionGraphActionRequest {
        action,
        graph_id: "graph".to_string(),
        session_id: SessionId::from("session-1"),
        snapshot: "5eed".to_string(),
    }
}

#[tokio::test]
async fn existing_hosts_report_unsupported_graph_actions() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let supported = service.supports(SessionOperation::GraphActions);
    let result = service
        .act_on_session_graph(graph_action_request(SessionGraphAction::Cancel))
        .await;

    // Assert
    assert!(!supported);
    assert_eq!(
        result,
        Err(SessionError::Operation(
            "Host does not support execution graph actions".to_string()
        ))
    );
}

#[tokio::test]
async fn service_delegates_graph_actions_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());
    let requests = [
        graph_action_request(SessionGraphAction::Retry {
            node_id: "inspect".to_string(),
        }),
        graph_action_request(SessionGraphAction::ExtendBudget {
            additional_calls: 3,
            budget_id: "calls".to_string(),
        }),
    ];

    // Act
    let supported = service.supports(SessionOperation::GraphActions);
    for request in requests.clone() {
        service
            .act_on_session_graph(request)
            .await
            .expect("action should reach the host");
    }

    // Assert
    assert!(supported);
    assert_eq!(
        backend
            .graph_actions
            .lock()
            .expect("graph actions")
            .as_slice(),
        requests
    );
}

#[tokio::test]
async fn service_delegates_graph_proposal_generation_to_supporting_hosts() {
    // Arrange
    let backend = Arc::new(OptionalBackend::default());
    let service = SessionService::new(backend.clone());
    let request = GenerateGraphProposalRequest::from_text(
        "remote-1".to_string(),
        SessionId::from("session-1"),
        "Split the parser".to_string(),
    );

    // Act
    let supported = service.supports(SessionOperation::GenerateGraphProposal);
    service
        .generate_graph_proposal(request)
        .await
        .expect("generation should reach the host");

    // Assert
    assert!(supported);
    assert_eq!(
        backend.generations.lock().expect("generations").as_slice(),
        [GenerateGraphProposalRequest {
            expands: None,
            id: "remote-1".to_string(),
            request: TurnPrompt::from_text("Split the parser".to_string()),
            session_id: SessionId::from("session-1"),
        }]
    );
}

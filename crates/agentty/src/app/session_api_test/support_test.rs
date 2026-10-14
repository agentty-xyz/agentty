use std::sync::Arc;

use ag_contracts::{AgentRequestKind, PermissionMode, ReasoningLevel, SpeedMode};
use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, QuestionAnswer,
    ReviewRequest, SessionError as ApiSessionError, SessionId, SessionMessageKind,
    test_support as model_fixture,
};
use ag_worker::test_support::{AppServerTurnResponse, MockAppServerClient};

use crate::app::App;
use crate::infra::db::{SessionReviewRequestRow, SessionRow};

pub(super) async fn request_session_creation(
    app: &mut App,
    request: CreateSessionRequest,
) -> Result<SessionId, ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.create_session(request).await })
        .await
}

pub(super) async fn request_session(
    app: &mut App,
    session_id: SessionId,
) -> Result<Option<ag_session::Session>, ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.get_session(&session_id).await })
        .await
}

pub(super) async fn request_message(
    app: &mut App,
    session_id: SessionId,
    message: &str,
) -> Result<(), ApiSessionError> {
    let service = app.session_service();
    let message = message.to_string();

    app.drive_session_request(async move { service.send_message(&session_id, message).await })
        .await
}

pub(super) async fn request_durable_message(
    app: &mut App,
    session_id: SessionId,
    request: DurableMessageRequest,
) -> Result<(), ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(
        async move { service.submit_durable_message(&session_id, request).await },
    )
    .await
}

pub(super) async fn request_question_answers(
    app: &mut App,
    session_id: SessionId,
    request: AnswerQuestionsRequest,
) -> Result<(), ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.answer_questions(&session_id, request).await })
        .await
}

pub(super) async fn request_cancellation(
    app: &mut App,
    session_id: SessionId,
) -> Result<(), ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.cancel_session(&session_id).await })
        .await
}

pub(super) async fn request_merge(
    app: &mut App,
    session_id: SessionId,
) -> Result<(), ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.merge_session(&session_id).await })
        .await
}

pub(super) async fn request_review_request(
    app: &mut App,
    session_id: SessionId,
) -> Result<ReviewRequest, ApiSessionError> {
    let service = app.session_service();

    app.drive_session_request(async move { service.create_review_request(&session_id).await })
        .await
}

pub(super) fn question_transition_app_server(
    first_turn_release: Arc<tokio::sync::Notify>,
    turn_started_tx: tokio::sync::mpsc::UnboundedSender<AgentRequestKind>,
) -> MockAppServerClient {
    let turn_index = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut app_server = MockAppServerClient::new();
    app_server.expect_run_turn().times(3..).returning({
            move |request, _| {
                let first_turn_release = Arc::clone(&first_turn_release);
                let request_kind = request.request_kind;
                if request_kind == AgentRequestKind::UtilityPrompt {
                    return Box::pin(async {
                        Ok(app_server_response(
                            r#"{"answer":"Initial prompt","questions":[]}"#,
                            None,
                        ))
                    });
                }

                let turn_index =
                    turn_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = turn_started_tx.send(request_kind);

                Box::pin(async move {
                    if turn_index == 0 {
                        first_turn_release.notified().await;

                        return Ok(app_server_response(
                            r#"{"answer":"Need detail","questions":[{"text":"Current question?","options":[]}]}"#,
                            Some("conversation-1"),
                        ));
                    }

                    Ok(app_server_response(
                        r#"{"answer":"ready","questions":[]}"#,
                        Some("conversation-1"),
                    ))
                })
            }
        });
    app_server
        .expect_shutdown_session()
        .times(0..)
        .returning(|_| Box::pin(async {}));

    app_server
}

pub(super) fn app_server_response(
    assistant_message: &str,
    provider_conversation_id: Option<&str>,
) -> AppServerTurnResponse {
    AppServerTurnResponse {
        assistant_message: assistant_message.to_string(),
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        pid: None,
        provider_conversation_id: provider_conversation_id.map(str::to_string),
    }
}

pub(super) fn current_question_answer(answer: &str) -> AnswerQuestionsRequest {
    AnswerQuestionsRequest {
        answers: vec![QuestionAnswer {
            answer: answer.to_string(),
            question: "Current question?".to_string(),
        }],
    }
}

pub(super) fn clarification_answer_count(session: &ag_session::Session) -> usize {
    session
        .messages
        .iter()
        .filter(|message| {
            message.kind == SessionMessageKind::UserPrompt
                && message.content.starts_with("Clarifications:")
        })
        .count()
}

pub(super) fn session_row() -> SessionRow {
    SessionRow {
        added_lines: 12,
        agent: "codex".to_string(),
        base_branch: "main".to_string(),
        created_at: 10,
        deleted_lines: 4,
        has_archived_diff: false,
        has_diff: Some(true),
        id: "session-1".to_string(),
        in_progress_started_at: None,
        in_progress_total_seconds: 40,
        input_tokens: 50,
        is_draft: true,
        model: model_fixture::CODEX_MODEL_ID.to_string(),
        output_tokens: 60,
        parent_session_id: Some("parent-1".to_string()),
        permission_mode: "read_only".to_string(),
        personality_id: Some("reviewer".to_string()),
        project_id: Some(7),
        prompt: "staged prompt".to_string(),
        published_upstream_ref: Some("origin/wt/session-1".to_string()),
        questions: Some(r#"[{"text":"Which target?","options":["main","develop"]}]"#.to_string()),
        reasoning_level_override: Some("xhigh".to_string()),
        response_style: "detailed".to_string(),
        review_request: Some(SessionReviewRequestRow {
            display_id: "#42".to_string(),
            forge_kind: "GitHub".to_string(),
            last_refreshed_at: 15,
            source_branch: "wt/session-1".to_string(),
            state: "Open".to_string(),
            status_summary: Some("checks passing".to_string()),
            target_branch: "main".to_string(),
            title: "Build feature".to_string(),
            web_url: "https://example.test/pull/42".to_string(),
        }),
        size: "S".to_string(),
        speed_mode: "normal".to_string(),
        status: "Draft".to_string(),
        title: Some("Build feature".to_string()),
        updated_at: 20,
    }
}

pub(super) async fn persist_inherited_launch_settings(app: &App, session_id: &SessionId) {
    app.services
        .db()
        .sessions()
        .update_session_agent_model(session_id, "claude", "claude-sonnet-5-5")
        .await
        .expect("source agent should update");
    app.services
        .db()
        .sessions()
        .update_session_reasoning_level(session_id, ReasoningLevel::High)
        .await
        .expect("source reasoning should update");
    app.services
        .db()
        .sessions()
        .update_session_speed_mode(session_id, SpeedMode::Fast)
        .await
        .expect("source speed mode should update");
    app.services
        .db()
        .sessions()
        .update_session_permission_mode(session_id, PermissionMode::ReadOnly)
        .await
        .expect("source permission mode should update");
    app.services
        .db()
        .sessions()
        .update_session_personality_id(session_id, Some("inherited-personality".to_string()))
        .await
        .expect("source personality should update");
}

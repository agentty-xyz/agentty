//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use ag_agent::{ReasoningLevel, parse_persisted_session_agent_model};
use ag_protocol::QuestionItem;
use ag_session::{
    CreateSessionMode, CreateSessionRequest, ReviewRequest, ReviewRequestState, SessionBackend,
    SessionError as ApiSessionError, SessionId, SessionMessage, SessionMessageKind,
    SessionSettings, SessionStatus,
};
use async_trait::async_trait;

use crate::app::{App, AppError, SessionError};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

#[async_trait]
impl SessionBackend for App {
    async fn create_session(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        if request.project_id != self.active_project_id() {
            return Err(ApiSessionError::Operation(format!(
                "Project `{}` is not active",
                request.project_id
            )));
        }

        let session_id = match request.mode {
            CreateSessionMode::Regular => App::create_session(self).await,
            CreateSessionMode::Draft => self.create_draft_session().await,
            CreateSessionMode::Stacked { parent_session_id } => {
                self.create_stacked_draft_session(&parent_session_id).await
            }
        }
        .map_err(api_error_from_app)?;

        Ok(SessionId::from(session_id))
    }

    async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let Some(row) = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let message_rows = self
            .services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let queued_messages = self
            .sessions
            .session_for_id(session_id)
            .map(|session| session.queued_messages.clone())
            .unwrap_or_default();

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    async fn send_message(
        &mut self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        if message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot send an empty session message".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let is_draft = session.is_draft_session();
        let status = session.status;
        let prompt = TurnPrompt::from_text(message);

        if status == SessionStatus::Draft {
            if is_draft {
                self.stage_draft_message(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
                self.start_staged_session(session_id)
                    .await
                    .map_err(api_error_from_app)?;
            } else {
                self.start_session(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
            }

            return Ok(());
        }

        if matches!(status, SessionStatus::InProgress | SessionStatus::Rebasing) {
            return self
                .enqueue_message(session_id, prompt)
                .map_err(api_error_from_session);
        }

        if App::reply(self, session_id, prompt).await {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `{status}`"
        )))
    }

    async fn merge_session(&mut self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        App::merge_session(self, session_id)
            .await
            .map_err(api_error_from_app)
    }

    async fn create_review_request(
        &mut self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        self.sessions
            .publish_review_request(&self.services, session_id)
            .await
            .map_err(api_error_from_session)
    }
}

/// Converts complete persistence rows into the public session aggregate.
fn build_api_session(
    row: SessionRow,
    message_rows: Vec<SessionMessageRow>,
    queued_messages: Vec<String>,
) -> Result<ag_session::Session, ApiSessionError> {
    let project_id = row.project_id.ok_or_else(|| {
        ApiSessionError::InvalidData(format!("session `{}` has no project", row.id))
    })?;
    let status = row
        .status
        .parse::<SessionStatus>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let reasoning_level = row
        .reasoning_level_override
        .as_deref()
        .and_then(|value| value.parse::<ReasoningLevel>().ok())
        .unwrap_or_default();
    let messages = message_rows
        .into_iter()
        .map(api_message_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let questions = api_questions_from_json(row.questions.as_deref(), &row.id)?;
    let review_request = row
        .review_request
        .map(api_review_request_from_row)
        .transpose()?;
    let draft_prompt = (row.is_draft && !row.prompt.is_empty()).then_some(row.prompt);

    Ok(ag_session::Session {
        created_at: row.created_at,
        draft_prompt,
        id: SessionId::from(row.id),
        messages,
        published_upstream_ref: row.published_upstream_ref,
        questions,
        queued_messages,
        review_request,
        settings: SessionSettings {
            agent: parse_persisted_session_agent_model(Some(&row.agent), &row.model),
            base_branch: row.base_branch,
            is_draft: row.is_draft,
            parent_session_id: row.parent_session_id.map(SessionId::from),
            personality_id: row.personality_id,
            project_id,
            reasoning_level,
        },
        status,
        summary: row.summary,
        title: row.title,
        updated_at: row.updated_at,
    })
}

/// Converts one persisted transcript row into its shared typed model.
fn api_message_from_row(row: SessionMessageRow) -> Result<SessionMessage, ApiSessionError> {
    let kind = row.kind.parse::<SessionMessageKind>().map_err(|error| {
        ApiSessionError::InvalidData(format!(
            "session message at position {}: {error}",
            row.position
        ))
    })?;

    Ok(SessionMessage::new(row.position, kind, row.content))
}

/// Parses current and legacy persisted clarification-question payloads.
fn api_questions_from_json(
    raw_json: Option<&str>,
    session_id: &str,
) -> Result<Vec<QuestionItem>, ApiSessionError> {
    let Some(raw_json) = raw_json.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    if let Ok(questions) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Ok(questions);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .map(|questions| questions.into_iter().map(QuestionItem::new).collect())
        .map_err(|error| {
            ApiSessionError::InvalidData(format!(
                "session `{session_id}` has invalid questions: {error}"
            ))
        })
}

/// Converts persisted joined forge metadata into its shared typed model.
fn api_review_request_from_row(
    row: SessionReviewRequestRow,
) -> Result<ReviewRequest, ApiSessionError> {
    let forge_kind = row
        .forge_kind
        .parse()
        .map_err(ApiSessionError::InvalidData)?;
    let state = row
        .state
        .parse::<ReviewRequestState>()
        .map_err(ApiSessionError::InvalidData)?;

    Ok(ReviewRequest {
        last_refreshed_at: row.last_refreshed_at,
        summary: ag_session::ReviewRequestSummary {
            display_id: row.display_id,
            forge_kind,
            source_branch: row.source_branch,
            state,
            status_summary: row.status_summary,
            target_branch: row.target_branch,
            title: row.title,
            web_url: row.web_url,
        },
    })
}

/// Preserves stable not-found semantics while translating host app errors.
fn api_error_from_app(error: AppError) -> ApiSessionError {
    match error {
        AppError::Session(error) => api_error_from_session(error),
        other => ApiSessionError::Operation(other.to_string()),
    }
}

/// Preserves stable not-found semantics while translating session errors.
fn api_error_from_session(error: SessionError) -> ApiSessionError {
    match error {
        SessionError::NotFound => ApiSessionError::NotFound,
        other => ApiSessionError::Operation(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use ag_agent::{AgentKind, AgentModel};
    use ag_forge::ForgeKind;
    use ag_session::SessionService;

    use super::*;

    fn session_row() -> SessionRow {
        SessionRow {
            added_lines: 12,
            agent: "codex".to_string(),
            base_branch: "main".to_string(),
            created_at: 10,
            deleted_lines: 4,
            has_diff: Some(true),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 40,
            input_tokens: 50,
            is_draft: true,
            model: "gpt-5.6-sol".to_string(),
            output_tokens: 60,
            parent_session_id: Some("parent-1".to_string()),
            personality_id: Some("reviewer".to_string()),
            project_id: Some(7),
            prompt: "staged prompt".to_string(),
            published_upstream_ref: Some("origin/wt/session-1".to_string()),
            questions: Some(
                r#"[{"text":"Which target?","options":["main","develop"]}]"#.to_string(),
            ),
            reasoning_level_override: Some("xhigh".to_string()),
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
            status: "Draft".to_string(),
            summary: Some("Summary".to_string()),
            title: Some("Build feature".to_string()),
            updated_at: 20,
        }
    }

    #[test]
    fn build_api_session_returns_complete_settings_and_messages() {
        // Arrange
        let row = session_row();
        let message_rows = vec![
            SessionMessageRow {
                content: "first".to_string(),
                kind: "user_prompt".to_string(),
                position: 0,
            },
            SessionMessageRow {
                content: "done".to_string(),
                kind: "assistant_answer".to_string(),
                position: 1,
            },
        ];

        // Act
        let session = build_api_session(row, message_rows, vec!["queued message".to_string()])
            .expect("row should convert");

        // Assert
        assert_eq!(session.id, "session-1");
        assert_eq!(session.draft_prompt.as_deref(), Some("staged prompt"));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.questions[0].text, "Which target?");
        assert_eq!(session.queued_messages, ["queued message"]);
        assert_eq!(session.settings.project_id, 7);
        assert_eq!(session.settings.parent_session_id, Some("parent-1".into()));
        assert_eq!(session.settings.personality_id.as_deref(), Some("reviewer"));
        assert_eq!(
            session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
        );
        assert_eq!(session.settings.reasoning_level, ReasoningLevel::XHigh);
        assert_eq!(
            session
                .review_request
                .expect("review request should convert")
                .summary
                .forge_kind,
            ForgeKind::GitHub
        );
    }

    #[test]
    fn build_api_session_rejects_invalid_persisted_data() {
        // Arrange
        let mut missing_project = session_row();
        missing_project.project_id = None;
        let mut invalid_status = session_row();
        invalid_status.status = "Unknown".to_string();
        let invalid_message = SessionMessageRow {
            content: "content".to_string(),
            kind: "unknown".to_string(),
            position: 4,
        };
        let mut invalid_review = session_row();
        invalid_review
            .review_request
            .as_mut()
            .expect("fixture should have review metadata")
            .state = "Unknown".to_string();
        let mut invalid_questions = session_row();
        invalid_questions.questions = Some("{invalid".to_string());

        // Act
        let missing_project_error = build_api_session(missing_project, Vec::new(), Vec::new())
            .expect_err("project is required");
        let invalid_status_error = build_api_session(invalid_status, Vec::new(), Vec::new())
            .expect_err("status should be validated");
        let invalid_message_error =
            build_api_session(session_row(), vec![invalid_message], Vec::new())
                .expect_err("message kind should be validated");
        let invalid_review_error = build_api_session(invalid_review, Vec::new(), Vec::new())
            .expect_err("review should be validated");
        let invalid_questions_error = build_api_session(invalid_questions, Vec::new(), Vec::new())
            .expect_err("questions should be validated");
        let legacy_questions = api_questions_from_json(Some(r#"["Legacy question"]"#), "session-1")
            .expect("legacy questions should convert");
        let empty_questions =
            api_questions_from_json(Some(""), "session-1").expect("empty questions should convert");

        // Assert
        assert!(matches!(
            missing_project_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_status_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_message_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_review_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_questions_error,
            ApiSessionError::InvalidData(_)
        ));
        assert_eq!(legacy_questions[0].text, "Legacy question");
        assert_eq!(empty_questions, Vec::new());
    }

    #[test]
    fn api_error_translation_preserves_not_found() {
        // Arrange / Act
        let app_error = api_error_from_app(AppError::Session(SessionError::NotFound));
        let session_error = api_error_from_session(SessionError::NotFound);
        let workflow_error = api_error_from_app(AppError::Workflow("workflow failed".to_string()));

        // Assert
        assert_eq!(app_error, ApiSessionError::NotFound);
        assert_eq!(session_error, ApiSessionError::NotFound);
        assert_eq!(
            workflow_error,
            ApiSessionError::Operation("workflow failed".to_string())
        );
    }

    #[tokio::test]
    async fn app_backend_creates_and_loads_complete_sessions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let session_id = SessionService::new(&mut app)
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Regular,
                project_id,
            })
            .await
            .expect("regular session should be created");
        app.services
            .db()
            .sessions()
            .append_session_message(&session_id, SessionMessageKind::UserPrompt, "build it")
            .await
            .expect("message should persist");
        let loaded_session = SessionService::new(&mut app)
            .get_session(&session_id)
            .await
            .expect("session should load")
            .expect("session should exist");
        let missing_session = SessionService::new(&mut app)
            .get_session(&SessionId::from("missing"))
            .await
            .expect("missing lookup should succeed");
        let stacked_session_id = SessionService::new(&mut app)
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Stacked {
                    parent_session_id: session_id.clone(),
                },
                project_id,
            })
            .await
            .expect("stacked session should be created");
        let stacked_session = SessionService::new(&mut app)
            .get_session(&stacked_session_id)
            .await
            .expect("stacked session should load")
            .expect("stacked session should exist");

        // Assert
        assert_eq!(loaded_session.id, session_id);
        assert_eq!(loaded_session.status, SessionStatus::Draft);
        assert_eq!(loaded_session.messages.len(), 1);
        assert_eq!(loaded_session.messages[0].content, "build it");
        assert_eq!(
            loaded_session.settings.project_id,
            app.projects.active_project_id()
        );
        assert_eq!(missing_session, None);
        assert_eq!(stacked_session.settings.parent_session_id, Some(session_id));
    }

    #[tokio::test]
    async fn app_backend_preserves_workflow_validation_errors() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let project_error = SessionService::new(&mut app)
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Regular,
                project_id: project_id.saturating_add(1),
            })
            .await
            .expect_err("inactive project should fail");
        let session_id = SessionService::new(&mut app)
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Draft,
                project_id,
            })
            .await
            .expect("draft session should be created");

        let empty_message_error = SessionService::new(&mut app)
            .send_message(&session_id, "  ")
            .await
            .expect_err("empty message should fail");
        let missing_message_error = SessionService::new(&mut app)
            .send_message(&SessionId::from("missing"), "continue")
            .await
            .expect_err("missing session should fail");
        app.sessions
            .session_at_mut(0)
            .expect("draft session should be loaded")
            .status = SessionStatus::InProgress;
        if let Some(handles) = app.sessions.session_handles().get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = SessionStatus::InProgress;
        }
        SessionService::new(&mut app)
            .send_message(&session_id, "queued follow-up")
            .await
            .expect("running session should queue the message");
        let queued_session = SessionService::new(&mut app)
            .get_session(&session_id)
            .await
            .expect("session should load")
            .expect("session should exist");
        let merge_error = SessionService::new(&mut app)
            .merge_session(&session_id)
            .await
            .expect_err("draft session should not merge");
        let review_error = SessionService::new(&mut app)
            .create_review_request(&session_id)
            .await
            .expect_err("draft session should not publish");

        // Assert
        assert!(matches!(project_error, ApiSessionError::Operation(_)));
        assert!(matches!(empty_message_error, ApiSessionError::Operation(_)));
        assert_eq!(missing_message_error, ApiSessionError::NotFound);
        assert_eq!(queued_session.queued_messages, ["queued follow-up"]);
        assert!(matches!(merge_error, ApiSessionError::Operation(_)));
        assert!(matches!(review_error, ApiSessionError::Operation(_)));
    }

    #[tokio::test]
    async fn app_backend_reports_session_read_failures() {
        // Arrange
        let (mut session_query_app, _session_temp_dir, session_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let (mut message_query_app, _message_temp_dir, message_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let message_project_id = message_query_app.active_project_id();
        let message_session_id = SessionService::new(&mut message_query_app)
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Regular,
                project_id: message_project_id,
            })
            .await
            .expect("session should be created");
        sqlx::query("DROP TABLE session")
            .execute(&session_pool)
            .await
            .expect("session table should be dropped");
        sqlx::query("DROP TABLE session_message")
            .execute(&message_pool)
            .await
            .expect("message table should be dropped");

        // Act
        let session_query_error = SessionService::new(&mut session_query_app)
            .get_session(&SessionId::from("missing"))
            .await
            .expect_err("session query should fail");
        let message_query_error = SessionService::new(&mut message_query_app)
            .get_session(&message_session_id)
            .await
            .expect_err("message query should fail");

        // Assert
        assert!(matches!(session_query_error, ApiSessionError::Operation(_)));
        assert!(matches!(message_query_error, ApiSessionError::Operation(_)));
    }
}

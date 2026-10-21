use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, OneShotSubmission, PermissionMode, SessionStats};
use ag_protocol::AgentResponse;
use ag_session::{
    AgentModel, ReasoningLevel, SessionId, SettingName, StartPlanThenImplementRequest,
};
use ag_worker::MockRunClient;

use super::test_support::{ChainFixture, launch_request};

#[tokio::test]
async fn new_chain_titles_the_original_request_once() {
    // Arrange
    let mut fixture = ChainFixture::new_untitled().await;
    configure_fast_title_settings(&fixture).await;
    let launch = launch_request();
    let original = launch.user_request.clone();
    let mut client = MockRunClient::new();
    client.expect_submit().once().returning(move |request| {
        assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
        assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
        assert_eq!(request.model, AgentModel::Gpt53CodexSpark.as_str());
        assert_eq!(request.reasoning_level, ReasoningLevel::Low);
        assert!(request.prompt.contains(&format!(
            "Original request (JSON string): {}",
            serde_json::to_string(&original).expect("encoded original")
        )));
        assert!(request.prompt.contains(&format!(
            "Latest request (JSON string): {}",
            serde_json::to_string(&original).expect("encoded latest")
        )));
        assert!(!request.prompt.contains("/plan"));
        assert!(!request.prompt.contains("Act as the planner"));

        Ok(OneShotSubmission {
            response: AgentResponse::plain("Deliver the scoped feature"),
            stats: SessionStats::default(),
        })
    });
    fixture.app.services.set_run_client(Arc::new(client));
    let planner = fixture.register_result(&launch.planner_request().task_id, "A plan", true);

    // Act
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("launch");
    let session = fixture
        .app
        .sessions
        .session_or_err(&fixture.session_id)
        .expect("live session");

    // Assert: foreground metadata is initialized even while the Planner runs.
    assert_eq!(session.prompt, launch.user_request);
    assert!(session.title.is_some());

    // Act: matching receipts and conflicting redelivery cannot spawn another
    // title.
    fixture
        .app
        .drive_session_request(
            fixture
                .service
                .start_plan_then_implement(&fixture.session_id, launch.clone()),
        )
        .await
        .expect("receipt");
    assert!(
        fixture
            .app
            .drive_session_request(fixture.service.start_plan_then_implement(
                &fixture.session_id,
                StartPlanThenImplementRequest {
                    user_request: "Conflicting request".into(),
                    ..launch.clone()
                },
            ))
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row = fixture
                .app
                .services
                .db()
                .sessions()
                .load_session(&fixture.session_id)
                .await
                .expect("session row")
                .expect("session exists");
            if row.title.as_deref() == Some("Deliver the scoped feature") {
                assert_eq!(row.prompt, launch.user_request);
                break;
            }
            let event = fixture.app.next_app_event().await.expect("title refresh");
            fixture.app.apply_app_events(event).await;
        }
    })
    .await
    .expect("title completion guard");

    // Assert: persistence completed while the Planner still awaits release.
    assert!(!planner.release.is_cancelled());
    planner.release.cancel();
    fixture.settle(&planner).await;
}

#[tokio::test]
async fn new_chain_preserves_authoritative_metadata() {
    for original in ["", "An established session goal"] {
        // Arrange
        let mut fixture = ChainFixture::new_untitled().await;
        fixture
            .app
            .services
            .db()
            .sessions()
            .update_session_title(&fixture.session_id, "Authoritative title")
            .await
            .expect("authoritative title");
        fixture
            .app
            .services
            .db()
            .sessions()
            .update_session_prompt(&fixture.session_id, original)
            .await
            .expect("existing prompt");
        let index = fixture
            .app
            .sessions
            .session_index_or_err(&fixture.session_id)
            .expect("index");
        let session = fixture.app.sessions.session_at_mut(index).expect("session");
        session.title = Some("Authoritative title".into());
        session.prompt = original.into();
        let mut client = MockRunClient::new();
        client.expect_submit().never();
        fixture.app.services.set_run_client(Arc::new(client));
        let launch = launch_request();
        let planner = fixture.register_result(&launch.planner_request().task_id, "A plan", false);
        let expected_prompt = if original.is_empty() {
            launch.user_request.clone()
        } else {
            original.into()
        };

        // Act
        fixture
            .app
            .drive_session_request(
                fixture
                    .service
                    .start_plan_then_implement(&fixture.session_id, launch),
            )
            .await
            .expect("launch");
        fixture.settle(&planner).await;

        // Assert
        let row = fixture
            .app
            .services
            .db()
            .sessions()
            .load_session(&fixture.session_id)
            .await
            .expect("session row")
            .expect("session exists");
        assert_eq!(row.title.as_deref(), Some("Authoritative title"));
        assert_eq!(row.prompt, expected_prompt);
    }
}

#[tokio::test]
async fn chain_title_metadata_failures_do_not_start_generation() {
    // Arrange
    let mut fixture = ChainFixture::new_untitled().await;
    sqlx::query(
        "CREATE TRIGGER reject_metadata BEFORE UPDATE OF prompt, title ON session BEGIN SELECT \
         RAISE(ABORT, 'metadata unavailable'); END",
    )
    .execute(&fixture.pool)
    .await
    .expect("reject metadata writes");
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    fixture.app.services.set_run_client(Arc::new(client));

    // Act
    fixture
        .app
        .sessions
        .initialize_task_chain_title(
            &fixture.app.services,
            &SessionId::from("missing"),
            "Request",
        )
        .await;
    fixture
        .app
        .sessions
        .initialize_task_chain_title(&fixture.app.services, &fixture.session_id, "Request")
        .await;

    // Assert
    let row = fixture
        .app
        .services
        .db()
        .sessions()
        .load_session(&fixture.session_id)
        .await
        .expect("session row")
        .expect("session exists");
    assert_eq!(row.prompt, "");
    assert_eq!(row.title, None);
}

/// Makes the utility model differ from the role model for selection assertions.
async fn configure_fast_title_settings(fixture: &ChainFixture) {
    fixture
        .app
        .services
        .db()
        .settings()
        .upsert_project_settings(
            fixture.app.active_project_id(),
            vec![
                (SettingName::DefaultFastAgent, "codex".into()),
                (
                    SettingName::DefaultFastModel,
                    AgentModel::Gpt53CodexSpark.as_str().into(),
                ),
                (
                    SettingName::DefaultFastReasoningLevel,
                    ReasoningLevel::Low.as_str().into(),
                ),
            ],
        )
        .await
        .expect("Fast title settings");
}

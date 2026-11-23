//! API key popup opening, saving, and closing unit tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_session::AgentAvailabilityProbe;

use crate::app::App;
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, HarnessModelOption, HarnessModelStatus,
};
use crate::domain::harness::HarnessModelCatalog;
use crate::domain::input::InputCommand;
use crate::domain::session::{SessionId, Status};
use crate::presentation::api_key::ApiKeyPhase;
use crate::presentation::app_mode::AppMode;
use crate::test_support;

/// Probe that records saved keys and answers with `result`. Its listing
/// never finishes, so only the ready publish after a save changes the rows.
struct RecordingKeyProbe {
    result: Result<(), String>,
    saved: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl AgentAvailabilityProbe for RecordingKeyProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        Vec::new()
    }

    async fn native_harness_models(&self) -> Vec<HarnessModelOption> {
        std::future::pending().await
    }

    async fn save_harness_api_key(&self, provider: &str, api_key: &str) -> Result<(), String> {
        self.saved
            .lock()
            .expect("saved lock")
            .push((provider.to_string(), api_key.to_string()));

        self.result.clone()
    }
}

/// Returns the Kimi Harness selection used by these tests.
fn kimi_selection() -> AgentSelection {
    AgentSelection::new(AgentKind::Harness, AgentModel::KimiK3)
}

/// Builds an app with one session whose listed Kimi models need a key, and
/// a probe answering saves with `result`; returns the recorded saves.
async fn key_app(
    result: Result<(), String>,
) -> (
    App,
    tempfile::TempDir,
    SessionId,
    Arc<Mutex<Vec<(String, String)>>>,
) {
    let (mut app, base_dir) = test_support::new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    let saved = Arc::new(Mutex::new(Vec::new()));
    app.services.set_harness_models(
        HarnessModelCatalog::new(
            vec![
                test_support::api_key_required_harness_model(AgentModel::KimiK3),
                test_support::api_key_required_harness_model(AgentModel::KimiK26),
                test_support::sign_in_required_harness_model(AgentModel::ChatGptGpt6Sol),
            ],
            None,
        ),
        Arc::new(RecordingKeyProbe {
            result,
            saved: Arc::clone(&saved),
        }),
    );

    (app, base_dir, session_id, saved)
}

/// Opens the popup for Kimi above a composer of `session_id` and pastes
/// `key`.
fn open_with_key(app: &mut App, session_id: &SessionId, key: &str) {
    app.open_api_key_input(
        kimi_selection(),
        test_support::prompt_mode_snapshot(session_id.as_str(), "draft"),
    );
    if let AppMode::ApiKeyInput { dialog } = &mut app.mode {
        dialog
            .input
            .apply(InputCommand::InsertText(key.to_string()));
    }
}

/// Returns the visible popup phase.
fn phase(app: &App) -> ApiKeyPhase {
    match &app.mode {
        AppMode::ApiKeyInput { dialog } => dialog.phase.clone(),
        _ => unreachable!("API key popup should be open"),
    }
}

/// Reduces app events until every listed Kimi model is ready.
async fn reduce_until_kimi_ready(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.services.harness_model_options().iter().any(|option| {
            matches!(option.model, AgentModel::KimiK3 | AgentModel::KimiK26) && !option.is_ready()
        }) {
            let event = app.next_app_event().await.expect("app event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("Kimi models become ready");
}

#[tokio::test]
async fn opening_names_the_provider_whose_key_the_model_needs() {
    // Arrange
    let (mut app, _base_dir, session_id, _saved) = key_app(Ok(())).await;

    // Act
    open_with_key(&mut app, &session_id, "");

    // Assert
    let AppMode::ApiKeyInput { dialog } = &app.mode else {
        unreachable!("API key popup should be open");
    };
    assert_eq!(dialog.provider, "kimi");
    assert_eq!(dialog.provider_name, "Kimi");
    assert_eq!(dialog.selection, kimi_selection());
    assert_eq!(dialog.session_id(), &session_id);
}

#[tokio::test]
async fn opening_for_a_model_that_needs_no_key_restores_the_composer() {
    // Arrange
    let (mut app, _base_dir, session_id, _saved) = key_app(Ok(())).await;

    // Act
    app.open_api_key_input(
        AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol),
        test_support::prompt_mode_snapshot(session_id.as_str(), "draft"),
    );

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft"
    ));
}

#[tokio::test]
async fn saving_stores_the_key_switches_the_model_and_marks_the_provider_ready() {
    // Arrange
    let (mut app, _base_dir, session_id, saved) = key_app(Ok(())).await;
    open_with_key(&mut app, &session_id, "sk-kimi");

    // Act
    app.save_api_key().await;
    reduce_until_kimi_ready(&mut app).await;

    // Assert
    assert_eq!(
        *saved.lock().expect("saved lock"),
        [("kimi".to_string(), "sk-kimi".to_string())]
    );
    assert_eq!(
        phase(&app),
        ApiKeyPhase::Saved {
            message: "Saved your Kimi API key. This session now uses `kimi-k3`.".to_string(),
        }
    );
    let AppMode::ApiKeyInput { dialog } = &app.mode else {
        unreachable!("API key popup should be open");
    };
    assert_eq!(dialog.input.text(), "");
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .map(|session| session.agent),
        Some(kimi_selection())
    );
    let statuses = app
        .services
        .harness_model_options()
        .into_iter()
        .map(|option| (option.model, option.status))
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        [
            (AgentModel::KimiK3, HarnessModelStatus::Ready),
            (AgentModel::KimiK26, HarnessModelStatus::Ready),
            (
                AgentModel::ChatGptGpt6Sol,
                HarnessModelStatus::SignInRequired("ag-harness login chatgpt"),
            ),
        ]
    );
}

#[tokio::test]
async fn saving_defers_the_model_switch_while_the_session_runs() {
    // Arrange
    let (mut app, _base_dir, session_id, _saved) = key_app(Ok(())).await;
    *app.sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("session handles")
        .status
        .lock()
        .expect("status lock") = Status::InProgress;
    app.sessions.sync_session_from_handle(&session_id);
    open_with_key(&mut app, &session_id, "sk-kimi");

    // Act
    app.save_api_key().await;

    // Assert
    assert_eq!(
        phase(&app),
        ApiKeyPhase::Saved {
            message: "Saved your Kimi API key. This session switches to `kimi-k3` after its \
                      current turn and queued messages finish."
                .to_string(),
        }
    );
}

#[tokio::test]
async fn saving_reports_a_model_switch_that_failed() {
    // Arrange
    let (mut app, _base_dir, _session_id, _saved) = key_app(Ok(())).await;
    open_with_key(&mut app, &SessionId::from("missing-session"), "sk-kimi");

    // Act
    app.save_api_key().await;

    // Assert
    assert_eq!(
        phase(&app),
        ApiKeyPhase::Saved {
            message: "Saved your Kimi API key, but switching this session to `kimi-k3` failed. \
                      Pick it again with /model."
                .to_string(),
        }
    );
}

#[tokio::test]
async fn a_failed_save_keeps_the_key_for_another_try() {
    // Arrange
    let (mut app, _base_dir, session_id, _saved) =
        key_app(Err("Cannot save the key.".to_string())).await;
    open_with_key(&mut app, &session_id, "sk-kimi");

    // Act
    app.save_api_key().await;

    // Assert
    assert_eq!(
        phase(&app),
        ApiKeyPhase::Editing {
            error: Some("Cannot save the key.".to_string()),
        }
    );
    let AppMode::ApiKeyInput { dialog } = &app.mode else {
        unreachable!("API key popup should be open");
    };
    assert_eq!(dialog.input.text(), "sk-kimi");
}

#[tokio::test]
async fn saving_outside_the_editing_step_does_nothing() {
    // Arrange
    let (mut app, _base_dir, session_id, saved) = key_app(Ok(())).await;
    app.mode = AppMode::List;
    app.save_api_key().await;
    open_with_key(&mut app, &session_id, "sk-kimi");
    let saved_phase = ApiKeyPhase::Saved {
        message: "Saved.".to_string(),
    };
    if let AppMode::ApiKeyInput { dialog } = &mut app.mode {
        dialog.phase = saved_phase.clone();
    }

    // Act
    app.save_api_key().await;

    // Assert
    assert_eq!(phase(&app), saved_phase);
    assert!(saved.lock().expect("saved lock").is_empty());
}

#[tokio::test]
async fn a_save_result_after_the_popup_closed_changes_nothing() {
    // Arrange
    let mut app = test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::List;

    // Act
    app.finish_api_key_input(ApiKeyPhase::Saved {
        message: "Saved.".to_string(),
    });

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn the_popup_animates_nothing_over_an_idle_session() {
    // Arrange
    let mut app = test_support::new_test_app_without_retained_base_dir().await;
    app.mode = test_support::api_key_input_mode("idle-session");

    // Act
    let has_tick_driven_ui = app.has_visible_tick_driven_ui();

    // Assert
    assert!(!has_tick_driven_ui);
}

#[tokio::test]
async fn closing_restores_the_composer_and_leaves_other_modes() {
    // Arrange
    let (mut app, _base_dir, session_id, _saved) = key_app(Ok(())).await;
    open_with_key(&mut app, &session_id, "sk-kimi");

    // Act
    app.close_api_key_input();
    let restored = matches!(
        &app.mode,
        AppMode::Prompt { input, .. } if input.text() == "draft"
    );
    app.mode = AppMode::List;
    app.close_api_key_input();

    // Assert
    assert!(restored);
    assert!(matches!(app.mode, AppMode::List));
}

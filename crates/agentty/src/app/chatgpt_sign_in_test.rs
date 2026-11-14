use std::sync::Arc;
use std::time::Duration;

use ag_session::{AgentAvailabilityProbe, ChatGptSignInOutcome, ChatGptSignInUrlHandler};
use mockall::predicate::eq;

use super::ChatGptSignInUpdate;
use crate::app::{App, AppEvent};
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, HarnessModelOption, HarnessModelStatus,
};
use crate::domain::harness::HarnessModelCatalog;
use crate::domain::session::{SessionId, Status};
use crate::infra::browser::MockBrowserClient;
use crate::presentation::app_mode::AppMode;
use crate::presentation::chatgpt_sign_in::ChatGptSignInPhase;
use crate::test_support;

const SIGN_IN_URL: &str = "https://auth.example.test/oauth/authorize?state=abc";

/// How the scripted probe answers a sign-in.
#[derive(Clone)]
enum ScriptedSignIn {
    /// Reports the URL, then finishes with the result.
    Finish(Result<ChatGptSignInOutcome, String>),
    /// Reports the URL, then never finishes.
    Hang,
}

/// Probe whose sign-in reports [`SIGN_IN_URL`] and follows its script, and
/// whose listing reports the `ChatGPT` model ready.
struct ScriptedSignInProbe {
    script: ScriptedSignIn,
}

#[async_trait::async_trait]
impl AgentAvailabilityProbe for ScriptedSignInProbe {
    fn available_agent_kinds(&self) -> Vec<AgentKind> {
        Vec::new()
    }

    async fn native_harness_models(&self) -> Vec<HarnessModelOption> {
        vec![HarnessModelOption {
            status: HarnessModelStatus::Ready,
            ..test_support::sign_in_required_harness_model(AgentModel::ChatGptGpt6Sol)
        }]
    }

    async fn sign_in_chatgpt(
        &self,
        on_url: ChatGptSignInUrlHandler,
    ) -> Result<ChatGptSignInOutcome, String> {
        on_url(SIGN_IN_URL.to_string());
        match self.script.clone() {
            ScriptedSignIn::Finish(result) => result,
            ScriptedSignIn::Hang => std::future::pending().await,
        }
    }
}

/// Returns the `ChatGPT`-plan Harness selection used by these tests.
fn chatgpt_selection() -> AgentSelection {
    AgentSelection::new(AgentKind::Harness, AgentModel::ChatGptGpt6Sol)
}

/// Builds an app with one session, a browser expecting `opens` sign-in
/// URLs, and harness models listed by a probe following `script`.
async fn sign_in_app(script: ScriptedSignIn, opens: usize) -> (App, tempfile::TempDir, SessionId) {
    let mut browser = MockBrowserClient::new();
    browser
        .expect_open()
        .with(eq(SIGN_IN_URL))
        .times(opens)
        .return_const(());
    let clients = test_support::test_app_clients().with_browser_client(Arc::new(browser));
    let (mut app, base_dir) = test_support::new_git_test_app_with_clients(clients).await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.services.set_harness_models(
        HarnessModelCatalog::new(
            true,
            vec![
                test_support::ready_harness_model(AgentModel::KimiK3),
                test_support::sign_in_required_harness_model(AgentModel::ChatGptGpt6Sol),
            ],
            None,
        ),
        Arc::new(ScriptedSignInProbe { script }),
    );

    (app, base_dir, session_id)
}

/// Opens the popup for the `ChatGPT` model above a composer with `draft`.
fn open_popup(app: &mut App, session_id: &SessionId, draft: &str) {
    app.open_chatgpt_sign_in(
        chatgpt_selection(),
        test_support::prompt_mode_snapshot(session_id.as_str(), draft),
    );
}

/// Returns the visible sign-in phase.
fn phase(app: &App) -> ChatGptSignInPhase {
    match &app.mode {
        AppMode::ChatGptSignIn { dialog } => dialog.phase.clone(),
        _ => unreachable!("sign-in popup should be open"),
    }
}

/// Reduces app events until the popup leaves the waiting phase.
async fn reduce_until_finished(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(phase(app), ChatGptSignInPhase::Waiting { .. }) {
            let event = app.next_app_event().await.expect("sign-in event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("sign-in finishes");
}

/// Waits for a sign-in event independently of unrelated startup events.
async fn next_sign_in_event(app: &mut App) -> AppEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = app.next_app_event().await.expect("open event channel");
            if matches!(event, AppEvent::ChatGptSignInUpdated { .. }) {
                return event;
            }
        }
    })
    .await
    .expect("sign-in event")
}

#[tokio::test]
async fn successful_sign_in_applies_the_model_and_marks_it_ready() {
    // Arrange
    let outcome = ChatGptSignInOutcome {
        email: Some("ada@example.com".to_string()),
    };
    let (mut app, _base_dir, session_id) =
        sign_in_app(ScriptedSignIn::Finish(Ok(outcome)), 1).await;
    open_popup(&mut app, &session_id, "draft");

    // Act
    app.start_chatgpt_sign_in();
    reduce_until_finished(&mut app).await;

    // Assert
    assert_eq!(
        phase(&app),
        ChatGptSignInPhase::Finished {
            message: "Signed in as ada@example.com. `chatgpt/gpt-6-sol` now uses your ChatGPT \
                      plan."
                .to_string(),
        }
    );
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .map(|session| session.agent),
        Some(chatgpt_selection())
    );
    assert!(
        app.services
            .harness_model_options()
            .iter()
            .all(HarnessModelOption::is_ready)
    );
    assert_eq!(app.chatgpt_sign_in_required(0), None);
}

#[tokio::test]
async fn successful_sign_in_reports_a_model_switch_that_failed() {
    // Arrange
    let outcome = ChatGptSignInOutcome {
        email: Some("ada@example.com".to_string()),
    };
    let (mut app, _base_dir, _session_id) =
        sign_in_app(ScriptedSignIn::Finish(Ok(outcome)), 1).await;
    open_popup(&mut app, &SessionId::from("missing-session"), "draft");

    // Act
    app.start_chatgpt_sign_in();
    reduce_until_finished(&mut app).await;

    // Assert
    assert_eq!(
        phase(&app),
        ChatGptSignInPhase::Finished {
            message: "Signed in as ada@example.com, but switching this session to \
                      `chatgpt/gpt-6-sol` failed. Pick it again with /model."
                .to_string(),
        }
    );
}

#[tokio::test]
async fn successful_sign_in_defers_the_model_switch_while_the_session_runs() {
    // Arrange
    let outcome = ChatGptSignInOutcome {
        email: Some("ada@example.com".to_string()),
    };
    let (mut app, _base_dir, session_id) =
        sign_in_app(ScriptedSignIn::Finish(Ok(outcome)), 1).await;
    let initial_agent = app
        .sessions
        .session_for_id(&session_id)
        .map(|session| session.agent);
    *app.sessions
        .session_handles()
        .get(session_id.as_str())
        .expect("session handles")
        .status
        .lock()
        .expect("status lock") = Status::InProgress;
    app.sessions.sync_session_from_handle(&session_id);
    open_popup(&mut app, &session_id, "draft");

    // Act
    app.start_chatgpt_sign_in();
    reduce_until_finished(&mut app).await;

    // Assert
    assert_eq!(
        phase(&app),
        ChatGptSignInPhase::Finished {
            message: "Signed in as ada@example.com. This session switches to `chatgpt/gpt-6-sol` \
                      on your ChatGPT plan after its current turn and queued messages finish."
                .to_string(),
        }
    );
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .map(|session| session.agent),
        initial_agent
    );
    assert_eq!(
        app.deferred_session_settings
            .get(&session_id)
            .and_then(|settings| settings.agent),
        Some(chatgpt_selection())
    );
}

#[tokio::test]
async fn sign_in_without_an_email_names_the_account_generically() {
    // Arrange
    let outcome = ChatGptSignInOutcome { email: None };
    let (mut app, _base_dir, session_id) =
        sign_in_app(ScriptedSignIn::Finish(Ok(outcome)), 1).await;
    open_popup(&mut app, &session_id, "");

    // Act
    app.start_chatgpt_sign_in();
    reduce_until_finished(&mut app).await;

    // Assert
    assert!(matches!(
        phase(&app),
        ChatGptSignInPhase::Finished { message }
            if message.starts_with("Signed in as your ChatGPT account.")
    ));
}

#[tokio::test]
async fn failed_sign_in_reports_the_error_and_keeps_the_session_model() {
    // Arrange
    let (mut app, _base_dir, session_id) =
        sign_in_app(ScriptedSignIn::Finish(Err("access denied".to_string())), 1).await;
    let original_agent = app
        .sessions
        .session_for_id(&session_id)
        .map(|session| session.agent);
    open_popup(&mut app, &session_id, "draft");

    // Act
    app.start_chatgpt_sign_in();
    reduce_until_finished(&mut app).await;

    // Assert
    assert_eq!(
        phase(&app),
        ChatGptSignInPhase::Finished {
            message: "Sign-in failed: access denied".to_string(),
        }
    );
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .map(|session| session.agent),
        original_agent
    );
    assert!(
        !app.services
            .harness_model_options()
            .iter()
            .any(|option| option.model == AgentModel::ChatGptGpt6Sol && option.is_ready())
    );
}

#[tokio::test]
async fn waiting_sign_in_shows_its_url_and_ignores_stale_updates() {
    // Arrange
    let (mut app, _base_dir, session_id) = sign_in_app(ScriptedSignIn::Hang, 1).await;
    open_popup(&mut app, &session_id, "draft");
    app.start_chatgpt_sign_in();
    let event = next_sign_in_event(&mut app).await;

    // Act
    app.apply_app_events(event).await;
    app.apply_chatgpt_sign_in_update(ChatGptSignInUpdate::Url {
        request_id: 99,
        url: "https://stale.example.test".to_string(),
    })
    .await;
    app.apply_chatgpt_sign_in_update(ChatGptSignInUpdate::Completed {
        request_id: 99,
        result: Err("stale".to_string()),
    })
    .await;

    // Assert
    assert_eq!(
        phase(&app),
        ChatGptSignInPhase::Waiting {
            request_id: 1,
            url: Some(SIGN_IN_URL.to_string()),
        }
    );
}

#[tokio::test]
async fn closing_a_waiting_sign_in_restores_the_composer_and_drops_late_results() {
    // Arrange
    let (mut app, _base_dir, session_id) = sign_in_app(ScriptedSignIn::Hang, 1).await;
    open_popup(&mut app, &session_id, "keep this draft");
    app.services
        .event_sender()
        .send(AppEvent::SessionProgressUpdated {
            progress_message: Some("Unrelated startup progress".into()),
            session_id: session_id.clone(),
        })
        .expect("queue unrelated event before sign-in");
    app.start_chatgpt_sign_in();
    app.start_chatgpt_sign_in();
    let url_event = next_sign_in_event(&mut app).await;
    assert!(matches!(
        url_event,
        AppEvent::ChatGptSignInUpdated {
            update: ChatGptSignInUpdate::Url { request_id: 1, .. }
        }
    ));

    // Act
    app.close_chatgpt_sign_in();
    app.apply_app_events(AppEvent::ChatGptSignInUpdated {
        update: ChatGptSignInUpdate::Completed {
            request_id: 1,
            result: Err("late".to_string()),
        },
    })
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Prompt { input, session_id: prompt_session, .. }
            if input.text() == "keep this draft" && *prompt_session == session_id
    ));
}

#[tokio::test]
async fn sign_in_actions_outside_the_popup_leave_the_mode_unchanged() {
    // Arrange
    let (mut app, _base_dir, _session_id) = sign_in_app(ScriptedSignIn::Hang, 0).await;

    // Act
    app.start_chatgpt_sign_in();
    app.close_chatgpt_sign_in();
    app.apply_chatgpt_sign_in_update(ChatGptSignInUpdate::Url {
        request_id: 1,
        url: SIGN_IN_URL.to_string(),
    })
    .await;
    app.finish_chatgpt_sign_in("Signed in".to_string());

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn sign_in_is_required_only_for_harness_sessions_on_sign_in_models() {
    // Arrange
    let (mut app, _base_dir, session_id) = sign_in_app(ScriptedSignIn::Hang, 0).await;
    let codex_required = app.chatgpt_sign_in_required(0);
    app.set_session_model(&session_id, chatgpt_selection())
        .await
        .expect("model update");

    // Act
    let chatgpt_required = app.chatgpt_sign_in_required(0);
    let missing_session_required = app.chatgpt_sign_in_required(5);

    // Assert
    assert_eq!(codex_required, None);
    assert_eq!(chatgpt_required, Some(chatgpt_selection()));
    assert_eq!(missing_session_required, None);
}

#[tokio::test]
async fn only_a_waiting_sign_in_drives_redraw_ticks() {
    // Arrange
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.mode = test_support::chatgpt_sign_in_mode("missing-session");
    let confirming_ticks = app.has_visible_tick_driven_ui();
    if let AppMode::ChatGptSignIn { dialog } = &mut app.mode {
        dialog.phase = ChatGptSignInPhase::Waiting {
            request_id: 1,
            url: None,
        };
    }

    // Act
    let waiting_ticks = app.has_visible_tick_driven_ui();

    // Assert
    assert!(!confirming_ticks);
    assert!(waiting_ticks);
}

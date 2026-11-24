//! Platform sign-in, polling, account check, sign-out, and link tests
//! against a scripted platform client.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_remote::{RemoteError, RemoteServerStatus};
use mockall::predicate::{always, eq};
use tokio::sync::watch;

use super::{
    Access, Platform, PlatformUpdate, SIGNED_OUT_BY_PLATFORM, check_account,
    platform_instance_name, wait_for_sign_in,
};
use crate::app::remote_control::RemoteControl;
use crate::app::{App, AppEvent, AppRuntimeEvent, Tab};
use crate::domain::setting::SettingName;
use crate::infra::browser::MockBrowserClient;
use crate::infra::platform::{
    AccessToken, DeviceAuthorization, DeviceCode, MockPlatformClient, PlatformAccount,
    PlatformClient, PlatformError, RegisteredInstance, RemoteControlService, SignIn,
};
use crate::infra::remote_control::{MockRemoteControlLauncher, RemoteControlServer};
use crate::infra::terminal_clipboard::MockTerminalClipboard;
use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformSignInView, PlatformState,
};
use crate::test_support;

/// Platform origin the tests sign in to.
const PLATFORM_URL: &str = "https://platform.example";
/// Name this Agentty asks to be shown as.
const INSTANCE_NAME: &str = "Agentty on box";
/// Token the scripted platform issues.
const TOKEN: &str = "agp_test-token";

/// Builds a sign-in that the platform says to poll every `interval`
/// seconds for `expires_in` seconds.
fn authorization(interval: u64, expires_in: u64) -> DeviceAuthorization {
    DeviceAuthorization {
        device_code: DeviceCode::new("device-code".to_string()),
        expires_in: Duration::from_secs(expires_in),
        interval: Duration::from_secs(interval),
        user_code: "BCDF-GHJK".to_string(),
        verification_uri: "https://platform.example/device".to_string(),
        verification_uri_complete: Some(
            "https://platform.example/device?user_code=BCDF-GHJK".to_string(),
        ),
    }
}

/// Returns the token the scripted platform issues.
fn token() -> AccessToken {
    AccessToken::new(TOKEN.to_string())
}

/// Returns an approved sign-in.
fn signed_in() -> SignIn {
    SignIn {
        access_token: token(),
        login: "octocat".to_string(),
    }
}

/// Returns the relay the scripted platform offers.
fn service() -> RemoteControlService {
    RemoteControlService {
        pairing_url: "https://platform.example/sessions/".to_string(),
        relay_url: "wss://relay.platform.example".to_string(),
    }
}

/// Returns the account, offering the relay when `has_relay` is set.
fn account(has_relay: bool) -> PlatformAccount {
    PlatformAccount {
        login: "octocat".to_string(),
        remote_control: has_relay.then(service),
    }
}

/// Builds a client whose token polls answer `answers` in order.
fn polling_client(answers: Vec<Result<SignIn, PlatformError>>) -> MockPlatformClient {
    let answers = Mutex::new(VecDeque::from(answers));
    let mut client = MockPlatformClient::new();
    client.expect_poll_device_token().returning(move |_| {
        answers
            .lock()
            .expect("answers lock")
            .pop_front()
            .expect("an answer for every poll")
    });

    client
}

/// Builds a client whose account offers the relay `account_checks` times,
/// registering this Agentty under its own name each time.
fn registering_client(account_checks: usize) -> MockPlatformClient {
    let mut client = MockPlatformClient::new();
    client
        .expect_account()
        .times(account_checks)
        .returning(|_| Ok(account(true)));
    client
        .expect_register_instance()
        .times(account_checks)
        .returning(|_, instance_id, name| {
            Ok(RegisteredInstance {
                instance_id: instance_id.to_string(),
                name: name.to_string(),
            })
        });

    client
}

/// Builds a launcher that starts `starts` listeners on a free port, each
/// linked to the platform's relay.
fn platform_relay_launcher(starts: usize) -> MockRemoteControlLauncher {
    let mut launcher = MockRemoteControlLauncher::new();
    launcher
        .expect_start()
        .withf(|port, _, relay, _| {
            *port == 0
                && relay
                    .as_ref()
                    .is_some_and(|relay| relay.url == service().relay_url)
        })
        .times(starts)
        .returning(
            |_, _, _, _| Ok(Box::new(StubServer::default()) as Box<dyn RemoteControlServer>),
        );

    launcher
}

/// Builds an app whose remote control starts listeners through `launcher`
/// and whose link actions go through `browser` and `clipboard`, with the
/// platform not yet enabled.
async fn app_with_fakes(
    launcher: MockRemoteControlLauncher,
    browser: MockBrowserClient,
    clipboard: MockTerminalClipboard,
) -> (App, tempfile::TempDir) {
    let (mut app, base_dir) = test_support::new_test_app().await;
    let browser = Arc::new(browser);
    let clipboard = Arc::new(clipboard);
    app.platform = Platform::new(browser.clone(), clipboard.clone());
    app.remote_control =
        RemoteControl::new(Arc::new(launcher), browser, clipboard, app.services.db());

    (app, base_dir)
}

/// Enables the Platform tab of `app` over `client`, as a launch does.
async fn enable(app: &mut App, client: impl PlatformClient + 'static) {
    app.enable_platform(
        Arc::new(client),
        PLATFORM_URL.to_string(),
        INSTANCE_NAME.to_string(),
    )
    .await;
}

/// Builds an app offering the Platform tab over `client`, with remote
/// control behind `launcher` and link actions through `browser` and
/// `clipboard`.
async fn platform_app_with(
    client: impl PlatformClient + 'static,
    launcher: MockRemoteControlLauncher,
    browser: MockBrowserClient,
    clipboard: MockTerminalClipboard,
) -> (App, tempfile::TempDir) {
    let (mut app, base_dir) = app_with_fakes(launcher, browser, clipboard).await;
    enable(&mut app, client).await;

    (app, base_dir)
}

/// Builds an app offering the Platform tab over `client`, with remote
/// control behind `launcher`.
async fn platform_app_with_launcher(
    client: impl PlatformClient + 'static,
    launcher: MockRemoteControlLauncher,
) -> (App, tempfile::TempDir) {
    platform_app_with(
        client,
        launcher,
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
    .await
}

/// Builds an app offering the Platform tab over `client`, with no remote
/// control listener or link action expected.
async fn platform_app(client: impl PlatformClient + 'static) -> (App, tempfile::TempDir) {
    platform_app_with(
        client,
        MockRemoteControlLauncher::new(),
        MockBrowserClient::new(),
        MockTerminalClipboard::new(),
    )
    .await
}

/// Stores a sign-in issued by `platform_url`, saved before keep on existed.
async fn store_sign_in(app: &App, platform_url: &str) {
    store_sign_in_value(
        app,
        serde_json::json!({ "access_token": TOKEN, "platform_url": platform_url }),
    )
    .await;
}

/// Stores a sign-in issued by `platform_url` with `keep_on`.
async fn store_sign_in_keeping_on(app: &App, platform_url: &str, keep_on: bool) {
    store_sign_in_value(
        app,
        serde_json::json!({
            "access_token": TOKEN,
            "keep_on": keep_on,
            "platform_url": platform_url,
        }),
    )
    .await;
}

/// Stores `stored` as the saved sign-in.
async fn store_sign_in_value(app: &App, stored: serde_json::Value) {
    app.services
        .db()
        .settings()
        .upsert_setting(SettingName::PlatformSignIn, &stored.to_string())
        .await
        .expect("sign-in should save");
}

/// Reads the stored sign-in.
async fn stored_sign_in(app: &App) -> Option<String> {
    app.services
        .db()
        .settings()
        .get_setting(SettingName::PlatformSignIn)
        .await
        .expect("setting should load")
}

/// Reads keep on from the stored sign-in, or `None` without one.
async fn stored_keep_on(app: &App) -> Option<bool> {
    let stored: serde_json::Value = serde_json::from_str(&stored_sign_in(app).await?).ok()?;

    stored["keep_on"].as_bool()
}

/// Returns whether the remote-control listener runs.
fn is_remote_control_running(app: &App) -> bool {
    app.remote_control.view().listener.is_some()
}

/// Returns whether the signed-in account keeps remote control on.
fn is_kept_on(app: &App) -> bool {
    account_view(app).is_some_and(|account| account.keep_on)
}

/// Signs `app` in with the test token and `access`, with keep on off.
fn sign_in_directly(app: &mut App, login: Option<&str>, access: PlatformAccess) {
    app.platform.token = Some(token());
    app.platform.view.state = PlatformState::SignedIn(PlatformAccountView {
        access,
        ..PlatformAccountView::checking(login.map(str::to_string))
    });
}

/// Signs `app` in as registered with the platform's relay, keep on off.
async fn sign_in_registered(app: &mut App) {
    sign_in_directly(app, Some("octocat"), PlatformAccess::Checking);
    app.apply_platform_update(PlatformUpdate::Refreshed {
        request_id: 0,
        login: None,
        result: Ok(Access::Registered {
            name: INSTANCE_NAME.to_string(),
            service: service(),
        }),
    })
    .await;
}

/// Applies background app events until `is_done` holds, failing after five
/// seconds.
async fn apply_events_until(app: &mut App, is_done: fn(&App) -> bool) {
    let wait = async {
        while !is_done(app) {
            match app.next_runtime_event().await {
                AppRuntimeEvent::App(event) => app.apply_app_events(*event).await,
                AppRuntimeEvent::Session(command) => {
                    app.apply_session_runtime_command(*command).await;
                }
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("the awaited state should arrive");
}

/// Returns the signed-in account view.
fn account_view(app: &App) -> Option<&PlatformAccountView> {
    app.platform.view().account()
}

/// Returns whether the signed-in account is on the waitlist.
fn is_waitlisted(app: &App) -> bool {
    account_view(app).is_some_and(|account| account.access == PlatformAccess::Waitlisted)
}

/// Returns the shown sign-out or sign-in outcome while signed out.
fn signed_out_notice(app: &App) -> Option<&str> {
    match &app.platform.view().state {
        PlatformState::SignedOut { notice } => notice.as_deref(),
        _ => None,
    }
}

/// Returns the notice under a waiting sign-in.
fn sign_in_notice(app: &App) -> Option<String> {
    match &app.platform.view().state {
        PlatformState::SigningIn(sign_in) => sign_in.notice.clone(),
        _ => None,
    }
}

#[test]
fn instance_name_names_the_computer_without_control_characters_and_within_the_limit() {
    // Arrange
    let long_host = "x".repeat(80);
    let cases = [
        ("box", "Agentty on box".to_string()),
        (" box\u{1b}[2J \n", "Agentty on box[2J".to_string()),
        ("", "Agentty".to_string()),
        ("\u{7}", "Agentty".to_string()),
        (long_host.as_str(), format!("Agentty on {}", "x".repeat(53))),
    ];

    for (host_name, expected) in cases {
        // Act
        let name = platform_instance_name(host_name);

        // Assert
        assert_eq!(name, expected);
    }
}

#[tokio::test(start_paused = true)]
async fn polling_waits_the_interval_and_adds_five_seconds_after_slow_down() {
    // Arrange
    let client = polling_client(vec![
        Err(PlatformError::AuthorizationPending),
        Err(PlatformError::SlowDown),
        Err(PlatformError::Unreachable),
        Ok(signed_in()),
    ]);
    let started = tokio::time::Instant::now();

    // Act
    let result = wait_for_sign_in(&client, &authorization(5, 600)).await;

    // Assert: 5 s, 5 s, then 10 s twice after `slow_down`.
    assert_eq!(result, Ok(signed_in()));
    assert_eq!(started.elapsed(), Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn polling_honours_retry_after_and_the_code_lifetime() {
    // Arrange
    let answers = || {
        vec![
            Err(PlatformError::RateLimited {
                retry_after: Some(Duration::from_secs(600)),
            }),
            Err(PlatformError::RateLimited { retry_after: None }),
            Ok(signed_in()),
        ]
    };
    let started = tokio::time::Instant::now();

    // Act
    let result = wait_for_sign_in(&polling_client(answers()), &authorization(5, 3600)).await;
    let elapsed = started.elapsed();
    let expired = wait_for_sign_in(&polling_client(answers()), &authorization(5, 600)).await;

    // Assert: 5 s, the asked 600 s, then the grown 15 s interval; a wait
    // past the code's lifetime ends the sign-in.
    assert_eq!(result, Ok(signed_in()));
    assert_eq!(elapsed, Duration::from_secs(620));
    assert_eq!(expired, Err(PlatformError::ExpiredToken));
}

#[tokio::test(start_paused = true)]
async fn polling_ends_on_denial_expiry_or_an_unexpected_error() {
    // Arrange: the last code expires before its third poll.
    let cases = [
        (
            vec![Err(PlatformError::AccessDenied)],
            600,
            PlatformError::AccessDenied,
        ),
        (
            vec![Err(PlatformError::ExpiredToken)],
            600,
            PlatformError::ExpiredToken,
        ),
        (
            vec![Err(PlatformError::InvalidResponse)],
            600,
            PlatformError::InvalidResponse,
        ),
        (
            vec![
                Err(PlatformError::AuthorizationPending),
                Err(PlatformError::AuthorizationPending),
            ],
            12,
            PlatformError::ExpiredToken,
        ),
    ];

    for (answers, expires_in, expected) in cases {
        let client = polling_client(answers);

        // Act
        let result = wait_for_sign_in(&client, &authorization(5, expires_in)).await;

        // Assert
        assert_eq!(result, Err(expected));
    }
}

#[tokio::test]
async fn account_check_registers_only_when_a_relay_is_offered() {
    // Arrange
    let instance = ("a".repeat(64), INSTANCE_NAME.to_string());
    let mut no_relay = MockPlatformClient::new();
    no_relay.expect_account().returning(|_| Ok(account(false)));
    let mut registering = MockPlatformClient::new();
    registering
        .expect_account()
        .returning(|_| Ok(account(true)));
    registering
        .expect_register_instance()
        .with(always(), eq("a".repeat(64)), eq(INSTANCE_NAME))
        .times(1)
        .returning(|_, instance_id, _| {
            Ok(RegisteredInstance {
                instance_id: instance_id.to_string(),
                name: "Box on the platform".to_string(),
            })
        });
    let mut limited = MockPlatformClient::new();
    limited.expect_account().returning(|_| Ok(account(true)));
    limited
        .expect_register_instance()
        .returning(|_, _, _| Err(PlatformError::InstanceLimit));
    let mut waitlisted = MockPlatformClient::new();
    waitlisted
        .expect_account()
        .returning(|_| Err(PlatformError::Waitlisted));

    // Act
    let no_relay = check_account(&no_relay, &token(), instance.clone()).await;
    let registered = check_account(&registering, &token(), instance.clone()).await;
    let limited = check_account(&limited, &token(), instance.clone()).await;
    let waitlisted = check_account(&waitlisted, &token(), instance).await;

    // Assert
    let login = Some("octocat".to_string());
    assert_eq!(no_relay, (login.clone(), Ok(Access::NoRelay)));
    assert_eq!(
        registered,
        (
            login.clone(),
            Ok(Access::Registered {
                name: "Box on the platform".to_string(),
                service: service(),
            })
        )
    );
    assert_eq!(limited, (login, Err(PlatformError::InstanceLimit)));
    assert_eq!(waitlisted, (None, Err(PlatformError::Waitlisted)));
}

#[tokio::test]
async fn enabling_without_a_sign_in_for_this_platform_shows_signed_out_and_calls_nothing() {
    // Arrange: a sign-in from another platform, and its keep on, must never
    // reach this one.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    store_sign_in_keeping_on(&app, "https://other.example", true).await;

    // Act
    app.enable_platform(
        Arc::new(MockPlatformClient::new()),
        PLATFORM_URL.to_string(),
        INSTANCE_NAME.to_string(),
    )
    .await;
    app.tabs.set(Tab::Platform);

    // Assert
    assert!(app.platform.is_enabled());
    assert_eq!(app.tabs.current(), Tab::Platform);
    assert_eq!(app.platform.view().platform_url, PLATFORM_URL);
    assert_eq!(
        app.platform.view().state,
        PlatformState::SignedOut { notice: None }
    );
}

#[tokio::test]
async fn nothing_happens_without_a_client_a_token_or_a_matching_state() {
    // Arrange: the platform is not enabled.
    let (mut app, _base_dir) = test_support::new_test_app().await;
    let hidden = app.view_snapshot().platform.is_none();

    // Act
    app.activate_platform().await;
    let without_client = app.platform.view().state.clone();
    app.platform.view.state = PlatformState::SignedIn(PlatformAccountView {
        access: PlatformAccess::NoRelay,
        ..PlatformAccountView::checking(None)
    });
    app.activate_platform().await;
    app.toggle_platform_keep_on().await;
    let without_token = account_view(&app).cloned();
    app.platform.view.state = PlatformState::SignedOut { notice: None };
    app.set_platform_access(PlatformAccess::Waitlisted);
    app.set_platform_link_notice(Some("Link copied.".to_string()));
    app.toggle_platform_keep_on().await;

    // Assert
    assert!(hidden);
    assert_eq!(without_client, PlatformState::SignedOut { notice: None });
    assert_eq!(
        without_token.map(|account| (account.access, account.keep_on)),
        Some((PlatformAccess::NoRelay, false))
    );
    assert_eq!(
        app.platform.view().state,
        PlatformState::SignedOut { notice: None }
    );
}

#[tokio::test]
async fn an_unreadable_instance_key_stops_registration_and_sign_out_skips_it() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client.expect_remove_instance().times(0);
    client.expect_revoke_token().times(1).returning(|_| Ok(()));
    let (mut app, _base_dir) = platform_app(client).await;
    app.services
        .db()
        .settings()
        .upsert_setting(SettingName::RemoteInstanceKey, "not a key")
        .await
        .expect("key should save");
    sign_in_directly(&mut app, None, PlatformAccess::NoRelay);

    // Act
    app.activate_platform().await;
    let access = account_view(&app).map(|account| account.access.clone());
    app.sign_out_platform().await;
    apply_events_until(&mut app, |app| {
        signed_out_notice(app) == Some("Signed out.")
    })
    .await;

    // Assert
    assert_eq!(
        access,
        Some(PlatformAccess::Unavailable {
            message: "The stored remote key is invalid.".to_string(),
        })
    );
}

#[tokio::test]
async fn a_sign_in_that_cannot_be_saved_lasts_until_agentty_quits() {
    // Arrange
    let (mut app, _base_dir, pool) = test_support::new_git_test_app_with_pool().await;
    app.enable_platform(
        Arc::new(MockPlatformClient::new()),
        PLATFORM_URL.to_string(),
        INSTANCE_NAME.to_string(),
    )
    .await;
    pool.close().await;

    // Act
    app.apply_platform_update(PlatformUpdate::SignInFinished {
        request_id: 0,
        result: Ok(signed_in()),
    })
    .await;

    // Assert: the `Enter` that uses it anyway is to connect.
    assert!(app.platform.token.is_some());
    assert!(matches!(
        account_view(&app).map(|account| &account.access),
        Some(PlatformAccess::Unavailable { message }) if message.starts_with("Could not save")
    ));
    assert!(app.platform.connects_on_next_check);
}

#[tokio::test]
async fn using_an_unsaved_sign_in_anyway_ends_connected_once() {
    // Arrange: the state a sign-in that could not be saved leaves.
    let (mut app, _base_dir) =
        platform_app_with_launcher(registering_client(1), platform_relay_launcher(1)).await;
    sign_in_directly(
        &mut app,
        Some("octocat"),
        PlatformAccess::Unavailable {
            message: "Could not save the sign-in.".to_string(),
        },
    );
    app.platform.connects_on_next_check = true;

    // Act
    app.activate_platform().await;
    let connects_again = app.platform.connects_on_next_check;
    apply_events_until(&mut app, is_remote_control_running).await;

    // Assert
    assert!(!connects_again);
    assert!(app.platform.view().is_registered());
}

#[tokio::test]
async fn a_saved_sign_in_is_checked_at_launch_and_connects_only_with_keep_on() {
    // Arrange: keep on as saved with the sign-in, if at all. Remote control
    // that ran when Agentty quit leaves nothing behind.
    let cases = [(None, false), (Some(false), false), (Some(true), true)];

    for (keep_on, reconnects) in cases {
        let mut client = MockPlatformClient::new();
        client
            .expect_account()
            .withf(|token| token.expose() == TOKEN)
            .times(1)
            .returning(|_| Ok(account(true)));
        client
            .expect_register_instance()
            .with(always(), always(), eq(INSTANCE_NAME))
            .times(1)
            .returning(|_, instance_id, name| {
                Ok(RegisteredInstance {
                    instance_id: instance_id.to_string(),
                    name: name.to_string(),
                })
            });
        let launcher = platform_relay_launcher(usize::from(reconnects));
        let (mut app, _base_dir) = app_with_fakes(
            launcher,
            MockBrowserClient::new(),
            MockTerminalClipboard::new(),
        )
        .await;
        match keep_on {
            Some(keep_on) => store_sign_in_keeping_on(&app, PLATFORM_URL, keep_on).await,
            None => store_sign_in(&app, PLATFORM_URL).await,
        }

        // Act
        enable(&mut app, client).await;
        let checking = account_view(&app).cloned();
        let was_running_before_check = is_remote_control_running(&app);
        apply_events_until(&mut app, |app| app.platform.view().is_registered()).await;

        // Assert
        let keep_on = keep_on.unwrap_or_default();
        assert_eq!(
            checking,
            Some(PlatformAccountView {
                keep_on,
                ..PlatformAccountView::checking(None)
            })
        );
        assert_eq!(
            account_view(&app).cloned(),
            Some(PlatformAccountView {
                access: PlatformAccess::Registered {
                    name: INSTANCE_NAME.to_string(),
                },
                keep_on,
                ..PlatformAccountView::checking(Some("octocat".to_string()))
            })
        );
        assert!(!was_running_before_check);
        assert_eq!(is_remote_control_running(&app), reconnects, "{keep_on}");
    }
}

#[tokio::test]
async fn sign_in_asks_for_a_code_shows_it_and_reports_a_denial() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_start_device_authorization()
        .with(eq(INSTANCE_NAME))
        .times(1)
        .returning(|_| Ok(authorization(1, 600)));
    client
        .expect_poll_device_token()
        .times(1)
        .returning(|_| Err(PlatformError::AccessDenied));
    let (mut app, _base_dir) = platform_app(client).await;

    // Act
    app.activate_platform().await;
    let requesting = app.platform.view().state.clone();
    apply_events_until(&mut app, |app| {
        matches!(app.platform.view().state, PlatformState::SigningIn(_))
    })
    .await;
    let signing_in = app.platform.view().state.clone();
    apply_events_until(&mut app, |app| signed_out_notice(app).is_some()).await;

    // Assert
    assert_eq!(requesting, PlatformState::Requesting);
    assert_eq!(
        signing_in,
        PlatformState::SigningIn(PlatformSignInView {
            link: "https://platform.example/device?user_code=BCDF-GHJK".to_string(),
            notice: None,
            user_code: "BCDF-GHJK".to_string(),
        })
    );
    assert_eq!(
        signed_out_notice(&app),
        Some("The sign-in was denied in the browser.")
    );
}

#[tokio::test]
async fn a_platform_that_refuses_to_start_a_sign_in_is_reported() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_start_device_authorization()
        .times(1)
        .returning(|_| Err(PlatformError::Unreachable));
    let (mut app, _base_dir) = platform_app(client).await;

    // Act
    app.activate_platform().await;
    apply_events_until(&mut app, |app| signed_out_notice(app).is_some()).await;

    // Assert
    assert_eq!(
        signed_out_notice(&app),
        Some("Sign-in failed: Could not reach the platform.")
    );
}

#[tokio::test]
async fn cancelling_a_sign_in_stops_its_polling_and_drops_its_late_results() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client.expect_start_device_authorization().returning(|_| {
        Ok(DeviceAuthorization {
            verification_uri_complete: None,
            ..authorization(5, 600)
        })
    });
    client
        .expect_poll_device_token()
        .returning(|_| Err(PlatformError::AuthorizationPending));
    let (mut app, _base_dir) = platform_app(client).await;
    app.activate_platform().await;
    apply_events_until(&mut app, |app| {
        matches!(app.platform.view().state, PlatformState::SigningIn(_))
    })
    .await;
    let link = match &app.platform.view().state {
        PlatformState::SigningIn(sign_in) => Some(sign_in.link.clone()),
        _ => None,
    };

    // Act
    app.activate_platform().await;
    for request_id in 0..=1 {
        app.apply_platform_update(PlatformUpdate::SignInFinished {
            request_id,
            result: Ok(signed_in()),
        })
        .await;
        app.apply_platform_update(PlatformUpdate::Authorized {
            request_id,
            authorization: Box::new(authorization(5, 600)),
        })
        .await;
        app.apply_platform_update(PlatformUpdate::Refreshed {
            request_id,
            login: None,
            result: Ok(Access::NoRelay),
        })
        .await;
    }
    let notice = signed_out_notice(&app).map(str::to_string);
    // A sign-in still running when the app closes stops with it.
    app.activate_platform().await;

    // Assert
    assert_eq!(link.as_deref(), Some("https://platform.example/device"));
    assert_eq!(notice.as_deref(), Some("Sign-in cancelled."));
    assert_eq!(stored_sign_in(&app).await, None);
}

#[tokio::test]
async fn failed_sign_ins_say_why_in_plain_words() {
    // Arrange
    let cases = [
        (
            PlatformError::ExpiredToken,
            "The sign-in code expired before it was approved. Press Enter to try again.",
        ),
        (
            PlatformError::Unreachable,
            "Sign-in failed: Could not reach the platform.",
        ),
    ];
    let (mut app, _base_dir) = platform_app(MockPlatformClient::new()).await;

    for (error, expected) in cases {
        // Act
        app.apply_platform_update(PlatformUpdate::SignInFinished {
            request_id: 0,
            result: Err(error),
        })
        .await;

        // Assert
        assert_eq!(signed_out_notice(&app), Some(expected));
    }
}

#[tokio::test]
async fn approved_sign_in_saves_the_token_and_a_waitlisted_account_keeps_it() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_account()
        .times(2)
        .returning(|_| Err(PlatformError::Waitlisted));
    let (mut app, _base_dir) = platform_app(client).await;

    // Act
    app.apply_platform_update(PlatformUpdate::SignInFinished {
        request_id: 0,
        result: Ok(signed_in()),
    })
    .await;
    apply_events_until(&mut app, is_waitlisted).await;
    let waitlisted = account_view(&app).cloned();
    app.activate_platform().await;
    let rechecking = account_view(&app).map(|account| account.access.clone());
    apply_events_until(&mut app, is_waitlisted).await;

    // Assert
    let stored: serde_json::Value =
        serde_json::from_str(&stored_sign_in(&app).await.expect("sign-in should be saved"))
            .expect("stored sign-in should parse");
    // A fresh sign-in starts with keep on off, and nothing starts without
    // a registration; the launcher has no listener to give.
    assert_eq!(
        stored,
        serde_json::json!({ "access_token": TOKEN, "keep_on": false, "platform_url": PLATFORM_URL })
    );
    assert_eq!(
        waitlisted,
        Some(PlatformAccountView {
            access: PlatformAccess::Waitlisted,
            ..PlatformAccountView::checking(Some("octocat".to_string()))
        })
    );
    assert_eq!(rechecking, Some(PlatformAccess::Checking));
    assert!(app.platform.token.is_some());
    assert!(!is_remote_control_running(&app));
}

#[tokio::test]
async fn an_approved_sign_in_ends_with_remote_control_running_through_the_relay() {
    // Arrange
    let (mut app, _base_dir) =
        platform_app_with_launcher(registering_client(1), platform_relay_launcher(1)).await;

    // Act
    app.apply_platform_update(PlatformUpdate::SignInFinished {
        request_id: 0,
        result: Ok(signed_in()),
    })
    .await;
    let is_running_before_check = is_remote_control_running(&app);
    apply_events_until(&mut app, is_remote_control_running).await;

    // Assert: connected for this launch only.
    assert!(!is_running_before_check);
    assert!(app.platform.view().is_registered());
    assert!(!is_kept_on(&app));
    assert_eq!(stored_keep_on(&app).await, Some(false));
}

#[tokio::test]
async fn a_revoked_token_signs_this_agentty_out_and_forgets_the_token() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_account()
        .times(1)
        .returning(|_| Err(PlatformError::InvalidToken));
    let (mut app, _base_dir) = test_support::new_test_app().await;
    store_sign_in_keeping_on(&app, PLATFORM_URL, true).await;
    app.enable_platform(
        Arc::new(client),
        PLATFORM_URL.to_string(),
        INSTANCE_NAME.to_string(),
    )
    .await;

    // Act
    apply_events_until(&mut app, |app| signed_out_notice(app).is_some()).await;

    // Assert: the revoked sign-in never reached remote control, and keep
    // on went with it.
    assert_eq!(signed_out_notice(&app), Some(SIGNED_OUT_BY_PLATFORM));
    assert_eq!(stored_sign_in(&app).await, Some(String::new()));
    assert!(app.platform.token.is_none());
    assert!(!is_remote_control_running(&app));
}

#[tokio::test]
async fn account_outcomes_show_what_this_agentty_may_use_and_start_nothing_unregistered() {
    // Arrange: each check is to connect, yet without a registration the
    // launcher, which has no listener to give, is never asked.
    let cases = [
        (Ok(Access::NoRelay), PlatformAccess::NoRelay),
        (Err(PlatformError::Waitlisted), PlatformAccess::Waitlisted),
        (
            Err(PlatformError::InstanceLimit),
            PlatformAccess::Unavailable {
                message: PlatformError::InstanceLimit.to_string(),
            },
        ),
    ];
    let (mut app, _base_dir) = platform_app(MockPlatformClient::new()).await;
    sign_in_directly(&mut app, None, PlatformAccess::Checking);

    for (result, expected) in cases {
        app.platform.starts_remote_control_after_check = true;

        // Act
        app.apply_platform_update(PlatformUpdate::Refreshed {
            request_id: 0,
            login: Some("octocat".to_string()),
            result,
        })
        .await;

        // Assert
        assert_eq!(
            account_view(&app).map(|account| &account.access),
            Some(&expected)
        );
        assert!(!is_remote_control_running(&app));
    }
}

#[tokio::test]
async fn checking_again_after_an_unregistered_answer_connects_only_with_keep_on() {
    for keep_on in [false, true] {
        // Arrange: the platform offered no relay at the last check.
        let (mut app, _base_dir) = platform_app_with_launcher(
            registering_client(1),
            platform_relay_launcher(usize::from(keep_on)),
        )
        .await;
        sign_in_directly(&mut app, Some("octocat"), PlatformAccess::NoRelay);
        if let PlatformState::SignedIn(account) = &mut app.platform.view.state {
            account.keep_on = keep_on;
        }

        // Act
        app.activate_platform().await;
        apply_events_until(&mut app, |app| app.platform.view().is_registered()).await;

        // Assert
        assert_eq!(is_remote_control_running(&app), keep_on, "{keep_on}");
    }
}

#[tokio::test]
async fn turning_keep_on_on_while_registered_connects_and_off_leaves_it_running() {
    // Arrange
    let (mut app, _base_dir) =
        platform_app_with_launcher(registering_client(1), platform_relay_launcher(1)).await;
    store_sign_in(&app, PLATFORM_URL).await;
    sign_in_registered(&mut app).await;

    // Act
    app.toggle_platform_keep_on().await;
    let checking = account_view(&app).map(|account| account.access.clone());
    apply_events_until(&mut app, is_remote_control_running).await;
    let saved_on = stored_keep_on(&app).await;
    app.toggle_platform_keep_on().await;

    // Assert
    assert_eq!(checking, Some(PlatformAccess::Checking));
    assert_eq!(saved_on, Some(true));
    assert!(!is_kept_on(&app));
    assert_eq!(stored_keep_on(&app).await, Some(false));
    assert!(is_remote_control_running(&app));
}

#[tokio::test]
async fn stopping_with_enter_while_kept_on_stays_stopped_for_this_launch() {
    // Arrange: keep on is on and remote control runs.
    let (mut app, _base_dir) =
        platform_app_with_launcher(registering_client(1), platform_relay_launcher(1)).await;
    store_sign_in(&app, PLATFORM_URL).await;
    sign_in_registered(&mut app).await;
    app.toggle_platform_keep_on().await;
    apply_events_until(&mut app, is_remote_control_running).await;

    // Act
    app.activate_platform().await;
    // A late answer to the check that connected starts nothing again.
    let request_id = app.platform.request_id;
    app.apply_platform_update(PlatformUpdate::Refreshed {
        request_id,
        login: None,
        result: Ok(Access::Registered {
            name: INSTANCE_NAME.to_string(),
            service: service(),
        }),
    })
    .await;

    // Assert
    assert!(!is_remote_control_running(&app));
    assert!(is_kept_on(&app));
    assert_eq!(stored_keep_on(&app).await, Some(true));
}

#[tokio::test]
async fn keep_on_waits_for_the_check_and_starting_or_stopping_never_changes_it() {
    // Arrange
    let (mut app, _base_dir) =
        platform_app_with_launcher(MockPlatformClient::new(), platform_relay_launcher(1)).await;
    store_sign_in(&app, PLATFORM_URL).await;
    sign_in_directly(&mut app, Some("octocat"), PlatformAccess::Checking);

    // Act
    app.toggle_platform_keep_on().await;
    let while_checking = is_kept_on(&app);
    sign_in_registered(&mut app).await;
    app.toggle_remote_control().await;
    let started = is_remote_control_running(&app);
    app.toggle_remote_control().await;

    // Assert
    assert!(!while_checking);
    assert!(started);
    assert!(!is_kept_on(&app));
    assert_eq!(stored_keep_on(&app).await, None);
}

#[tokio::test]
async fn a_keep_on_change_that_cannot_be_saved_lasts_until_agentty_quits() {
    // Arrange
    let (mut app, _base_dir, pool) = test_support::new_git_test_app_with_pool().await;
    enable(&mut app, MockPlatformClient::new()).await;
    sign_in_directly(&mut app, Some("octocat"), PlatformAccess::NoRelay);
    pool.close().await;

    // Act
    app.toggle_platform_keep_on().await;

    // Assert
    assert_eq!(
        account_view(&app).map(|account| (account.keep_on, account.keep_on_notice.as_deref())),
        Some((
            true,
            Some("Could not save keep on, so the change lasts only until Agentty quits.")
        ))
    );
}

#[tokio::test]
async fn sign_out_stops_remote_control_and_clears_keep_on() {
    // Arrange
    let mut client = registering_client(1);
    client.expect_remove_instance().returning(|_, _| Ok(()));
    client.expect_revoke_token().returning(|_| Ok(()));
    let (mut app, _base_dir) = platform_app_with_launcher(client, platform_relay_launcher(1)).await;
    store_sign_in(&app, PLATFORM_URL).await;
    sign_in_registered(&mut app).await;
    app.toggle_platform_keep_on().await;
    apply_events_until(&mut app, is_remote_control_running).await;

    // Act
    app.sign_out_platform().await;

    // Assert
    assert!(!is_remote_control_running(&app));
    assert_eq!(stored_sign_in(&app).await, Some(String::new()));
}

#[tokio::test]
async fn a_registered_agentty_checks_the_account_then_starts_remote_control_through_its_relay() {
    // Arrange: without `--experimental-remote`, a free port is enough.
    let (mut app, _base_dir) =
        platform_app_with_launcher(registering_client(1), platform_relay_launcher(1)).await;
    sign_in_directly(&mut app, None, PlatformAccess::Checking);
    app.activate_platform().await;
    app.apply_platform_update(PlatformUpdate::Refreshed {
        request_id: 0,
        login: None,
        result: Ok(Access::Registered {
            name: INSTANCE_NAME.to_string(),
            service: service(),
        }),
    })
    .await;
    app.tabs.set(Tab::Settings);

    // Act
    app.next_tab();
    let tab = app.tabs.current();
    app.activate_platform().await;
    let checking = account_view(&app).map(|account| account.access.clone());
    apply_events_until(&mut app, is_remote_control_running).await;
    let link = app
        .remote_control
        .view()
        .listener
        .as_ref()
        .and_then(|listener| listener.relay.as_ref())
        .map(|relay| relay.link.clone());
    app.activate_platform().await;

    // Assert
    assert_eq!(tab, Tab::Platform);
    assert_eq!(checking, Some(PlatformAccess::Checking));
    assert!(link.is_some_and(|link| link.starts_with("https://platform.example/sessions/#relay=")));
    assert_eq!(app.remote_control.view().listener, None);
}

/// Listener that serves nothing and issues one fixed pairing code.
struct StubServer {
    status_tx: watch::Sender<RemoteServerStatus>,
}

impl Default for StubServer {
    fn default() -> Self {
        Self {
            status_tx: watch::Sender::new(RemoteServerStatus::default()),
        }
    }
}

#[async_trait::async_trait]
impl RemoteControlServer for StubServer {
    fn local_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    fn issue_pairing_code(&self) -> Result<String, RemoteError> {
        Ok("ABCD-EFGH".to_string())
    }

    fn status(&self) -> watch::Receiver<RemoteServerStatus> {
        self.status_tx.subscribe()
    }

    fn approve_pairing(&self, _request_id: u64) -> bool {
        false
    }

    fn deny_pairing(&self, _request_id: u64) -> bool {
        false
    }

    async fn revoke_device(&self, _device_id: &str) -> Result<(), String> {
        Ok(())
    }

    async fn shutdown(self: Box<Self>) {}
}

#[tokio::test]
async fn sign_out_forgets_the_token_at_once_and_tells_the_platform() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_remove_instance()
        .withf(|token, instance_id| token.expose() == TOKEN && instance_id.len() == 64)
        .times(1)
        .returning(|_, _| Ok(()));
    client
        .expect_revoke_token()
        .withf(|token| token.expose() == TOKEN)
        .times(1)
        .returning(|_| Ok(()));
    let (mut app, _base_dir) = platform_app(client).await;
    store_sign_in(&app, PLATFORM_URL).await;
    sign_in_directly(&mut app, Some("octocat"), PlatformAccess::Waitlisted);

    // Act
    app.sign_out_platform().await;
    let signing_out = app.platform.view().state.clone();
    let forgotten = stored_sign_in(&app).await;
    // A new sign-in waits until the old registration is removed.
    app.activate_platform().await;
    let while_signing_out = app.platform.view().state.clone();
    apply_events_until(&mut app, |app| {
        signed_out_notice(app) == Some("Signed out.")
    })
    .await;
    app.sign_out_platform().await;

    // Assert
    assert_eq!(signing_out, PlatformState::SigningOut { notice: None });
    assert_eq!(while_signing_out, signing_out);
    assert_eq!(forgotten, Some(String::new()));
    assert!(app.platform.token.is_none());
}

/// Platform whose registration waits until the instance is removed, and
/// that records the calls it gets in order.
#[derive(Default)]
struct RacingPlatform {
    calls: Arc<Mutex<Vec<&'static str>>>,
    removed: Arc<tokio::sync::Notify>,
}

impl RacingPlatform {
    /// Records `call`.
    fn record(&self, call: &'static str) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

#[async_trait::async_trait]
impl PlatformClient for RacingPlatform {
    async fn start_device_authorization(
        &self,
        _client_name: &str,
    ) -> Result<DeviceAuthorization, PlatformError> {
        Err(PlatformError::Unreachable)
    }

    async fn poll_device_token(&self, _device_code: &DeviceCode) -> Result<SignIn, PlatformError> {
        Err(PlatformError::Unreachable)
    }

    async fn account(&self, _token: &AccessToken) -> Result<PlatformAccount, PlatformError> {
        Ok(account(true))
    }

    async fn register_instance(
        &self,
        _token: &AccessToken,
        instance_id: &str,
        name: &str,
    ) -> Result<RegisteredInstance, PlatformError> {
        self.record("register-start");
        self.removed.notified().await;
        self.record("register-end");

        Ok(RegisteredInstance {
            instance_id: instance_id.to_string(),
            name: name.to_string(),
        })
    }

    async fn remove_instance(
        &self,
        _token: &AccessToken,
        _instance_id: &str,
    ) -> Result<(), PlatformError> {
        self.record("remove");
        self.removed.notify_one();

        Ok(())
    }

    async fn revoke_token(&self, _token: &AccessToken) -> Result<(), PlatformError> {
        self.record("revoke");

        Ok(())
    }
}

#[tokio::test]
async fn sign_out_stops_a_running_registration_before_removing_the_instance() {
    // Arrange
    let platform = RacingPlatform::default();
    let calls = Arc::clone(&platform.calls);
    let (mut app, _base_dir) = platform_app(platform).await;
    sign_in_directly(&mut app, Some("octocat"), PlatformAccess::Waitlisted);
    app.activate_platform().await;
    let registering = async {
        while !calls
            .lock()
            .expect("calls lock")
            .contains(&"register-start")
        {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), registering)
        .await
        .expect("the account check should start registering");

    // Act
    app.sign_out_platform().await;
    apply_events_until(&mut app, |app| {
        signed_out_notice(app) == Some("Signed out.")
    })
    .await;
    // A registration that kept running would finish right after the removal.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Assert
    assert_eq!(
        *calls.lock().expect("calls lock"),
        ["register-start", "remove", "revoke"]
    );
    assert!(app.platform.task.is_none());
}

#[tokio::test]
async fn sign_out_warns_when_the_saved_sign_in_cannot_be_removed() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client.expect_revoke_token().times(1).returning(|_| Ok(()));
    let (mut app, _base_dir, pool) = test_support::new_git_test_app_with_pool().await;
    app.enable_platform(
        Arc::new(client),
        PLATFORM_URL.to_string(),
        INSTANCE_NAME.to_string(),
    )
    .await;
    sign_in_directly(&mut app, Some("octocat"), PlatformAccess::NoRelay);
    pool.close().await;

    // Act
    app.sign_out_platform().await;
    let signing_out = app.platform.view().state.clone();
    apply_events_until(&mut app, |app| {
        signed_out_notice(app).is_some_and(|notice| notice.starts_with("Signed out."))
    })
    .await;

    // Assert
    let warning = "The saved sign-in could not be removed, so it returns when Agentty restarts.";
    assert_eq!(
        signing_out,
        PlatformState::SigningOut {
            notice: Some(warning.to_string())
        }
    );
    assert_eq!(
        signed_out_notice(&app),
        Some(format!("Signed out. {warning}").as_str())
    );
}

#[tokio::test]
async fn sign_out_says_when_the_platform_did_not_confirm_it() {
    // Arrange
    let cases = [
        (Ok(()), "Signed out.".to_string()),
        (Err(PlatformError::InvalidToken), "Signed out.".to_string()),
        (
            Err(PlatformError::Waitlisted),
            format!(
                "Signed out on this computer, but the platform did not confirm it: {}",
                PlatformError::Waitlisted
            ),
        ),
    ];
    let (mut app, _base_dir) = platform_app(MockPlatformClient::new()).await;

    for (result, expected) in cases {
        app.platform.view.state = PlatformState::SigningOut { notice: None };

        // Act
        app.apply_platform_update(PlatformUpdate::SignedOut {
            request_id: 0,
            result,
        })
        .await;
        app.apply_platform_update(PlatformUpdate::SignedOut {
            request_id: 7,
            result: Ok(()),
        })
        .await;

        // Assert
        assert_eq!(signed_out_notice(&app), Some(expected.as_str()));
    }
}

#[tokio::test]
async fn sign_in_page_opens_and_copies_through_the_remote_tab_paths() {
    // Arrange
    let link = "https://platform.example/device?user_code=BCDF-GHJK";
    let mut browser = MockBrowserClient::new();
    browser
        .expect_open()
        .with(eq(link))
        .times(1)
        .return_const(());
    let mut clipboard = MockTerminalClipboard::new();
    clipboard
        .expect_copy_through_tmux()
        .with(eq(link))
        .times(2)
        .return_const(false);
    let answers = Mutex::new(VecDeque::from([true, false]));
    clipboard
        .expect_copy_through_terminal()
        .with(eq(link))
        .times(2)
        .returning(move |_| {
            answers
                .lock()
                .expect("answers lock")
                .pop_front()
                .unwrap_or_default()
        });
    let (mut app, _base_dir) = platform_app_with(
        MockPlatformClient::new(),
        MockRemoteControlLauncher::new(),
        browser,
        clipboard,
    )
    .await;
    app.open_platform_link();
    app.copy_platform_link();
    app.apply_platform_update(PlatformUpdate::Authorized {
        request_id: 0,
        authorization: Box::new(authorization(5, 600)),
    })
    .await;

    // Act
    app.open_platform_link();
    app.copy_platform_link();
    let notice_while_copying = sign_in_notice(&app);
    apply_events_until(&mut app, |app| sign_in_notice(app).is_some()).await;
    let copied = sign_in_notice(&app);
    app.copy_platform_link();
    apply_events_until(&mut app, |app| sign_in_notice(app).is_some()).await;
    let not_copied = sign_in_notice(&app);
    app.apply_app_events(AppEvent::PlatformLinkCopied {
        is_copied: true,
        request_id: 1,
    })
    .await;

    // Assert
    assert_eq!(notice_while_copying, None);
    assert_eq!(copied.as_deref(), Some("Link copied."));
    assert_eq!(
        not_copied.as_deref(),
        Some("Could not copy the link; select it instead.")
    );
    assert_eq!(sign_in_notice(&app), not_copied);
}

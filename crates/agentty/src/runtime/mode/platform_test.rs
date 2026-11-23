//! Platform tab key dispatch tests.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::app::platform::{Access, PlatformUpdate};
use crate::infra::platform::{
    AccessToken, DeviceAuthorization, DeviceCode, MockPlatformClient, PlatformError,
    RemoteControlService, SignIn,
};
use crate::presentation::platform::PlatformState;
use crate::test_support;

/// Builds a key press of `character`.
fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

#[tokio::test]
async fn platform_keys_follow_the_sign_in_state() {
    // Arrange
    let mut client = MockPlatformClient::new();
    client
        .expect_account()
        .returning(|_| Err(PlatformError::Waitlisted));
    client.expect_remove_instance().returning(|_, _| Ok(()));
    client.expect_revoke_token().returning(|_| Ok(()));
    let (mut app, _base_dir) = test_support::new_test_app().await;
    app.enable_platform(
        Arc::new(client),
        "https://platform.example".to_string(),
        "Agentty on box".to_string(),
    )
    .await;
    handle(&mut app, key('l')).await;
    let signed_out = app.platform.view().state.clone();
    app.apply_platform_update(PlatformUpdate::Authorized {
        request_id: 0,
        authorization: Box::new(DeviceAuthorization {
            device_code: DeviceCode::new("device-code".to_string()),
            expires_in: std::time::Duration::from_secs(600),
            interval: std::time::Duration::from_secs(5),
            user_code: "BCDF-GHJK".to_string(),
            verification_uri: "https://platform.example/device".to_string(),
            verification_uri_complete: None,
        }),
    })
    .await;

    // Act: open, copy, and an unused key while the sign-in waits.
    handle(&mut app, key('o')).await;
    handle(&mut app, key('c')).await;
    handle(&mut app, key('x')).await;
    let signing_in = matches!(app.platform.view().state, PlatformState::SigningIn(_));
    app.apply_platform_update(PlatformUpdate::SignInFinished {
        request_id: 0,
        result: Ok(SignIn {
            access_token: AccessToken::new("agp_test-token".to_string()),
            login: "octocat".to_string(),
        }),
    })
    .await;
    handle(&mut app, key('n')).await;
    app.apply_platform_update(PlatformUpdate::Refreshed {
        request_id: 1,
        login: None,
        result: Ok(Access::Registered {
            name: "Agentty on box".to_string(),
            service: RemoteControlService {
                pairing_url: "https://platform.example/sessions/".to_string(),
                relay_url: "wss://relay.platform.example".to_string(),
            },
        }),
    })
    .await;
    let is_registered = app.platform.view().is_registered();
    handle(&mut app, key('n')).await;
    handle(&mut app, key('l')).await;

    // Assert
    assert_eq!(signed_out, PlatformState::SignedOut { notice: None });
    assert!(signing_in);
    assert!(is_registered);
    assert_eq!(
        app.platform.view().state,
        PlatformState::SigningOut { notice: None }
    );
}

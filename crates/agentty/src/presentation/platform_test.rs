//! Platform view projection tests.

use ag_remote::RelayStatus;

use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformSignInView, PlatformState, PlatformTabView,
    PlatformView,
};
use crate::presentation::remote_control::{RemoteControlView, RemoteDeviceView, RemoteRelayView};

/// Builds a view of the platform in `state`.
fn view(state: PlatformState) -> PlatformView {
    PlatformView {
        state,
        ..PlatformView::new("https://platform.example".to_string())
    }
}

#[test]
fn only_a_signed_in_view_has_an_account_and_only_a_registered_one_uses_the_relay() {
    // Arrange
    let signed_in = |access| {
        view(PlatformState::SignedIn(PlatformAccountView {
            access,
            ..PlatformAccountView::checking(Some("octocat".to_string()))
        }))
    };
    let views = [
        (
            PlatformView::new("https://platform.example".to_string()),
            false,
            false,
        ),
        (view(PlatformState::Requesting), false, false),
        (
            view(PlatformState::SigningOut { notice: None }),
            false,
            false,
        ),
        (
            view(PlatformState::SigningIn(PlatformSignInView {
                link: "https://platform.example/device".to_string(),
                notice: None,
                user_code: "BCDF-GHJK".to_string(),
            })),
            false,
            false,
        ),
        (signed_in(PlatformAccess::Waitlisted), true, false),
        (
            signed_in(PlatformAccess::Registered {
                name: "Agentty on box".to_string(),
            }),
            true,
            true,
        ),
    ];

    for (view, has_account, is_registered) in views {
        // Act
        let account = view.account();

        // Assert
        assert_eq!(account.is_some(), has_account);
        assert_eq!(view.is_registered(), is_registered);
    }
}

#[test]
fn a_checked_account_offers_keep_on_starting_off() {
    // Arrange
    let checking = PlatformAccountView::checking(Some("octocat".to_string()));
    let answered = PlatformAccountView {
        access: PlatformAccess::NoRelay,
        ..checking.clone()
    };

    // Act, Assert
    assert!(!checking.keep_on);
    assert_eq!(checking.keep_on_notice, None);
    assert!(!checking.offers_keep_on());
    assert!(answered.offers_keep_on());
}

#[test]
fn tab_label_counts_connected_devices_only_while_remote_control_runs() {
    // Arrange
    let platform = PlatformView::new("https://platform.example".to_string());
    let devices = vec![
        RemoteDeviceView {
            id: "phone".to_string(),
            is_connected: true,
            name: "Phone".to_string(),
        },
        RemoteDeviceView {
            id: "tablet".to_string(),
            is_connected: false,
            name: "Tablet".to_string(),
        },
    ];
    let stopped = RemoteControlView {
        devices: devices.clone(),
        ..RemoteControlView::default()
    };
    let running = RemoteControlView {
        devices,
        relay: Some(RemoteRelayView::new(
            "https://platform.example/pair#code=ABCD".to_string(),
            RelayStatus::Connected,
        )),
        ..RemoteControlView::default()
    };

    // Act
    let stopped_label = PlatformTabView {
        platform: &platform,
        remote_control: &stopped,
    }
    .tab_label();
    let running_label = PlatformTabView {
        platform: &platform,
        remote_control: &running,
    }
    .tab_label();

    // Assert
    assert_eq!(stopped_label, "Platform");
    assert_eq!(running_label, "Platform (1 online)");
}

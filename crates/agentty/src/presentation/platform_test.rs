//! Platform view projection tests.

use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformSignInView, PlatformState, PlatformView,
};

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

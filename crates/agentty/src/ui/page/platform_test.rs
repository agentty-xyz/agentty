//! Platform tab rendering tests for each sign-in and account state.

use ag_remote::RelayStatus;

use super::PlatformPage;
use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformSignInView, PlatformState, PlatformTabView,
    PlatformView,
};
use crate::presentation::remote_control::{
    RemoteControlView, RemoteDeviceView, RemoteListenerView, RemotePairingRequestView,
    RemoteRelayView,
};
use crate::ui::render::Page;
use crate::ui::style::palette;

/// Returns a platform view in `state`.
fn platform(state: PlatformState) -> PlatformView {
    PlatformView {
        state,
        ..PlatformView::new("https://platform.example".to_string())
    }
}

/// Returns a signed-in platform view with `access`.
fn signed_in(access: PlatformAccess) -> PlatformView {
    platform(PlatformState::SignedIn(PlatformAccountView {
        access,
        ..PlatformAccountView::checking(Some("octocat".to_string()))
    }))
}

/// Returns the registered platform view.
fn registered() -> PlatformView {
    signed_in(PlatformAccess::Registered {
        name: "Agentty on box".to_string(),
    })
}

/// Renders the page on a `width` by `height` terminal and returns its text.
fn rendered_text(
    platform: &PlatformView,
    remote_control: &RemoteControlView,
    width: u16,
    height: u16,
) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut page = PlatformPage::new(
        PlatformTabView {
            platform,
            remote_control,
        },
        Some("agentty".to_string()),
    );
    terminal
        .draw(|frame| {
            let area = frame.area();
            page.render(frame, area);
        })
        .expect("failed to draw");
    let buffer = terminal.backend().buffer();

    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn signed_out_page_explains_what_signing_in_gives_and_offers_it() {
    // Arrange
    let view = platform(PlatformState::SignedOut {
        notice: Some("Signed out.".to_string()),
    });

    // Act
    let text = rendered_text(&view, &RemoteControlView::default(), 200, 30);

    // Assert
    assert!(text.contains("'agentty' on the platform"));
    assert!(text.contains("Address https://platform.example"));
    assert!(text.contains("Account Signed out"));
    assert!(text.contains("Sign in to connect this Agentty to the platform's relay"));
    assert!(text.contains("without a tunnel or a relay of your own"));
    assert!(text.contains("grants no device any access"));
    assert!(text.contains("Signed out."));
    assert!(text.contains("Enter: sign in and connect"));
}

#[test]
fn signing_in_page_shows_the_code_the_page_and_its_actions() {
    // Arrange
    let requesting = platform(PlatformState::Requesting);
    let sign_in = PlatformSignInView {
        link: "https://platform.example/device?user_code=BCDF-GHJK".to_string(),
        notice: Some("Link copied.".to_string()),
        user_code: "BCDF-GHJK".to_string(),
    };
    let waiting = platform(PlatformState::SigningIn(sign_in.clone()));
    let not_copied = platform(PlatformState::SigningIn(PlatformSignInView {
        notice: None,
        ..sign_in
    }));

    // Act
    let requesting = rendered_text(&requesting, &RemoteControlView::default(), 200, 30);
    let waiting = rendered_text(&waiting, &RemoteControlView::default(), 200, 30);
    let not_copied = rendered_text(&not_copied, &RemoteControlView::default(), 200, 30);

    // Assert
    assert!(requesting.contains("Signing in..."));
    assert!(requesting.contains("Enter: cancel"));
    assert!(waiting.contains("Code    BCDF-GHJK"));
    assert!(waiting.contains("Open    https://platform.example/device?user_code=BCDF-GHJK"));
    assert!(waiting.contains("Link copied."));
    assert!(waiting.contains("o: open page | c: copy link | Enter: cancel"));
    assert!(!not_copied.contains("Link copied."));
}

#[test]
fn signing_out_page_offers_no_sign_in_until_the_platform_answers() {
    // Arrange
    let quiet = platform(PlatformState::SigningOut { notice: None });
    let warned = platform(PlatformState::SigningOut {
        notice: Some("The saved sign-in could not be removed.".to_string()),
    });

    // Act
    let quiet = rendered_text(&quiet, &RemoteControlView::default(), 200, 30);
    let warned = rendered_text(&warned, &RemoteControlView::default(), 200, 30);

    // Assert
    assert!(quiet.contains("Account Signing out..."));
    assert!(!quiet.contains("Enter: sign in"));
    assert!(warned.contains("The saved sign-in could not be removed."));
}

#[test]
fn signed_in_page_says_what_the_account_may_use_in_plain_words() {
    // Arrange
    let cases = [
        (PlatformAccess::Checking, "Checking the account...", false),
        (
            PlatformAccess::NoRelay,
            "offers this account no relay",
            true,
        ),
        (
            PlatformAccess::Waitlisted,
            "on the platform's waitlist",
            true,
        ),
        (
            PlatformAccess::Unavailable {
                message: "Could not reach the platform.".to_string(),
            },
            "Could not reach the platform.",
            true,
        ),
    ];
    let checking = platform(PlatformState::SignedIn(PlatformAccountView::checking(None)));
    let checking_text = rendered_text(&checking, &RemoteControlView::default(), 200, 30);

    for (access, expected, has_answered) in cases {
        // Act
        let text = rendered_text(&signed_in(access), &RemoteControlView::default(), 200, 30);

        // Assert: keep on is offered once the check answered.
        assert!(text.contains("Account octocat"));
        assert!(text.contains("Keep on Off"));
        assert!(text.contains(expected));
        assert_eq!(text.contains("Enter: check again"), has_answered);
        assert_eq!(text.contains("a: keep on"), has_answered);
        assert!(text.contains("l: sign out"));
    }
    assert!(checking_text.contains("Account Signed in"));
}

#[test]
fn keep_on_shows_its_state_what_it_does_and_an_unsaved_change() {
    // Arrange
    let kept_on = platform(PlatformState::SignedIn(PlatformAccountView {
        access: PlatformAccess::NoRelay,
        keep_on: true,
        keep_on_notice: Some("Could not save keep on.".to_string()),
        login: None,
    }));

    // Act
    let text = rendered_text(&kept_on, &RemoteControlView::default(), 200, 30);

    // Assert
    assert!(text.contains("Keep on On"));
    assert!(text.contains("starts whenever Agentty starts"));
    assert!(text.contains("Could not save keep on."));
}

#[test]
fn registered_page_shows_remote_control_through_the_platform_relay() {
    // Arrange
    let stopped = RemoteControlView::default();
    let running = RemoteControlView {
        devices: vec![RemoteDeviceView {
            id: "phone".to_string(),
            is_connected: true,
            name: "Phone".to_string(),
        }],
        listener: Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: Some(RemoteRelayView::new(
                "https://platform.example/sessions/#relay=x".to_string(),
                RelayStatus::NotAdmitted,
            )),
            url: "http://127.0.0.1:50000/".to_string(),
        }),
        pairing_requests: vec![RemotePairingRequestView {
            confirmation: "K7QX".to_string(),
            device_name: "Laptop".to_string(),
            id: 1,
        }],
        ..RemoteControlView::default()
    };

    // Act
    let stopped = rendered_text(&registered(), &stopped, 200, 40);
    let running = rendered_text(&registered(), &running, 200, 80);

    // Assert
    assert!(stopped.contains("Name    Agentty on box"));
    assert!(stopped.contains("Remote  Off"));
    assert!(stopped.contains("Enter: start"));
    assert!(running.contains("Remote  On"));
    assert!(running.contains("Relay   Not admitted"));
    assert!(running.contains("The relay refused this Agentty"));
    assert!(running.contains("https://platform.example/sessions/#relay=x"));
    assert!(running.contains("Confirm K7QX"));
    assert!(running.contains("Phone"));
    assert!(running.contains("y: approve"));
    assert!(running.contains("l: sign out"));
    assert!(!running.contains("127.0.0.1:50000"));
}

#[test]
fn registered_page_shows_the_qr_code_only_when_it_fits() {
    // Arrange
    let relay = RemoteRelayView::new(
        "https://platform.example/sessions/#relay=x".to_string(),
        RelayStatus::Connected,
    );
    let first_qr_row = relay.qr_code[0].clone();
    let remote_control = RemoteControlView {
        listener: Some(RemoteListenerView {
            pairing_code: "ABCD-EFGH".to_string(),
            relay: Some(relay),
            url: "http://127.0.0.1:50000/".to_string(),
        }),
        ..RemoteControlView::default()
    };
    let platform = registered();
    let page = PlatformPage::new(
        PlatformTabView {
            platform: &platform,
            remote_control: &remote_control,
        },
        None,
    );

    // Act
    let tall = page.lines(200, 80);
    let short = page.lines(200, 10);

    // Assert
    let has_qr_code = |lines: &[ratatui::text::Line<'_>]| {
        lines.iter().any(|line| line.to_string() == first_qr_row)
    };
    assert!(has_qr_code(&tall));
    assert!(!has_qr_code(&short));
    assert_eq!(page.title(), "Agentty Platform");
    let relay_row = tall
        .iter()
        .find(|line| line.to_string().starts_with("Relay   Connected"))
        .expect("relay row should render");
    assert_eq!(relay_row.spans[1].style.fg, Some(palette::success()));
}

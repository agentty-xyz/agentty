//! Relay, pairing page, and platform address validation tests.

use super::{parse_page_url, parse_platform_url, parse_relay_url};

#[test]
fn relay_url_needs_a_websocket_scheme_and_a_host() {
    // Arrange
    let accepted = [
        ("wss://relay.example/", "wss://relay.example"),
        ("ws://127.0.0.1:8787", "ws://127.0.0.1:8787"),
        (" wss://relay.example/base ", "wss://relay.example/base"),
    ];
    let rejected = [
        "https://relay.example",
        "relay.example",
        "wss://",
        "wss:///path",
        "wss://relay.example/?token=1",
        "wss://relay.example/#x",
        "wss://relay example",
        "wss://user:token@relay.example",
    ];

    // Act
    let parsed: Vec<_> = accepted
        .iter()
        .map(|(value, _)| parse_relay_url(value))
        .collect();
    let refused: Vec<_> = rejected
        .iter()
        .map(|value| parse_relay_url(value))
        .collect();

    // Assert
    for ((_, expected), parsed) in accepted.iter().zip(parsed) {
        assert_eq!(parsed.as_deref(), Ok(*expected));
    }
    for refused in refused {
        assert_eq!(
            refused,
            Err("expected a relay URL such as wss://relay.example".to_string())
        );
    }
}

#[test]
fn page_url_needs_a_web_scheme_and_keeps_its_trailing_slash() {
    // Arrange
    let accepted = [
        (
            "https://platform.example/sessions/",
            "https://platform.example/sessions/",
        ),
        (" http://127.0.0.1:9/ ", "http://127.0.0.1:9/"),
        ("https://platform.example", "https://platform.example"),
        (
            "https://platform.example/device?user_code=BCDF-GHJK",
            "https://platform.example/device?user_code=BCDF-GHJK",
        ),
    ];
    let rejected = [
        "wss://platform.example/",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://",
        "https://platform.example/#fragment",
        "https://platform.example/?code=1#fragment",
        "https://platform.example/?code=1 2",
        "https://user@platform.example/",
        "https://platform.example/\u{1b}[2J",
        "https://platform.example/?code=\u{1b}",
    ];

    // Act
    let parsed: Vec<_> = accepted
        .iter()
        .map(|(value, _)| parse_page_url(value))
        .collect();
    let refused = rejected.iter().all(|value| parse_page_url(value).is_err());

    // Assert
    for ((_, expected), parsed) in accepted.iter().zip(parsed) {
        assert_eq!(parsed.as_deref(), Ok(*expected));
    }
    assert!(refused);
}

#[test]
fn platform_url_needs_https_except_on_this_computer() {
    // Arrange
    let accepted = [
        (
            "https://platform.agentty.xyz/",
            "https://platform.agentty.xyz",
        ),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("http://localhost/", "http://localhost"),
        ("http://[::1]:8080/base", "http://[::1]:8080/base"),
    ];
    let rejected = [
        "http://platform.example",
        "http://127.0.0.1.example",
        "http://http://localhost",
        "wss://platform.example",
        "https://user:secret@platform.example",
        "https://platform.example/?x=1",
        "platform.example",
    ];

    // Act
    let parsed: Vec<_> = accepted
        .iter()
        .map(|(value, _)| parse_platform_url(value))
        .collect();
    let refused: Vec<_> = rejected
        .iter()
        .map(|value| parse_platform_url(value))
        .collect();

    // Assert
    for ((_, expected), parsed) in accepted.iter().zip(parsed) {
        assert_eq!(parsed.as_deref(), Ok(*expected));
    }
    for refused in refused {
        assert_eq!(
            refused,
            Err(
                "expected a platform URL such as https://platform.agentty.xyz; plain http:// is \
                 accepted only for localhost"
                    .to_string()
            )
        );
    }
}

use std::ffi::{OsStr, OsString};

use super::{BrowserClient, RealBrowserClient};

#[test]
fn real_browser_client_uses_the_platform_url_handler() {
    // Arrange
    let expected_program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    // Act
    let browser = RealBrowserClient::default();
    let command = browser.command("https://example.com/sign-in?state=abc");

    // Assert
    let standard_command = command.as_std();
    assert_eq!(standard_command.get_program(), OsStr::new(expected_program));
    assert_eq!(
        standard_command.get_args().collect::<Vec<_>>(),
        vec![OsStr::new("https://example.com/sign-in?state=abc")]
    );
}

#[tokio::test]
async fn real_browser_client_ignores_launch_failures() {
    // Arrange
    let missing = RealBrowserClient {
        program: OsString::from("/nonexistent/agentty-browser"),
    };
    let succeeding = RealBrowserClient {
        program: OsString::from("true"),
    };

    // Act
    missing.open("https://example.com");
    succeeding.open("https://example.com");

    // Assert
    assert_eq!(
        missing.program,
        OsString::from("/nonexistent/agentty-browser")
    );
}

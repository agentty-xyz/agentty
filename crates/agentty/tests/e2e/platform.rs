//! Platform tab E2E tests against a fake platform server: device sign-in,
//! registration, remote control through the platform's relay, keep on, the
//! waitlist, and sign-out.

use ag_session::SettingName;
use serde_json::json;
use testty::assertion;
use testty::region::Region;
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::common;
use crate::common::FeatureTest;

/// Token the fake platform issues.
const TOKEN: &str = "agp_e2e-token";
/// Code the fake platform asks the user to confirm.
const USER_CODE: &str = "BCDF-GHJK";

/// Starts a fake platform that issues a sign-in code, answers the first
/// token poll as pending and the next with a token, and answers account
/// requests with what `account` builds from the platform's address.
async fn fake_platform(account: fn(&str) -> ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/v1/device/authorizations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "e2e-device-code",
            "user_code": USER_CODE,
            "verification_uri": format!("{uri}/device"),
            "verification_uri_complete": format!("{uri}/device?user_code={USER_CODE}"),
            "expires_in": 600,
            "interval": 1,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/device/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/device/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": TOKEN,
            "token_type": "bearer",
            "account": { "login": "octocat" },
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(account(&uri))
        .mount(&server)
        .await;

    server
}

/// Answers the account with the platform's relay, where nothing listens,
/// and its pairing page.
fn account_with_relay(uri: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "account": { "login": "octocat" },
        "services": {
            "remote_control": {
                "relay_url": "ws://127.0.0.1:9",
                "pairing_url": format!("{uri}/sessions/"),
            },
        },
    }))
}

/// Launch arguments offering only the Platform tab, signed in to `server`.
fn platform_args(server: &MockServer) -> [String; 2] {
    ["--platform-url".to_string(), server.uri()]
}

/// Starts a fake platform with a relay that registers this Agentty once,
/// as `Test machine`.
async fn kept_on_platform() -> MockServer {
    let server = fake_platform(account_with_relay).await;
    Mock::given(method("PUT"))
        .and(path_regex("^/api/v1/instances/[0-9a-f]{64}$"))
        .respond_with(|request: &Request| {
            let instance_id = request.url.path().rsplit('/').next().unwrap_or_default();

            ResponseTemplate::new(200)
                .set_body_json(json!({ "instance_id": instance_id, "name": "Test machine" }))
        })
        .expect(1)
        .mount(&server)
        .await;

    server
}

/// Builds the `name` scenario launched with a saved sign-in to `server`
/// that keeps remote control on, so it connects through the platform's
/// relay as soon as Agentty starts.
fn kept_on_test(name: &str, server: &MockServer) -> FeatureTest {
    let stored = json!({ "access_token": TOKEN, "keep_on": true, "platform_url": server.uri() });

    FeatureTest::new(name)
        .with_git()
        .args(platform_args(server))
        .setup(move |env| {
            Box::pin(async move {
                common::open_database(env)
                    .await?
                    .settings()
                    .upsert_setting(SettingName::PlatformSignIn, &stored.to_string())
                    .await?;

                Ok(())
            })
        })
}

/// Verify a launch offers the Platform tab, where
/// `Enter` signs in with a device code and the approved sign-in ends with
/// remote control running through the platform's relay, with a pairing
/// link on the platform's page; `Enter` stops it, and `l` signs out and
/// tells the platform.
#[tokio::test]
async fn platform_tab_sign_in() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(account_with_relay).await;
    let pairing_page = format!("{}/sessions/#relay=", server.uri());
    Mock::given(method("PUT"))
        .and(path_regex("^/api/v1/instances/[0-9a-f]{64}$"))
        .respond_with(|request: &Request| {
            let instance_id = request.url.path().rsplit('/').next().unwrap_or_default();

            ResponseTemplate::new(200)
                .set_body_json(json!({ "instance_id": instance_id, "name": "Test machine" }))
        })
        .expect(1..)
        .mount(&server)
        .await;
    for route in ["^/api/v1/instances/[0-9a-f]{64}$", "^/api/v1/token$"] {
        Mock::given(method("DELETE"))
            .and(path_regex(route))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }

    // Act, Assert
    FeatureTest::new("platform_tab_sign_in")
        .with_git()
        .with_terminal_size(160, 50)
        .args(platform_args(&server))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Account Signed out", 5000)
                    .capture_labeled("signed_out", "The Platform tab offers sign-in")
                    .press_key("Enter")
                    .wait_for_text(USER_CODE, 5000)
                    .capture_labeled("code", "Enter shows the code and the page")
                    .wait_for_text("Relay   Connecting", 10000)
                    .capture_labeled("connected", "The approved sign-in connects")
                    .press_key("Enter")
                    .wait_for_text("Remote  Off", 5000)
                    .capture_labeled("stopped", "Enter stops remote control")
                    .press_key("l")
                    .wait_for_text("Signed out.", 5000)
                    .capture_labeled("signed_out_again", "l signs out")
            },
            move |_frame, report| {
                Box::pin(async move {
                    let signed_out = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(signed_out.cols(), signed_out.rows());
                    assertion::assert_text_in_region(&signed_out, "Settings | Platform", &full);
                    assertion::assert_text_in_region(
                        &signed_out,
                        "connects out to the platform's relay",
                        &full,
                    );
                    assertion::assert_text_in_region(
                        &signed_out,
                        "Enter: sign in and connect",
                        &full,
                    );
                    assertion::assert_not_visible(&signed_out, "Remote ");

                    let code = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(&code, "/device?user_code=BCDF-GHJK", &full);
                    assertion::assert_text_in_region(&code, "o: open page", &full);
                    assertion::assert_text_in_region(&code, "c: copy link", &full);

                    let connected = common::frame_from_capture(&report.captures[2]);
                    assertion::assert_text_in_region(&connected, "Account octocat", &full);
                    assertion::assert_text_in_region(&connected, "Name    Test machine", &full);
                    assertion::assert_text_in_region(&connected, "Keep on Off", &full);
                    assertion::assert_text_in_region(&connected, "Remote  On", &full);
                    assertion::assert_text_in_region(&connected, &pairing_page, &full);
                    assertion::assert_text_in_region(&connected, "Paired devices", &full);

                    let stopped = common::frame_from_capture(&report.captures[3]);
                    assertion::assert_text_in_region(&stopped, "Enter: start", &full);
                    assertion::assert_text_in_region(&stopped, "l: sign out", &full);
                    assertion::assert_not_visible(&stopped, "Relay   Connecting");

                    let signed_out_again = common::frame_from_capture(&report.captures[4]);
                    assertion::assert_text_in_region(
                        &signed_out_again,
                        "Account Signed out",
                        &full,
                    );
                    assertion::assert_not_visible(&signed_out_again, "Relay   Connecting");
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify keep on, saved with the sign-in, connects remote control through
/// the platform's relay as soon as Agentty starts, and that `a` turns it
/// off without stopping remote control.
#[tokio::test]
async fn platform_tab_keep_on() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = kept_on_platform().await;

    // Act, Assert
    kept_on_test("platform_tab_keep_on", &server)
        .with_terminal_size(160, 50)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Relay   Connecting", 10000)
                    .capture_labeled("connected", "Keep on connects at launch")
                    .press_key("a")
                    .wait_for_text("Keep on Off", 5000)
                    .capture_labeled("kept_off", "a turns keep on off")
            },
            |_frame, report| {
                Box::pin(async move {
                    let connected = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(connected.cols(), connected.rows());
                    assertion::assert_text_in_region(&connected, "Keep on On", &full);
                    assertion::assert_text_in_region(
                        &connected,
                        "starts whenever Agentty starts",
                        &full,
                    );
                    assertion::assert_text_in_region(&connected, "a: keep on", &full);

                    let kept_off = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(&kept_off, "Remote  On", &full);
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Top edge of a QR finder pattern, which every pairing link's code
/// shows whatever its key.
const QR_FINDER_TOP: &str = "█▀▀▀▀▀█";

/// Verify a laptop-sized terminal shows the pairing link's QR code beside
/// the account and relay details, with the scan hint.
#[tokio::test]
async fn platform_tab_qr_code() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = kept_on_platform().await;

    // Act, Assert
    kept_on_test("platform_tab_qr_code", &server)
        .with_terminal_size(130, 38)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Relay   Connecting", 10000)
                    .capture_labeled("qr_code", "The QR code fits beside the details")
            },
            |_frame, report| {
                Box::pin(async move {
                    let frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(&frame, QR_FINDER_TOP, &full);
                    assertion::assert_text_in_region(&frame, "Scan the code", &full);
                    assertion::assert_text_in_region(&frame, "l: sign out", &full);
                    assertion::assert_not_visible(&frame, "Enlarge the terminal");
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify a terminal too short for the pairing link's QR code leaves it
/// out and says how to show it, while the link stays usable.
#[tokio::test]
async fn platform_tab_qr_code_too_short() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = kept_on_platform().await;

    // Act, Assert
    kept_on_test("platform_tab_qr_code_too_short", &server)
        .with_terminal_size(130, 26)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Relay   Connecting", 10000)
                    .capture_labeled("no_qr_code", "A short terminal leaves the code out")
            },
            |_frame, report| {
                Box::pin(async move {
                    let frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(&frame, "Open the link", &full);
                    assertion::assert_text_in_region(
                        &frame,
                        "Enlarge the terminal to show the link as a QR code.",
                        &full,
                    );
                    assertion::assert_not_visible(&frame, QR_FINDER_TOP);
                })
            },
        )
        .await?;
    server.verify().await;

    Ok(())
}

/// Verify a waitlisted account stays signed in without the platform's
/// relay, says so in plain words, and checks again on `Enter`, here once
/// the account is admitted to a platform that offers it no relay.
#[tokio::test]
async fn platform_tab_waitlist() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let server = fake_platform(|_| {
        ResponseTemplate::new(200).set_body_json(json!({ "account": { "login": "octocat" } }))
    })
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": "waitlisted",
            "message": "This account is on the waitlist.",
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // Act, Assert
    FeatureTest::new("platform_tab_waitlist")
        .with_git()
        .with_terminal_size(160, 40)
        .args(platform_args(&server))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .press_key("Enter")
                    .wait_for_text("waitlist", 10000)
                    .capture_labeled("waitlisted", "The account waits for access")
                    .press_key("Enter")
                    .wait_for_text("offers this account no relay", 5000)
                    .capture_labeled("checked_again", "Enter checks again")
            },
            |_frame, report| {
                Box::pin(async move {
                    let waitlisted = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(waitlisted.cols(), waitlisted.rows());
                    assertion::assert_text_in_region(&waitlisted, "Account octocat", &full);
                    assertion::assert_text_in_region(&waitlisted, "has no access yet", &full);
                    assertion::assert_text_in_region(
                        &waitlisted,
                        "Enter: check again | a: keep on | l: sign out",
                        &full,
                    );
                    assertion::assert_not_visible(&waitlisted, "Remote  ");

                    let checked_again = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(&checked_again, "Account octocat", &full);
                    assertion::assert_not_visible(&checked_again, "waitlist");
                })
            },
        )
        .await?;

    Ok(())
}

//! Remote-control E2E tests through a platform: a fake platform whose
//! account names an in-process relay, which Agentty really registers with,
//! the relay state and pairing link, and the tab label.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use ag_relay::{AdmissionError, AdmissionPolicy, RelayConfig, RelayHandle, RelayLimits};
use ag_session::SettingName;
use async_trait::async_trait;
use serde_json::json;
use testty::assertion;
use testty::region::Region;
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::common;
use crate::common::FeatureTest;

/// Token of the sign-in the scenarios start with.
const TOKEN: &str = "agp_e2e-token";

/// Admission policy that admits no instance.
struct AdmitNone;

#[async_trait]
impl AdmissionPolicy for AdmitNone {
    async fn admit(&self, _instance_id: &str) -> Result<bool, AdmissionError> {
        Ok(false)
    }
}

/// Settings of an in-process relay on a free loopback port.
fn relay_config() -> RelayConfig {
    RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: None,
        limits: RelayLimits::default(),
    }
}

/// Starts a fake platform whose account offers `relay` with the platform's
/// pairing page, and that registers this Agentty as `Test machine`.
async fn platform_with_relay(relay: &RelayHandle) -> MockServer {
    let server = MockServer::start().await;
    let relay_url = format!("ws://{}", relay.local_addr());
    let pairing_url = format!("{}/sessions/", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account": { "login": "octocat" },
            "services": {
                "remote_control": { "relay_url": relay_url, "pairing_url": pairing_url },
            },
        })))
        .mount(&server)
        .await;
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
/// that keeps remote control on, so it starts through the platform's relay
/// as soon as Agentty starts.
fn kept_on_test(name: &str, server: &MockServer) -> FeatureTest {
    let stored = json!({ "access_token": TOKEN, "keep_on": true, "platform_url": server.uri() });

    FeatureTest::new(name)
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--platform-url".to_string(), server.uri()])
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

/// Verify a platform whose account names a relay makes Agentty register
/// with that relay: the Platform tab shows it connected with a pairing link
/// on the platform's page that names the relay, the tab label counts the
/// connected devices, and `n` replaces the link.
#[tokio::test]
async fn remote_control_through_the_platform_relay() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let relay = ag_relay::start(relay_config()).await?;
    let server = platform_with_relay(&relay).await;
    let pairing_link = format!(
        "{}/sessions/#relay=ws%3A%2F%2F127%2E0%2E0%2E1%3A{}&key=",
        server.uri(),
        relay.local_addr().port()
    );

    // Act, Assert
    kept_on_test("remote_control_through_the_platform_relay", &server)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Relay   Connected", 10000)
                    .capture_labeled("connected", "Agentty registers with the platform's relay")
                    .press_key("n")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("new_code", "n replaces the pairing link")
            },
            move |_frame, report| {
                Box::pin(async move {
                    let connected = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(connected.cols(), connected.rows());
                    assertion::assert_text_in_region(
                        &connected,
                        "Settings | Platform (0 online)",
                        &full,
                    );
                    assertion::assert_text_in_region(&connected, "Remote  On", &full);
                    assertion::assert_text_in_region(&connected, "Relay   Connected", &full);
                    assertion::assert_text_in_region(&connected, &pairing_link, &full);
                    assertion::assert_text_in_region(&connected, "n: new code", &full);
                    assertion::assert_not_visible(&connected, "tunnel");

                    let new_code = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(&new_code, "Relay   Connected", &full);
                    assertion::assert_text_in_region(&new_code, &pairing_link, &full);
                })
            },
        )
        .await?;
    server.verify().await;
    relay.shutdown().await;

    Ok(())
}

/// Verify a platform relay that admits no instance shows as `Not admitted`,
/// with the reason, instead of connecting forever.
#[tokio::test]
async fn remote_control_relay_not_admitted() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let relay = ag_relay::start_with_admission(relay_config(), Arc::new(AdmitNone)).await?;
    let server = platform_with_relay(&relay).await;

    // Act, Assert
    kept_on_test("remote_control_relay_not_admitted", &server)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Platform"))
                    .wait_for_text("Relay   Not admitted", 10000)
                    .capture_labeled("not_admitted", "The relay refuses this Agentty")
            },
            |_frame, report| {
                Box::pin(async move {
                    let frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(&frame, "Relay   Not admitted", &full);
                    assertion::assert_text_in_region(
                        &frame,
                        "The relay refused this Agentty",
                        &full,
                    );
                })
            },
        )
        .await?;
    server.verify().await;
    relay.shutdown().await;

    Ok(())
}

//! Remote tab E2E tests: listener lifecycle, tab label, and relay state.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use ag_relay::{InstanceAllowList, RelayConfig, RelayLimits};
use ag_session::SettingName;
use testty::assertion;
use testty::region::Region;
use wiremock::MockServer;

use crate::common;
use crate::common::FeatureTest;

/// Verify `--experimental-remote` adds the Remote tab to the right of
/// Settings, where `Enter` starts a loopback listener with a pairing code,
/// only the tab label reports it while it runs, and `Enter` stops it again.
#[tokio::test]
async fn remote_control_tab() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("remote_control_tab")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--experimental-remote".to_string(),
            "--remote-control-port".to_string(),
            "0".to_string(),
        ])
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .compose(&common::switch_to_tab("Remote"))
                    .wait_for_text("Status  Off", 5000)
                    .capture_labeled("remote_tab_off", "Remote tab opens with the listener off")
                    .press_key("Enter")
                    .wait_for_text("Status  On", 5000)
                    .capture_labeled("remote_tab_on", "Enter starts the listener")
                    .press_key("Enter")
                    .wait_for_text("Status  Off", 5000)
                    .capture_labeled("remote_tab_stopped", "Enter stops the listener")
            },
            |_frame, report| {
                Box::pin(async move {
                    let off_frame = common::frame_from_capture(&report.captures[0]);
                    let off_full = Region::full(off_frame.cols(), off_frame.rows());
                    assertion::assert_text_in_region(&off_frame, "Settings | Remote", &off_full);
                    // The tab names no project: devices reach every listed
                    // one.
                    assertion::assert_text_in_region(&off_frame, "Remote control", &off_full);
                    assertion::assert_text_in_region(
                        &off_frame,
                        "steer the sessions of your projects",
                        &off_full,
                    );
                    assertion::assert_not_visible(&off_frame, "' remote control");
                    assertion::assert_text_in_region(&off_frame, "Enter: start", &off_full);
                    assertion::assert_text_in_region(&off_frame, "None yet", &off_full);
                    assertion::assert_not_visible(&off_frame, "online");
                    assertion::assert_not_visible(&off_frame, "Remote control on");

                    let on_frame = common::frame_from_capture(&report.captures[1]);
                    let on_full = Region::full(on_frame.cols(), on_frame.rows());
                    assertion::assert_text_in_region(&on_frame, "Remote (0 online)", &on_full);
                    assertion::assert_text_in_region(&on_frame, "http://127.0.0.1:", &on_full);
                    assertion::assert_text_in_region(&on_frame, "Code", &on_full);
                    assertion::assert_text_in_region(
                        &on_frame,
                        "n: new code | Enter: stop",
                        &on_full,
                    );
                    assertion::assert_not_visible(&on_frame, "Remote control on");

                    let stopped_frame = common::frame_from_capture(&report.captures[2]);
                    let stopped_full = Region::full(stopped_frame.cols(), stopped_frame.rows());
                    assertion::assert_text_in_region(&stopped_frame, "Enter: start", &stopped_full);
                    assertion::assert_not_visible(&stopped_frame, "http://127.0.0.1:");
                    assertion::assert_not_visible(&stopped_frame, "online");
                    assertion::assert_not_visible(&stopped_frame, "Remote control on");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify `--remote-relay` alone adds the Remote tab with the relay state,
/// a one-time pairing link, and the link actions. Nothing listens on the
/// relay address, so the link stays connecting.
#[tokio::test]
async fn remote_control_tab_relay() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange, Act, Assert
    FeatureTest::new("remote_control_tab_relay")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--remote-control-port".to_string(),
            "0".to_string(),
            "--remote-relay".to_string(),
            "ws://127.0.0.1:9".to_string(),
        ])
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Remote"))
                    .press_key("Enter")
                    .wait_for_text("Relay   Connecting", 5000)
                    .capture_labeled("relay_link", "Remote tab shows the relay link")
                    .press_key("Enter")
                    .wait_for_text("Status  Off", 5000)
                    .capture_labeled("relay_off", "Stopping hides the relay link")
            },
            |_frame, report| {
                Box::pin(async move {
                    let link_frame = common::frame_from_capture(&report.captures[0]);
                    let link_full = Region::full(link_frame.cols(), link_frame.rows());
                    assertion::assert_text_in_region(&link_frame, "Settings | Remote", &link_full);
                    assertion::assert_text_in_region(&link_frame, "Relay   Connecting", &link_full);
                    assertion::assert_text_in_region(
                        &link_frame,
                        "http://127.0.0.1:9/#relay=ws%3A%2F%2F127%2E0%2E0%2E1%3A9",
                        &link_full,
                    );
                    assertion::assert_text_in_region(&link_frame, "o: open link", &link_full);
                    assertion::assert_text_in_region(&link_frame, "c: copy link", &link_full);

                    let off_frame = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_not_visible(&off_frame, "Relay   Connecting");
                    assertion::assert_not_visible(&off_frame, "#relay=");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify `--remote-relay` keeps the platform out of the launch even with
/// `--platform-url` and a saved sign-in: no Platform tab and no request to
/// the platform.
#[tokio::test]
async fn remote_control_tab_relay_skips_the_platform() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange: a platform that answers nothing it is asked.
    let platform = MockServer::start().await;
    let stored = serde_json::json!({
        "access_token": "agp_e2e-token",
        "keep_on": true,
        "platform_url": platform.uri(),
    });

    // Act, Assert
    FeatureTest::new("remote_control_tab_relay_skips_the_platform")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--platform-url".to_string(),
            platform.uri(),
            "--remote-relay".to_string(),
            "ws://127.0.0.1:9".to_string(),
        ])
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
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Remote"))
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("remote_only", "Only the Remote tab is offered")
            },
            |_frame, report| {
                Box::pin(async move {
                    let frame = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(&frame, "Settings | Remote", &full);
                    assertion::assert_text_in_region(&frame, "Status  Off", &full);
                    assertion::assert_not_visible(&frame, "Platform");
                })
            },
        )
        .await?;
    let requests = platform.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "the platform was contacted");

    Ok(())
}

/// Verify a relay that admits no instance shows as `Not admitted`, with
/// the reason, instead of connecting forever.
#[tokio::test]
async fn remote_control_tab_relay_not_admitted() -> Result<(), Box<dyn std::error::Error>> {
    // Arrange
    let relay = ag_relay::start_with_admission(
        RelayConfig {
            bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            client_address_header: None,
            limits: RelayLimits::default(),
        },
        Arc::new(InstanceAllowList::new(Vec::new())),
    )
    .await?;

    // Act, Assert
    FeatureTest::new("remote_control_tab_relay_not_admitted")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--experimental-remote".to_string(),
            "--remote-control-port".to_string(),
            "0".to_string(),
            "--remote-relay".to_string(),
            format!("ws://{}", relay.local_addr()),
        ])
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab_reverse("Remote"))
                    .press_key("Enter")
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
    relay.shutdown().await;

    Ok(())
}

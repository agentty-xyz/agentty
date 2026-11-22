//! `/remote-control` popup, listener lifecycle, and status-bar notice.

use testty::assertion;
use testty::region::Region;

use super::fixture::{E2eResult, seed_review_ready_session};
use crate::common;
use crate::common::FeatureTest;

/// Verify `--experimental-remote` offers `/remote-control`, which starts a
/// loopback listener with a pairing code, shows the status-bar notice while
/// it runs, and stops from the popup.
#[tokio::test]
async fn session_remote_control() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_remote_control")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--experimental-remote".to_string(),
            "--remote-control-port".to_string(),
            "0".to_string(),
        ])
        .setup(|env| Box::pin(async move { seed_review_ready_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .write_text("remote")
                    .wait_for_text("Control Agentty sessions from another device", 3000)
                    .press_key("Enter")
                    .wait_for_text("Status  On", 5000)
                    .capture_labeled("remote_control_on", "Popup shows the running listener")
                    .press_key("s")
                    .wait_for_text("Status  Off", 5000)
                    .capture_labeled("remote_control_off", "Stopping turns the listener off")
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("session_view", "Closing returns to the session")
            },
            |_frame, report| {
                Box::pin(async move {
                    let on_frame = common::frame_from_capture(&report.captures[0]);
                    let on_full = Region::full(on_frame.cols(), on_frame.rows());
                    assertion::assert_text_in_region(&on_frame, "Remote Control", &on_full);
                    assertion::assert_text_in_region(&on_frame, "http://127.0.0.1:", &on_full);
                    assertion::assert_text_in_region(&on_frame, "Code", &on_full);
                    assertion::assert_text_in_region(&on_frame, "None yet", &on_full);
                    assertion::assert_text_in_region(
                        &on_frame,
                        "Remote control on, 0 devices connected",
                        &on_full,
                    );

                    let off_frame = common::frame_from_capture(&report.captures[1]);
                    let off_full = Region::full(off_frame.cols(), off_frame.rows());
                    assertion::assert_text_in_region(&off_frame, "s: start", &off_full);
                    assertion::assert_not_visible(&off_frame, "http://127.0.0.1:");
                    assertion::assert_not_visible(&off_frame, "Remote control on");

                    let view_frame = common::frame_from_capture(&report.captures[2]);
                    assertion::assert_not_visible(&view_frame, "Remote Control");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify `--remote-relay` adds the relay state, a one-time pairing link,
/// and the link actions to the `/remote-control` popup. Nothing listens on
/// the relay address, so the link stays connecting.
#[tokio::test]
async fn session_remote_control_relay() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_remote_control_relay")
        .with_git()
        .with_terminal_size(140, 36)
        .args([
            "--experimental-remote".to_string(),
            "--remote-control-port".to_string(),
            "0".to_string(),
            "--remote-relay".to_string(),
            "ws://127.0.0.1:9".to_string(),
        ])
        .setup(|env| Box::pin(async move { seed_review_ready_session(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("/")
                    .write_text("remote")
                    .wait_for_text("Control Agentty sessions from another device", 3000)
                    .press_key("Enter")
                    .wait_for_text("Relay   Connecting", 5000)
                    .capture_labeled("relay_link", "Popup shows the relay link")
                    .press_key("s")
                    .wait_for_text("Status  Off", 5000)
                    .capture_labeled("relay_off", "Stopping hides the relay link")
            },
            |_frame, report| {
                Box::pin(async move {
                    let link_frame = common::frame_from_capture(&report.captures[0]);
                    let link_full = Region::full(link_frame.cols(), link_frame.rows());
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

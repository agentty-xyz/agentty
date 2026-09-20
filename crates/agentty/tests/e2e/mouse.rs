//! Mouse support E2E tests: wheel scrolling over the session transcript and
//! the `Mouse Support` settings switch.
//!
//! Wheel input is injected as raw SGR mouse sequences (`ESC [ < 64 ; col ; row
//! M` for wheel-up, `65` for wheel-down) written straight to the PTY, the same
//! way the CSI-u and bracketed-paste tests inject their escape sequences.

use agentty::domain::session_message::SessionMessageKind;
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest, SessionSeed};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Stable id for the seeded session with a transcript taller than the view.
const SCROLL_SESSION_ID: &str = "mouse-scroll-0001";

/// Number of transcript paragraphs seeded so the output overflows the panel.
const TRANSCRIPT_PARAGRAPH_COUNT: usize = 40;

/// One SGR wheel-up notch over the middle of the transcript panel.
const WHEEL_UP_OVER_TRANSCRIPT: &str = "\x1b[<64;40;10M";

/// One SGR wheel-down notch over the middle of the transcript panel.
const WHEEL_DOWN_OVER_TRANSCRIPT: &str = "\x1b[<65;40;10M";

/// Seeds one review-ready session whose transcript is taller than the view.
///
/// Paragraph labels avoid spaces because testty's text search skips blank
/// cells the terminal never repainted.
async fn seed_session_with_long_transcript(env: &BuilderEnv) -> E2eResult {
    common::seed_session(
        env,
        SessionSeed::regular(SCROLL_SESSION_ID, "claude-opus-5", "main", "Review")
            .with_title("Mouse wheel scrolling"),
    )
    .await?;

    let transcript = (1..=TRANSCRIPT_PARAGRAPH_COUNT)
        .map(|index| format!("transcript_line_{index:02}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    (async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .append_session_message(
                SCROLL_SESSION_ID,
                SessionMessageKind::AssistantAnswer,
                &transcript,
            )
            .await
    })
    .await?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join(&SCROLL_SESSION_ID[..8]))?;

    Ok(())
}

/// Verify that the mouse wheel scrolls the session transcript and that
/// scrolling back down resumes following the newest output.
#[tokio::test]
async fn mouse_wheel_scrolls_session_output() {
    // Arrange, Act, Assert
    FeatureTest::new("mouse_wheel_scroll")
        .setup(|env| Box::pin(async move { seed_session_with_long_transcript(env).await }))
        .zola(
            "Mouse wheel scrolling",
            "Scroll the session transcript with the mouse wheel and drag its scrollbar.",
            127,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("transcript_line_40", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("tail", "Session view following the newest output")
                    .write_text(WHEEL_UP_OVER_TRANSCRIPT)
                    .write_text(WHEEL_UP_OVER_TRANSCRIPT)
                    .write_text(WHEEL_UP_OVER_TRANSCRIPT)
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("scrolled_up", "Transcript scrolled up with the wheel")
                    .write_text(WHEEL_DOWN_OVER_TRANSCRIPT)
                    .write_text(WHEEL_DOWN_OVER_TRANSCRIPT)
                    .write_text(WHEEL_DOWN_OVER_TRANSCRIPT)
                    .write_text(WHEEL_DOWN_OVER_TRANSCRIPT)
                    .wait_for_text("transcript_line_40", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("back_at_tail", "Wheel-down returned the view to the tail")
            },
            |frame, report| {
                Box::pin(async move {
                    assert_eq!(
                        report.captures.len(),
                        3,
                        "Expected 3 captures (tail, scrolled_up, back_at_tail)"
                    );

                    let tail_frame = common::frame_from_capture(&report.captures[0]);
                    let tail_full = Region::full(tail_frame.cols(), tail_frame.rows());
                    assertion::assert_text_in_region(&tail_frame, "transcript_line_40", &tail_full);
                    assertion::assert_text_in_region(&tail_frame, "█", &tail_full);

                    let scrolled_frame = common::frame_from_capture(&report.captures[1]);
                    let scrolled_full = Region::full(scrolled_frame.cols(), scrolled_frame.rows());
                    assertion::assert_not_visible(&scrolled_frame, "transcript_line_40");
                    assertion::assert_text_in_region(
                        &scrolled_frame,
                        "transcript_line_3",
                        &scrolled_full,
                    );

                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "transcript_line_40", &full);
                })
            },
        )
        .await
        .expect("feature test failed");
}

/// Verify that the `Mouse Support` switch lives in the global settings section
/// and can be turned off from its dropdown.
#[tokio::test]
async fn settings_mouse_support_switch() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_mouse_support")
        .zola(
            "Mouse support switch",
            "Turn terminal mouse capture off from the global settings.",
            157,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .wait_for_text("Mouse Support", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("enabled", "Mouse Support enabled by default")
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_text("Select setting value", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("dropdown", "Mouse Support dropdown")
                    .press_key("k")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("disabled", "Mouse Support turned off")
            },
            |frame, report| {
                Box::pin(async move {
                    assert_eq!(
                        report.captures.len(),
                        3,
                        "Expected 3 captures (enabled, dropdown, disabled)"
                    );

                    let enabled_frame = common::frame_from_capture(&report.captures[0]);
                    let enabled_full = Region::full(enabled_frame.cols(), enabled_frame.rows());
                    assertion::assert_text_in_region(
                        &enabled_frame,
                        "Global settings",
                        &enabled_full,
                    );
                    assertion::assert_text_in_region(
                        &enabled_frame,
                        "Mouse Support",
                        &enabled_full,
                    );
                    assertion::assert_match_count(&enabled_frame, "Enabled", 2);

                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Mouse Support", &full);
                    assertion::assert_match_count(frame, "Enabled", 1);
                    assertion::assert_match_count(frame, "Disabled", 2);
                })
            },
        )
        .await
        .expect("feature test failed");
}

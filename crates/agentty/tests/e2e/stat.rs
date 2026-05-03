//! Stats page E2E tests: provider usage and token table rendering.

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

/// Verify that the Stats tab renders the token stats table and provider
/// subscription usage panel.
///
/// Navigates to the Stats tab and asserts that both the provider usage and
/// token stats table titles are visible in the rendered frame.
#[test]
fn stats_tab_shows_usage_and_tokens() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_content")
        .with_stub_path_only()
        .with_terminal_size(160, 24)
        .zola(
            "Stats tab",
            "View provider usage and per-session token usage statistics.",
            160,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Review"))
                    .compose(&common::switch_to_tab("Stats"))
                    .wait_for_text("Claude", 3000)
                    .wait_for_stable_frame(500, 5000)
                    .viewing_pause_ms(3000)
                    .capture_labeled(
                        "stats_tab",
                        "Stats tab with subscription usage and token table",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Subscription Usage", &full);
                assertion::assert_text_in_region(frame, "Token Stats", &full);
            },
        )
        .expect("feature test failed");
}

/// Verify that the Stats tab shows the summary line with session
/// and token counts.
#[test]
fn stats_footer_shows_summary() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_footer")
        .with_stub_path_only()
        .with_terminal_size(160, 24)
        .zola(
            "Stats footer",
            "Stats footer shows aggregate session and token counts.",
            162,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Review"))
                    .compose(&common::switch_to_tab("Stats"))
                    .wait_for_text("Claude", 3000)
                    .wait_for_stable_frame(500, 5000)
                    .viewing_pause_ms(3000)
                    .capture_labeled("stats_footer", "Stats tab footer with counts")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(
                    frame,
                    "Sessions: 0 | Input: 0 | Output: 0",
                    &full,
                );
            },
        )
        .expect("feature test failed");
}

/// Verify that the Stats tab help overlay shows stats-specific keybindings.
#[test]
fn stats_help_shows_keybindings() {
    // Arrange, Act, Assert
    FeatureTest::new("stats_help")
        .with_stub_path_only()
        .with_terminal_size(160, 24)
        .zola(
            "Stats help",
            "Press ? on the Stats tab to see stats-specific keybindings.",
            164,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Review"))
                    .compose(&common::switch_to_tab("Stats"))
                    .wait_for_text("Claude", 3000)
                    .wait_for_stable_frame(500, 5000)
                    .viewing_pause_ms(2000)
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(3000)
                    .capture_labeled("stats_help", "Help overlay on Stats tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Keybindings", &full);
                assertion::assert_text_in_region(frame, "quit", &full);
            },
        )
        .expect("feature test failed");
}

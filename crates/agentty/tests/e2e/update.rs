//! Automatic Agentty update E2E tests.

use std::os::unix::fs::PermissionsExt;

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Mirrors the private app-task E2E interval override.
const VERSION_CHECK_INTERVAL_MS_ENV_VAR: &str = "AGENTTY_TEST_VERSION_CHECK_INTERVAL_MS";

/// Installs an npm stub whose first Agentty lookup returns the running
/// version and whose second lookup exposes a newer release.
fn install_periodic_agentty_update_stub(env: &BuilderEnv) -> E2eResult {
    let npm_path = env.stub_bin.join("npm");
    let npm_script = format!(
        r#"#!/bin/sh
lookup_count="$AGENTTY_ROOT/periodic-version-lookups"
if [ "$1" = "view" ] && [ "$2" = "agentty" ]; then
  if [ -f "$lookup_count" ]; then
    printf '"999.0.0"'
  else
    : > "$lookup_count"
    printf '"{}"'
  fi
  exit 0
fi
if [ "$1" = "i" ] && [ "$2" = "-g" ] && [ "$3" = "agentty@latest" ]; then
  sleep 4
  exit 0
fi
exit 1
"#,
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(&npm_path, npm_script)?;
    // The PTY child retains the test process's UID, so owner execution is
    // sufficient.
    std::fs::set_permissions(&npm_path, std::fs::Permissions::from_mode(0o700))?;

    Ok(())
}

/// Starts on the deterministic Sessions tab and installs the periodic npm
/// fixture before the application launches.
fn setup_periodic_agentty_update(env: &BuilderEnv) -> E2eResult {
    common::seed_active_project_setting(env)?;
    install_periodic_agentty_update_stub(env)
}

/// Verify that a newer version discovered on a periodic tick visibly updates
/// Agentty after the startup lookup reported no upgrade.
#[test]
fn test_periodic_auto_update() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("periodic_auto_update")
        .with_git()
        .setup(setup_periodic_agentty_update)
        .env(VERSION_CHECK_INTERVAL_MS_ENV_VAR, "6000")
        .zola(
            "Periodic automatic updates",
            "See Agentty install newly published versions while it remains open.",
            15,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Updating to v999.0.0...", 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "updating",
                        "Periodic check installing the newly published version",
                    )
                    .wait_for_text("Updated to v999.0.0", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("updated", "Periodic update complete with restart guidance")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Updated to v999.0.0", &full);
                assertion::assert_text_in_region(frame, "restart to use new version", &full);

                let updating_capture = report
                    .captures
                    .iter()
                    .find(|capture| capture.label == "updating")
                    .expect("missing periodic update capture");
                let updating_frame = common::frame_from_capture(updating_capture);
                let updating_region = Region::full(updating_frame.cols(), updating_frame.rows());
                assertion::assert_text_in_region(
                    &updating_frame,
                    "Updating to v999.0.0...",
                    &updating_region,
                );
            },
        )?;

    Ok(())
}

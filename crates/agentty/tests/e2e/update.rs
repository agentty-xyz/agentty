//! Automatic Agentty update E2E tests.

use std::os::unix::fs::{PermissionsExt, symlink};

use assert_cmd::cargo::cargo_bin;
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Mirrors the private app-task E2E interval override.
const VERSION_CHECK_INTERVAL_MS_ENV_VAR: &str = "AGENTTY_TEST_VERSION_CHECK_INTERVAL_MS";

/// Mirrors the private shell-installer URL the updater downloads.
const SHELL_INSTALLER_URL: &str =
    "https://github.com/agentty-xyz/agentty/releases/latest/download/agentty-installer.sh";

/// Installs an npm stub whose first Agentty lookup per app process returns the
/// running version and whose second lookup exposes a newer release.
fn install_periodic_agentty_update_stub(env: &BuilderEnv) -> E2eResult {
    let npm_path = env.stub_bin.join("npm");
    let npm_script = format!(
        r#"#!/bin/sh
# Keep the semantic run and VHS replay on independent lookup sequences.
lookup_count="$AGENTTY_ROOT/periodic-version-lookups-$PPID"
if [ "$1" = "view" ] && [ "$2" = "agentty" ]; then
  if [ -f "$lookup_count" ]; then
    printf '"999.0.0"'
  else
    : > "$lookup_count"
    printf '"{}"'
  fi
  exit 0
fi
if [ "$1" = "root" ] && [ "$2" = "--global" ]; then
  printf '%s' "$AGENTTY_ROOT/global-node-modules"
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

    for manager in ["bun", "pnpm"] {
        let path = env.stub_bin.join(manager);
        std::fs::write(&path, "#!/bin/sh\nexit 1\n")?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }

    Ok(())
}

/// Starts on the deterministic Sessions tab and installs the periodic npm
/// fixture before the application launches.
async fn setup_periodic_agentty_update(env: &BuilderEnv) -> E2eResult {
    common::seed_active_project_setting(env).await?;
    install_periodic_agentty_update_stub(env)?;
    let binary_directory = env
        .agentty_root
        .join("global-node-modules/agentty/node_modules/.bin_real");
    std::fs::create_dir_all(&binary_directory)?;
    symlink(cargo_bin("agentty"), binary_directory.join("agentty"))?;

    Ok(())
}

/// Verify that a newer version discovered on a periodic tick visibly updates
/// Agentty after the startup lookup reported no upgrade.
#[tokio::test]
async fn test_periodic_auto_update() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("periodic_auto_update")
        .with_git()
        .setup(|env| Box::pin(async move { setup_periodic_agentty_update(env).await }))
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
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Updated to v999.0.0", &full);
                    assertion::assert_text_in_region(frame, "restart to use new version", &full);

                    let updating_capture = report
                        .captures
                        .iter()
                        .find(|capture| capture.label == "updating")
                        .expect("missing periodic update capture");
                    let updating_frame = common::frame_from_capture(updating_capture);
                    let updating_region =
                        Region::full(updating_frame.cols(), updating_frame.rows());
                    assertion::assert_text_in_region(
                        &updating_frame,
                        "Updating to v999.0.0...",
                        &updating_region,
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Installs the receipt and binary link of a shell install below the isolated
/// home, plus a `curl` stub that serves an installer which succeeds only for
/// the receipt's prefix with `PATH` changes disabled.
///
/// The npm stub from [`install_periodic_agentty_update_stub`] still reports
/// versions, but no package manager installed the running binary.
async fn setup_shell_installation_update(env: &BuilderEnv) -> E2eResult {
    common::seed_active_project_setting(env).await?;
    install_periodic_agentty_update_stub(env)?;
    let prefix = env.home_dir.join(".local");
    std::fs::create_dir_all(prefix.join("bin"))?;
    symlink(cargo_bin("agentty"), prefix.join("bin/agentty"))?;
    let receipt_dir = env.config_dir().join("agentty");
    std::fs::create_dir_all(&receipt_dir)?;
    let receipt = serde_json::json!({
        "binaries": ["agentty"],
        "install_layout": "cargo-home",
        "install_prefix": prefix,
        "modify_path": true,
        "provider": { "source": "ag-release", "version": "0.1.0" },
        "source": { "app_name": "agentty", "name": "agentty", "owner": "agentty-xyz" },
        "version": env!("CARGO_PKG_VERSION"),
    });
    std::fs::write(
        receipt_dir.join("agentty-receipt.json"),
        receipt.to_string(),
    )?;

    let installer_path = env.stub_bin.join("agentty-installer.sh");
    let installer = format!(
        "[ \"$AGENTTY_INSTALL_DIR\" = '{}' ] && [ \"$AGENTTY_NO_MODIFY_PATH\" = 1 ]\n",
        prefix.display()
    );
    std::fs::write(&installer_path, installer)?;
    let curl_path = env.stub_bin.join("curl");
    let curl_script = format!(
        r#"#!/bin/sh
for argument in "$@"; do
  if [ "$previous" = --output ]; then output=$argument; fi
  previous=$argument
done
[ "$previous" = {SHELL_INSTALLER_URL} ] || exit 1
cp '{}' "$output"
"#,
        installer_path.display()
    );
    std::fs::write(&curl_path, curl_script)?;
    std::fs::set_permissions(&curl_path, std::fs::Permissions::from_mode(0o700))?;

    Ok(())
}

/// Verify that a shell install updates in the background by rerunning the
/// downloaded installer for the receipt's prefix.
#[tokio::test]
async fn test_shell_installation_auto_update() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("shell_installation_auto_update")
        .with_git()
        .setup(|env| Box::pin(async move { setup_shell_installation_update(env).await }))
        .env(VERSION_CHECK_INTERVAL_MS_ENV_VAR, "1000")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Updated to v999.0.0", 15000)
                    .capture_labeled("updated", "Shell installer update complete")
            },
            |frame, _| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Updated to v999.0.0", &full);
                    assertion::assert_text_in_region(frame, "restart to use new version", &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Unknown installation provenance displays manual guidance in the terminal.
#[tokio::test]
async fn test_unknown_installation_manual_update() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("unknown_installation_manual_update")
        .with_git()
        .setup(|env| {
            Box::pin(async move {
                common::seed_active_project_setting(env).await?;
                install_periodic_agentty_update_stub(env)
            })
        })
        .env(VERSION_CHECK_INTERVAL_MS_ENV_VAR, "1000")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("rerun your original install command", 15000)
                    .capture_labeled("manual-update", "Original installer required")
            },
            |frame, _| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "v999.0.0 available", &full);
                    assertion::assert_text_in_region(
                        frame,
                        "rerun your original install command",
                        &full,
                    );
                })
            },
        )
        .await?;

    Ok(())
}

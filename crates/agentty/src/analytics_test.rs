use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use tempfile::Builder;
use uuid::Uuid;

use super::{Analytics, InstallMethod};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn posthog_sender_uses_bundled_destination_and_stored_installation_id() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");

    // Act
    let analytics = Analytics::posthog(&repositories)
        .await
        .expect("bundled destination");

    // Assert
    assert_eq!(analytics.endpoint, "https://us.i.posthog.com/i/v0/e/");
    assert!(analytics.token.starts_with("phc_"));
    assert_eq!(
        repositories
            .settings()
            .get_setting(SettingName::TelemetryInstallationId)
            .await
            .expect("stored installation ID"),
        Some(analytics.distinct_id)
    );
}

#[tokio::test]
async fn installation_id_is_created_once_and_reused() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");

    // Act
    let first_id = Analytics::installation_id(&repositories).await;
    let second_id = Analytics::installation_id(&repositories).await;

    // Assert
    assert!(Uuid::parse_str(&first_id).is_ok());
    assert_eq!(first_id, second_id);
}

#[tokio::test]
async fn empty_installation_id_is_replaced() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("repositories");
    repositories
        .settings()
        .upsert_setting(SettingName::TelemetryInstallationId, "")
        .await
        .expect("empty installation ID");

    // Act
    let installation_id = Analytics::installation_id(&repositories).await;

    // Assert
    assert!(Uuid::parse_str(&installation_id).is_ok());
    assert_eq!(
        repositories
            .settings()
            .get_setting(SettingName::TelemetryInstallationId)
            .await
            .expect("stored installation ID"),
        Some(installation_id)
    );
}

#[test]
fn client_builder_failure_disables_analytics() {
    // Arrange
    let client = None;

    // Act
    let analytics = Analytics::with_client("token", "https://example.com", "installation", client);

    // Assert
    assert!(analytics.is_none());
}

#[test]
fn receipt_path_prefers_xdg_config_home_and_falls_back_to_home() {
    // Arrange
    let xdg_config_home = PathBuf::from("xdg-config");
    let home = PathBuf::from("home");

    // Act
    let xdg_receipt = InstallMethod::receipt_path(Some(xdg_config_home), Some(home.clone()));
    let home_receipt = InstallMethod::receipt_path(None, Some(home));
    let missing_receipt = InstallMethod::receipt_path(None, None);

    // Assert
    assert_eq!(
        xdg_receipt,
        Some(PathBuf::from("xdg-config/agentty/agentty-receipt.json"))
    );
    assert_eq!(
        home_receipt,
        Some(PathBuf::from("home/.config/agentty/agentty-receipt.json"))
    );
    assert_eq!(missing_receipt, None);
}

#[test]
fn npm_install_is_detected_from_generated_package_layout() {
    // Arrange
    let executable =
        std::path::Path::new("/opt/lib/node_modules/agentty/node_modules/.bin_real/agentty");

    // Act
    let method = InstallMethod::from_paths(executable, None);

    // Assert
    assert_eq!(method, InstallMethod::Npm);
    assert_eq!(method.as_str(), "npm");
}

#[test]
fn shell_install_is_detected_from_matching_dist_receipt() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let receipt = root.path().join("agentty-receipt.json");
    let executable = root.path().join("bin/agentty");
    fs::create_dir(root.path().join("bin")).expect("bin directory");
    fs::write(&executable, b"binary").expect("installed binary");
    let data = serde_json::json!({
        "provider": { "source": "cargo-dist" },
        "source": { "app_name": "agentty" },
        "version": env!("CARGO_PKG_VERSION"),
        "install_prefix": root.path(),
        "install_layout": "cargo-home",
    });
    fs::write(&receipt, data.to_string()).expect("shell receipt");

    // Act
    let method = InstallMethod::from_paths(&executable, Some(&receipt));

    // Assert
    assert_eq!(method, InstallMethod::Sh);
    assert_eq!(method.as_str(), "sh");
}

#[cfg(unix)]
#[test]
fn shell_install_is_detected_through_symlinked_prefix() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let install_root = root.path().join("actual");
    let executable = install_root.join("bin/agentty");
    fs::create_dir_all(executable.parent().expect("bin parent")).expect("bin directory");
    fs::write(&executable, b"binary").expect("installed binary");
    let linked_root = root.path().join("linked");
    symlink(&install_root, &linked_root).expect("linked install prefix");
    let receipt = root.path().join("agentty-receipt.json");
    let data = serde_json::json!({
        "provider": { "source": "cargo-dist" },
        "source": { "app_name": "agentty" },
        "version": env!("CARGO_PKG_VERSION"),
        "install_prefix": linked_root,
        "install_layout": "cargo-home",
    });
    fs::write(&receipt, data.to_string()).expect("shell receipt");

    // Act
    let method = InstallMethod::from_paths(&executable, Some(&receipt));

    // Assert
    assert_eq!(method, InstallMethod::Sh);
}

#[test]
fn cargo_install_is_detected_from_cargo_metadata() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    let package = format!(
        "agentty {} (registry+https://github.com/rust-lang/crates.io-index)",
        env!("CARGO_PKG_VERSION")
    );
    let data = serde_json::json!({ "installs": { (package): { "bins": ["agentty"] } } });
    fs::write(root.path().join(".crates2.json"), data.to_string()).expect("Cargo metadata");

    // Act
    let method = InstallMethod::from_paths(&bin.join("agentty"), None);

    // Assert
    assert_eq!(method, InstallMethod::Cargo);
    assert_eq!(method.as_str(), "cargo");
}

#[test]
fn unverified_install_method_is_unknown() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let executable = root.path().join("bin/agentty");
    let receipt = root.path().join("receipt.json");

    // Act / Assert
    assert_eq!(
        InstallMethod::from_paths(&executable, None),
        InstallMethod::Unknown
    );
    fs::write(&receipt, b"not json").expect("invalid receipt");
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
    assert_eq!(InstallMethod::Unknown.as_str(), "unknown");
}

#[test]
fn shell_receipt_must_identify_this_binary_and_version() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let receipt = root.path().join("receipt.json");
    let executable = root.path().join("bin/agentty");
    fs::create_dir(root.path().join("bin")).expect("bin directory");
    fs::write(&executable, b"binary").expect("installed binary");
    fs::write(root.path().join("agentty"), b"binary").expect("flat binary");
    let mut data = serde_json::json!({
        "provider": { "source": "cargo-dist" },
        "source": { "app_name": "agentty" },
        "version": env!("CARGO_PKG_VERSION"),
        "install_prefix": root.path(),
        "install_layout": "cargo-home",
    });

    // Act / Assert
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
    data["version"] = serde_json::json!("old-version");
    fs::write(&receipt, data.to_string()).expect("outdated receipt");
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
    data["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    data["install_prefix"] = serde_json::Value::Null;
    fs::write(&receipt, data.to_string()).expect("receipt without prefix");
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
    data["install_prefix"] = serde_json::json!(root.path());
    data["install_layout"] = serde_json::json!("other");
    fs::write(&receipt, data.to_string()).expect("receipt with unsupported layout");
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
    data["install_layout"] = serde_json::json!("flat");
    fs::write(&receipt, data.to_string()).expect("flat install receipt");
    assert_eq!(
        InstallMethod::from_paths(&root.path().join("agentty"), Some(&receipt)),
        InstallMethod::Sh
    );
    assert_eq!(
        InstallMethod::from_paths(&executable, Some(&receipt)),
        InstallMethod::Unknown
    );
}

#[test]
fn cargo_metadata_must_identify_this_binary_and_version() {
    // Arrange
    let root = Builder::new()
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .expect("temporary root");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    let metadata = root.path().join(".crates2.json");
    let executable = bin.join("agentty");

    // Act / Assert
    assert_eq!(
        InstallMethod::from_paths(&bin.join("other"), None),
        InstallMethod::Unknown
    );
    assert_eq!(
        InstallMethod::from_paths(&root.path().join("agentty"), None),
        InstallMethod::Unknown
    );
    fs::write(&metadata, b"not json").expect("invalid metadata");
    assert_eq!(
        InstallMethod::from_paths(&executable, None),
        InstallMethod::Unknown
    );
    fs::write(&metadata, b"{}").expect("empty metadata");
    assert_eq!(
        InstallMethod::from_paths(&executable, None),
        InstallMethod::Unknown
    );
    fs::write(
        &metadata,
        r#"{"installs":{"agentty 0.0.0 (registry)":{"bins":["agentty"]}}}"#,
    )
    .expect("outdated metadata");
    assert_eq!(
        InstallMethod::from_paths(&executable, None),
        InstallMethod::Unknown
    );
}

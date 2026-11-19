//! Shell-installer receipt location and matching tests.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::ShellInstallReceipt;

/// Returns a receipt the installer writes for this build at `/prefix`.
fn receipt() -> Value {
    json!({
        "binaries": ["agentty"],
        "install_layout": "cargo-home",
        "install_prefix": "/prefix",
        "modify_path": false,
        "provider": { "source": "ag-release", "version": "0.1.0" },
        "source": { "app_name": "agentty", "name": "agentty", "owner": "agentty-xyz" },
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Resolves the installed binary and a link to it to the same file.
fn canonicalize(path: &Path) -> Option<PathBuf> {
    [Path::new("/prefix/bin/agentty"), Path::new("/link/agentty")]
        .contains(&path)
        .then(|| PathBuf::from("/real/agentty"))
}

#[test]
fn receipt_path_prefers_non_empty_xdg_config_home_and_falls_back_to_home() {
    // Arrange
    let xdg = Some(OsString::from("/xdg"));
    let home = Some(OsString::from("/home"));
    let empty = Some(OsString::new());

    // Act
    let xdg_receipt = ShellInstallReceipt::path(xdg, home.clone());
    let home_receipt = ShellInstallReceipt::path(None, home.clone());
    let empty_xdg_receipt = ShellInstallReceipt::path(empty.clone(), home);
    let missing_receipt = ShellInstallReceipt::path(empty.clone(), empty);

    // Assert
    assert_eq!(
        xdg_receipt,
        Some(PathBuf::from("/xdg/agentty/agentty-receipt.json"))
    );
    assert_eq!(
        home_receipt,
        Some(PathBuf::from("/home/.config/agentty/agentty-receipt.json"))
    );
    assert_eq!(empty_xdg_receipt, home_receipt);
    assert_eq!(missing_receipt, None);
}

#[test]
fn receipt_matches_the_installed_executable_through_links() {
    // Arrange
    let contents = receipt().to_string();

    // Act
    let matched = ShellInstallReceipt::for_executable(
        contents.as_bytes(),
        Path::new("/link/agentty"),
        canonicalize,
    );

    // Assert
    assert_eq!(
        matched.as_ref().map(ShellInstallReceipt::install_prefix),
        Some(Path::new("/prefix"))
    );
}

#[test]
fn receipt_must_identify_this_binary_and_build() {
    // Arrange
    let mismatches = [
        ("/provider/source", json!("cargo-dist")),
        ("/source/app_name", json!("other")),
        ("/version", json!("0.0.0-old")),
        ("/install_layout", json!("flat")),
        ("/install_prefix", Value::Null),
        ("/install_prefix", json!("relative")),
        ("/install_prefix", json!("/other")),
    ];

    // Act / Assert
    for (pointer, value) in mismatches {
        let mut data = receipt();
        *data.pointer_mut(pointer).expect("receipt field") = value.clone();
        let matched = ShellInstallReceipt::for_executable(
            data.to_string().as_bytes(),
            Path::new("/prefix/bin/agentty"),
            canonicalize,
        );
        assert_eq!(matched, None, "{pointer} = {value}");
    }
    assert_eq!(
        ShellInstallReceipt::for_executable(
            b"not json",
            Path::new("/prefix/bin/agentty"),
            canonicalize
        ),
        None
    );
    assert_eq!(
        ShellInstallReceipt::for_executable(
            receipt().to_string().as_bytes(),
            Path::new("/other/agentty"),
            |path| Some(path.to_path_buf()),
        ),
        None
    );
    assert_eq!(
        ShellInstallReceipt::for_executable(
            receipt().to_string().as_bytes(),
            Path::new("/missing/agentty"),
            canonicalize,
        ),
        None
    );
}

#[test]
fn receipt_ignores_modify_path() {
    // Arrange
    let mut data = receipt();
    data["modify_path"] = json!(true);

    // Act
    let matched = ShellInstallReceipt::for_executable(
        data.to_string().as_bytes(),
        Path::new("/prefix/bin/agentty"),
        canonicalize,
    );

    // Assert
    assert!(matched.is_some());
}

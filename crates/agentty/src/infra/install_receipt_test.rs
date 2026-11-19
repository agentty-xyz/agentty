//! Shell-installer receipt location and matching tests.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use semver::Version;
use serde_json::{Value, json};

use super::ShellInstallReceipt;

/// Signature shared by the receipt entry points, with a plain resolver.
type EntryPoint = fn(&[u8], &Path, fn(&Path) -> Option<PathBuf>) -> Option<ShellInstallReceipt>;

/// Install-method analytics and updater entry points with their names.
const ENTRY_POINTS: [(&str, EntryPoint); 2] = [
    ("for_running_build", ShellInstallReceipt::for_running_build),
    ("for_update", ShellInstallReceipt::for_update),
];

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

/// Returns a release version newer than the running build.
fn newer_version() -> String {
    let running = Version::parse(env!("CARGO_PKG_VERSION")).expect("running version");

    Version::new(running.major, running.minor + 1, 0).to_string()
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

    // Act / Assert
    for (name, entry_point) in ENTRY_POINTS {
        let matched = entry_point(
            contents.as_bytes(),
            Path::new("/link/agentty"),
            canonicalize,
        );
        assert_eq!(
            matched.as_ref().map(ShellInstallReceipt::install_prefix),
            Some(Path::new("/prefix")),
            "{name}"
        );
    }
}

/// An installer run by another Agentty process rewrites the receipt with
/// the newer version while this build keeps running: the install still
/// updates, but analytics no longer describe this build as a shell install.
#[test]
fn newer_receipt_matches_for_update_but_not_for_running_build() {
    // Arrange
    let mut data = receipt();
    data["version"] = json!(newer_version());
    let contents = data.to_string();

    // Act
    let for_update = ShellInstallReceipt::for_update(
        contents.as_bytes(),
        Path::new("/prefix/bin/agentty"),
        canonicalize,
    );
    let for_running_build = ShellInstallReceipt::for_running_build(
        contents.as_bytes(),
        Path::new("/prefix/bin/agentty"),
        canonicalize,
    );

    // Assert
    assert_eq!(
        for_update.as_ref().map(ShellInstallReceipt::install_prefix),
        Some(Path::new("/prefix"))
    );
    assert_eq!(for_running_build, None);
}

#[test]
fn receipt_must_identify_this_binary_and_build() {
    // Arrange
    let mismatches = [
        ("/provider/source", json!("cargo-dist")),
        ("/source/app_name", json!("other")),
        ("/version", json!("0.0.0-old")),
        ("/version", json!("not-semver")),
        ("/version", json!(1)),
        ("/install_layout", json!("flat")),
        ("/install_prefix", Value::Null),
        ("/install_prefix", json!("relative")),
        ("/install_prefix", json!("/other")),
    ];

    // Act / Assert
    for (name, entry_point) in ENTRY_POINTS {
        for (pointer, value) in &mismatches {
            let mut data = receipt();
            *data.pointer_mut(pointer).expect("receipt field") = value.clone();
            let matched = entry_point(
                data.to_string().as_bytes(),
                Path::new("/prefix/bin/agentty"),
                canonicalize,
            );
            assert_eq!(matched, None, "{name}: {pointer} = {value}");
        }
        assert_eq!(
            entry_point(b"not json", Path::new("/prefix/bin/agentty"), canonicalize),
            None,
            "{name}"
        );
        assert_eq!(
            entry_point(
                receipt().to_string().as_bytes(),
                Path::new("/other/agentty"),
                |path| Some(path.to_path_buf()),
            ),
            None,
            "{name}"
        );
        assert_eq!(
            entry_point(
                receipt().to_string().as_bytes(),
                Path::new("/missing/agentty"),
                canonicalize,
            ),
            None,
            "{name}"
        );
    }
}

#[test]
fn receipt_ignores_modify_path() {
    // Arrange
    let mut data = receipt();
    data["modify_path"] = json!(true);

    // Act / Assert
    for (name, entry_point) in ENTRY_POINTS {
        let matched = entry_point(
            data.to_string().as_bytes(),
            Path::new("/prefix/bin/agentty"),
            canonicalize,
        );
        assert!(matched.is_some(), "{name}");
    }
}

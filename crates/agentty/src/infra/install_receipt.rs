//! Interpretation of the receipt the `ag-release` shell installer writes,
//! shared by install-source analytics and the background updater.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Application name the shell installer records for Agentty.
const APP_NAME: &str = "agentty";

/// A shell-installer receipt proven to describe the running Agentty binary.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ShellInstallReceipt {
    install_prefix: PathBuf,
}

impl ShellInstallReceipt {
    /// Returns where the shell installer writes the receipt for the current
    /// process environment.
    pub(crate) fn host_path() -> Option<PathBuf> {
        Self::path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// Returns `agentty/agentty-receipt.json` below
    /// `${XDG_CONFIG_HOME:-$HOME/.config}`, treating empty variables as unset
    /// like the installer does.
    pub(crate) fn path(
        xdg_config_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Option<PathBuf> {
        let non_empty = |value: Option<OsString>| value.filter(|value| !value.is_empty());

        non_empty(xdg_config_home)
            .map(PathBuf::from)
            .or_else(|| non_empty(home).map(|home| Path::new(&home).join(".config")))
            .map(|config| config.join(format!("{APP_NAME}/{APP_NAME}-receipt.json")))
    }

    /// Returns the receipt in `contents` when it proves that `executable` is
    /// this build installed by the `ag-release` shell installer.
    ///
    /// The receipt must name `ag-release` as its provider, Agentty as its app,
    /// the running version, and the `cargo-home` layout, and its absolute
    /// `install_prefix` joined with `bin/agentty` must resolve, through
    /// `canonicalize`, to the same file as `executable`. `modify_path` is
    /// ignored because an update rewrites it as `false`.
    pub(crate) fn for_executable(
        contents: &[u8],
        executable: &Path,
        canonicalize: impl Fn(&Path) -> Option<PathBuf>,
    ) -> Option<Self> {
        let data = serde_json::from_slice::<Value>(contents).ok()?;
        if data["provider"]["source"] != "ag-release"
            || data["source"]["app_name"] != APP_NAME
            || data["version"] != env!("CARGO_PKG_VERSION")
            || data["install_layout"] != "cargo-home"
        {
            return None;
        }
        let install_prefix = data["install_prefix"]
            .as_str()
            .map(PathBuf::from)
            .filter(|prefix| prefix.is_absolute())?;
        let installed = canonicalize(&install_prefix.join("bin").join(APP_NAME))?;

        (canonicalize(executable)? == installed).then_some(Self { install_prefix })
    }

    /// Returns the install prefix recorded by the installer.
    pub(crate) fn install_prefix(&self) -> &Path {
        &self.install_prefix
    }
}

#[cfg(test)]
#[path = "install_receipt_test.rs"]
mod tests;

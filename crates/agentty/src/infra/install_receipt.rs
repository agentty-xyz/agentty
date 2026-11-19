//! Interpretation of the receipt the `ag-release` shell installer writes,
//! shared by install-source analytics and the background updater.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use semver::Version;
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
    /// The receipt must name exactly the running version, so install-method
    /// analytics describe the build that is running. See
    /// [`Self::for_update`] for the other requirements.
    pub(crate) fn for_running_build(
        contents: &[u8],
        executable: &Path,
        canonicalize: impl Fn(&Path) -> Option<PathBuf>,
    ) -> Option<Self> {
        Self::matching(contents, executable, canonicalize, Self::is_running_version)
    }

    /// Returns the receipt in `contents` when it proves that the
    /// `ag-release` shell installer owns `executable`, so rerunning the
    /// installer updates it.
    ///
    /// The receipt must name `ag-release` as its provider, Agentty as its app,
    /// the running version or a newer one, and the `cargo-home` layout, and
    /// its absolute `install_prefix` joined with `bin/agentty` must resolve,
    /// through `canonicalize`, to the same file as `executable`. A newer
    /// version is accepted because the installer rewrites the receipt on
    /// every run, so an install that another process already updated still
    /// updates again; an older or unparseable version means something else
    /// replaced the binary. `modify_path` is ignored because an update
    /// rewrites it as `false`.
    pub(crate) fn for_update(
        contents: &[u8],
        executable: &Path,
        canonicalize: impl Fn(&Path) -> Option<PathBuf>,
    ) -> Option<Self> {
        Self::matching(
            contents,
            executable,
            canonicalize,
            Self::is_running_or_newer_version,
        )
    }

    /// Returns the install prefix recorded by the installer.
    pub(crate) fn install_prefix(&self) -> &Path {
        &self.install_prefix
    }

    /// Applies the shared receipt checks, accepting the receipt's `version`
    /// only when `accepts_version` does.
    fn matching(
        contents: &[u8],
        executable: &Path,
        canonicalize: impl Fn(&Path) -> Option<PathBuf>,
        accepts_version: fn(&str) -> bool,
    ) -> Option<Self> {
        let data = serde_json::from_slice::<Value>(contents).ok()?;
        if data["provider"]["source"] != "ag-release"
            || data["source"]["app_name"] != APP_NAME
            || !data["version"].as_str().is_some_and(accepts_version)
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

    /// Returns whether `version` is exactly the running version.
    fn is_running_version(version: &str) -> bool {
        version == env!("CARGO_PKG_VERSION")
    }

    /// Returns whether `version` parses as semver no older than the running
    /// version.
    fn is_running_or_newer_version(version: &str) -> bool {
        match (
            Version::parse(version),
            Version::parse(env!("CARGO_PKG_VERSION")),
        ) {
            (Ok(receipt), Ok(running)) => receipt >= running,
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "install_receipt_test.rs"]
mod tests;

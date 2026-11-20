//! File names of every artifact a release publishes.
//!
//! These names are part of the public contract: installers, release notes, and
//! download URLs refer to them, so commands derive every name here.

use crate::target::Target;

/// Checksum list with one `sha256sum` line per published file.
pub(crate) const CHECKSUM_LIST: &str = "sha256.sum";

/// The release plan with the SHA-256 of every artifact.
pub(crate) const MANIFEST: &str = "release-manifest.json";

/// `git archive` of the tagged commit.
pub(crate) const SOURCE_ARCHIVE: &str = "source.tar.gz";

/// Returns the top-level directory inside an app's archive for `target`.
pub(crate) fn archive_directory(app: &str, target: Target) -> String {
    format!("{app}-{target}")
}

/// Returns the archive holding an app's binaries for `target`.
pub(crate) fn archive(app: &str, target: Target) -> String {
    format!("{}.tar.gz", archive_directory(app, target))
}

/// Returns the top-level directory inside the source archive,
/// `<repository name>-<version>`.
pub(crate) fn source_directory(repository: &str, version: &str) -> String {
    format!("{repository}-{version}")
}

/// Returns the `sha256sum`-compatible checksum file published next to `file`.
pub(crate) fn checksum(file: &str) -> String {
    format!("{file}.sha256")
}

/// Returns an app's shell installer script.
pub(crate) fn shell_installer(app: &str) -> String {
    format!("{app}-installer.sh")
}

/// Returns an app's Homebrew formula, `<app>.rb`, the file name a tap's
/// `Formula/` directory gives it.
pub(crate) fn homebrew_formula(app: &str) -> String {
    format!("{app}.rb")
}

/// `cargo binstall` URL template of an app's archive, the `pkg-url` of
/// `[package.metadata.binstall]`, for a `v`-prefixed release tag.
pub(crate) const BINSTALL_PKG_URL: &str =
    "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz";

/// [`BINSTALL_PKG_URL`] for a release tag without the `v` prefix.
pub(crate) const BINSTALL_UNPREFIXED_PKG_URL: &str =
    "{ repo }/releases/download/{ version }/{ name }-{ target }.tar.gz";

/// `cargo binstall` template of a binary's path in an app's archive, the
/// `bin-dir` of `[package.metadata.binstall]`.
pub(crate) const BINSTALL_BIN_DIR: &str = "{ name }-{ target }/{ bin }{ binary-ext }";

/// `cargo binstall` format of an app's archive, the `pkg-fmt` of
/// `[package.metadata.binstall]`.
pub(crate) const BINSTALL_PKG_FMT: &str = "tgz";

/// Returns an app's npm package tarball.
pub(crate) fn npm_tarball(app: &str) -> String {
    format!("{app}-npm-package.tar.gz")
}

/// Returns an app's npm package name, `<scope>/<app>` or `<app>`.
pub(crate) fn npm_package(app: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("{scope}/{app}"),
        None => app.to_string(),
    }
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;

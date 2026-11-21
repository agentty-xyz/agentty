//! The POSIX `sh` installer published as `<app>-installer.sh`.
//!
//! The script is rendered from `src/template/shell_installer.sh` with every
//! app-, repository-, and release-specific value baked in, each one quoted
//! for the shell, so the same inputs always render the same script.

use std::collections::BTreeMap;

use askama::Template;
use serde_json::json;

use crate::artifact;
use crate::config::GlibcVersion;
use crate::plan::{AppPlan, ReleasePlan};
use crate::target::Target;

/// Values of one app's shell installer.
#[derive(Template)]
#[template(path = "shell_installer.sh", escape = "none")]
pub(crate) struct ShellInstaller<'plan> {
    /// Version of `ag-release`, recorded in the header and the receipt.
    ag_release_version: &'static str,
    /// App name as written in comments.
    app: &'plan str,
    app_name: String,
    app_version: String,
    /// Shell words of the binaries to install.
    binaries: String,
    /// Release download URL used without `<APP>_DOWNLOAD_URL`.
    download_url: String,
    /// File name of the installer as written in its usage.
    installer: String,
    min_glibc: Option<GlibcVersion>,
    /// JSON values that the receipt embeds verbatim.
    receipt_binaries: String,
    receipt_provider: String,
    receipt_source: String,
    receipt_version: String,
    repository_url: &'plan str,
    /// Comma-separated list of the configured targets for error messages.
    supported_targets: String,
    /// One platform-detection `case` arm per configured target.
    targets: Vec<TargetArm>,
    /// Prefix of the installer's environment variables, `<APP>`.
    variable_prefix: String,
    /// Version as written in comments.
    version: &'plan str,
}

impl<'plan> ShellInstaller<'plan> {
    /// Renders `app`'s installer for the release `tag` of `plan`, embedding
    /// each archive's SHA-256 from `sha256`.
    ///
    /// # Errors
    /// Returns an error when `sha256` has no digest for one of the app's
    /// archives.
    pub(crate) fn render(
        plan: &'plan ReleasePlan,
        app: &'plan AppPlan,
        tag: &str,
        sha256: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let targets = app
            .archives
            .iter()
            .map(|archive| {
                let digest = sha256
                    .get(&archive.name)
                    .ok_or_else(|| format!("no SHA-256 is recorded for `{}`", archive.name))?;

                Ok(TargetArm {
                    archive: Self::quote(&archive.name),
                    directory: Self::quote(&archive.directory),
                    patterns: Self::patterns(archive.target),
                    sha256: Self::quote(digest),
                    target: archive.target.triple(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let repository = &plan.repository;
        let installer = Self {
            ag_release_version: env!("CARGO_PKG_VERSION"),
            app: &app.name,
            app_name: Self::quote(&app.name),
            app_version: Self::quote(&plan.version),
            binaries: app
                .binaries
                .iter()
                .map(|binary| Self::quote(binary))
                .collect::<Vec<_>>()
                .join(" "),
            download_url: Self::quote(&plan.download_url(tag)),
            installer: artifact::shell_installer(&app.name),
            min_glibc: plan.min_glibc,
            receipt_binaries: Self::quote(&json!(app.binaries).to_string()),
            receipt_provider: Self::quote(
                &json!({"source": "ag-release", "version": env!("CARGO_PKG_VERSION")}).to_string(),
            ),
            receipt_source: Self::quote(
                &json!({
                    "app_name": app.name,
                    "name": repository.name,
                    "owner": repository.owner,
                    "release_type": "github",
                })
                .to_string(),
            ),
            receipt_version: Self::quote(&json!(plan.version).to_string()),
            repository_url: &repository.url,
            supported_targets: Self::quote(
                &app.archives
                    .iter()
                    .map(|archive| archive.target.triple())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            targets,
            variable_prefix: app.environment_prefix(),
            version: &plan.version,
        };

        Ok(format!("{installer}\n"))
    }

    /// Returns the `case` patterns that match
    /// `"<uname -s> <uname -m> <libc>"` on `target`, where `<libc>` is
    /// [`Libc::name`](crate::target::Libc::name).
    fn patterns(target: Target) -> String {
        target
            .uname_machines()
            .iter()
            .map(|machine| {
                Self::quote(&format!(
                    "{} {machine} {}",
                    target.uname_system(),
                    target.libc().name()
                ))
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Returns `value` as one single-quoted shell word.
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

/// The platform-detection `case` arm of one target.
struct TargetArm {
    /// Shell word of the archive file name.
    archive: String,
    /// Shell word of the archive's top-level directory.
    directory: String,
    /// `case` patterns that select this target.
    patterns: String,
    /// Shell word of the archive's SHA-256.
    sha256: String,
    target: &'static str,
}

#[cfg(test)]
#[path = "shell_installer_test.rs"]
mod tests;

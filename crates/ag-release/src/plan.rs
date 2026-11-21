//! The release plan: the machine-readable contract that release workflows
//! consume and that `release-manifest.json` embeds.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::artifact;
use crate::config::{GlibcVersion, HomebrewTap, Installer};
use crate::repository::GitHubRepository;
use crate::tag::ReleaseTag;
use crate::target::Target;
use crate::workspace::{App, Workspace};

/// Everything one release builds and publishes. Serialized field names are
/// part of the public contract documented in the crate README.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ReleasePlan {
    /// Released apps in name order.
    pub(crate) apps: Vec<AppPlan>,
    /// Release-wide artifact file names.
    pub(crate) artifacts: ReleaseArtifacts,
    /// Homebrew tap of the formulas when the Homebrew installer is enabled.
    pub(crate) homebrew_tap: Option<HomebrewTap>,
    /// Configured paths of the files added to every archive, which are also
    /// their paths in the archive, whichever directory supplied them.
    pub(crate) include: Vec<String>,
    pub(crate) installers: BTreeSet<Installer>,
    /// GitHub Actions build matrix with one entry per target.
    pub(crate) matrix: BuildMatrix,
    pub(crate) min_glibc: Option<GlibcVersion>,
    /// Whether the version has a semver pre-release component.
    pub(crate) prerelease: bool,
    /// Cargo profile used for release builds.
    pub(crate) profile: String,
    pub(crate) repository: GitHubRepository,
    /// The release tag, or `None` when planning without one.
    pub(crate) tag: Option<String>,
    /// The version every app shares.
    pub(crate) version: String,
}

impl ReleasePlan {
    /// Plans a release of `workspace`, checking that `tag`, when given, names
    /// the workspace's app version.
    ///
    /// # Errors
    /// Returns an error when `tag` names a different version than the apps.
    pub(crate) fn new(workspace: &Workspace, tag: Option<&ReleaseTag>) -> Result<Self, String> {
        if let Some(tag) = tag
            && tag.version != workspace.version
        {
            return Err(format!(
                "release tag `{}` names version {}, but the apps have version {}",
                tag.name, tag.version, workspace.version
            ));
        }
        let config = &workspace.config;
        let targets = config.targets.keys().copied().collect::<Vec<_>>();

        Ok(Self {
            apps: workspace
                .apps
                .iter()
                .map(|app| AppPlan::new(app, &targets, &config.installers))
                .collect(),
            artifacts: ReleaseArtifacts::new(),
            homebrew_tap: config.homebrew_tap.clone(),
            include: config.include.clone(),
            installers: config.installers.clone(),
            matrix: BuildMatrix {
                include: config
                    .targets
                    .iter()
                    .map(|(target, runner)| BuildJob {
                        runner: runner.clone(),
                        target: *target,
                    })
                    .collect(),
            },
            min_glibc: config.min_glibc,
            prerelease: !workspace.version.pre.is_empty(),
            profile: config.profile.clone(),
            repository: workspace.repository.clone(),
            tag: tag.map(|tag| tag.name.clone()),
            version: workspace.version.to_string(),
        })
    }

    /// Returns the GitHub URL that the release `tag`'s files download from.
    pub(crate) fn download_url(&self, tag: &str) -> String {
        format!("{}/releases/download/{tag}", self.repository.url)
    }
}

/// One app's release contents and artifact file names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AppPlan {
    /// One archive per target, in target order.
    pub(crate) archives: Vec<ArchivePlan>,
    pub(crate) authors: Vec<String>,
    pub(crate) binaries: Vec<String>,
    pub(crate) description: Option<String>,
    /// Homebrew formula `desc` when the Homebrew installer is enabled: the
    /// configured `homebrew-description`, else the package description.
    pub(crate) homebrew_description: Option<String>,
    /// Homebrew formula file when the Homebrew installer is enabled.
    pub(crate) homebrew_formula: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) name: String,
    /// npm package name when the npm installer is enabled.
    pub(crate) npm_package: Option<String>,
    /// npm package tarball when the npm installer is enabled.
    pub(crate) npm_tarball: Option<String>,
    /// Shell installer script when the shell installer is enabled.
    pub(crate) shell_installer: Option<String>,
}

impl AppPlan {
    fn new(app: &App, targets: &[Target], installers: &BTreeSet<Installer>) -> Self {
        let name = app.name.as_str();
        let npm = installers.contains(&Installer::Npm);
        let homebrew = installers.contains(&Installer::Homebrew);

        Self {
            archives: targets
                .iter()
                .map(|target| ArchivePlan::new(name, *target))
                .collect(),
            authors: app.authors.clone(),
            binaries: app.binaries.clone(),
            description: app.description.clone(),
            homebrew_description: homebrew
                .then(|| {
                    app.homebrew_description
                        .clone()
                        .or_else(|| app.description.clone())
                })
                .flatten(),
            homebrew_formula: homebrew.then(|| artifact::homebrew_formula(name)),
            license: app.license.clone(),
            name: name.to_string(),
            npm_package: npm.then(|| artifact::npm_package(name, app.npm_scope.as_deref())),
            npm_tarball: npm.then(|| artifact::npm_tarball(name)),
            shell_installer: installers
                .contains(&Installer::Shell)
                .then(|| artifact::shell_installer(name)),
        }
    }

    /// Returns `<APP>`, the prefix of the installers' environment variables:
    /// the app name in upper case with `-` replaced by `_`.
    pub(crate) fn environment_prefix(&self) -> String {
        self.name.to_ascii_uppercase().replace('-', "_")
    }
}

/// One app archive for one target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ArchivePlan {
    /// Checksum file published next to the archive.
    pub(crate) checksum: String,
    /// Top-level directory inside the archive.
    pub(crate) directory: String,
    /// Archive file name.
    pub(crate) name: String,
    pub(crate) target: Target,
}

impl ArchivePlan {
    fn new(app: &str, target: Target) -> Self {
        let name = artifact::archive(app, target);

        Self {
            checksum: artifact::checksum(&name),
            directory: artifact::archive_directory(app, target),
            name,
            target,
        }
    }
}

/// File names of the release-wide artifacts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ReleaseArtifacts {
    /// Checksum list covering every published file.
    pub(crate) checksums: String,
    /// Release manifest embedding this plan.
    pub(crate) manifest: String,
    /// Source archive of the tagged commit.
    pub(crate) source_archive: String,
    /// Checksum file of the source archive.
    pub(crate) source_checksum: String,
}

impl ReleaseArtifacts {
    fn new() -> Self {
        Self {
            checksums: artifact::CHECKSUM_LIST.to_string(),
            manifest: artifact::MANIFEST.to_string(),
            source_archive: artifact::SOURCE_ARCHIVE.to_string(),
            source_checksum: artifact::checksum(artifact::SOURCE_ARCHIVE),
        }
    }
}

/// A GitHub Actions matrix, `{"include": [{"runner", "target"}]}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct BuildMatrix {
    pub(crate) include: Vec<BuildJob>,
}

/// One build job of the matrix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct BuildJob {
    /// GitHub Actions runner label.
    pub(crate) runner: String,
    pub(crate) target: Target,
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;

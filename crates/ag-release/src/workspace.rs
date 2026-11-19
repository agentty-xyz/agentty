//! Release inputs read from `cargo metadata`: the release configuration and
//! the package data of every configured app.

use std::ffi::OsString;
use std::path::PathBuf;

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

use crate::config::ReleaseConfig;
use crate::process::{ProcessCommand, ProcessRunner};
use crate::repository::GitHubRepository;

/// A workspace's validated release configuration and app packages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Workspace {
    /// Configured apps in name order.
    pub(crate) apps: Vec<App>,
    pub(crate) config: ReleaseConfig,
    /// GitHub repository every app names in its `repository` field.
    pub(crate) repository: GitHubRepository,
    /// Absolute workspace root directory.
    pub(crate) root: PathBuf,
    /// Version every app shares; apps release in lockstep.
    pub(crate) version: Version,
}

impl Workspace {
    /// Reads the workspace that contains the current directory with one
    /// `cargo metadata --format-version 1 --no-deps` call.
    ///
    /// # Errors
    /// Returns an error when `cargo metadata` fails or the configuration or
    /// app packages are invalid.
    pub(crate) fn load(process: &dyn ProcessRunner, cargo: OsString) -> Result<Self, String> {
        let command = ProcessCommand {
            args: ["metadata", "--format-version", "1", "--no-deps"]
                .map(OsString::from)
                .to_vec(),
            program: cargo,
        };

        Self::from_metadata(&process.output(&command)?)
    }

    /// Validates `cargo metadata --format-version 1 --no-deps` output.
    ///
    /// # Errors
    /// Returns an error when the output is not Cargo metadata or the
    /// configuration or app packages are invalid.
    pub(crate) fn from_metadata(metadata: &[u8]) -> Result<Self, String> {
        let CargoMetadata {
            packages,
            workspace_metadata,
            workspace_root,
        } = serde_json::from_slice(metadata)
            .map_err(|error| format!("failed to parse `cargo metadata` output: {error}"))?;
        let config = ReleaseConfig::parse(workspace_metadata.as_ref())?;
        let apps = config
            .apps
            .iter()
            .map(|(name, app)| {
                let package = packages
                    .iter()
                    .find(|package| &package.name == name)
                    .ok_or_else(|| {
                        format!(
                            "`workspace.metadata.release.apps.{name}` is not a workspace package"
                        )
                    })?;

                App::from_package(package, app.npm_scope.clone())
            })
            .collect::<Result<Vec<_>, String>>()?;
        let (repository, version) = Self::shared_identity(&apps)?;

        Ok(Self {
            apps,
            config,
            repository,
            root: workspace_root,
            version,
        })
    }

    /// Returns the repository and version all apps share. The configuration
    /// guarantees at least one app.
    fn shared_identity(apps: &[App]) -> Result<(GitHubRepository, Version), String> {
        let (first, rest) = apps.split_first().ok_or("no apps are configured")?;
        for app in rest {
            if app.version != first.version {
                return Err(format!(
                    "apps release in lockstep, but `{}` has version {} and `{}` has version {}",
                    first.name, first.version, app.name, app.version
                ));
            }
            if app.repository != first.repository {
                return Err(format!(
                    "apps must share one repository, but `{}` names {} and `{}` names {}",
                    first.name, first.repository.url, app.name, app.repository.url
                ));
            }
        }

        Ok((first.repository.clone(), first.version.clone()))
    }
}

/// One released workspace package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct App {
    pub(crate) authors: Vec<String>,
    /// Names of the package's `bin` targets, sorted.
    pub(crate) binaries: Vec<String>,
    pub(crate) description: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) name: String,
    pub(crate) npm_scope: Option<String>,
    pub(crate) repository: GitHubRepository,
    pub(crate) version: Version,
}

impl App {
    fn from_package(package: &MetadataPackage, npm_scope: Option<String>) -> Result<Self, String> {
        let name = &package.name;
        let mut binaries = package
            .targets
            .iter()
            .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        binaries.sort();
        if binaries.is_empty() {
            return Err(format!("app `{name}` has no binary targets"));
        }
        let repository = package.repository.as_deref().ok_or_else(|| {
            format!("app `{name}` must set `repository` to its GitHub URL in `Cargo.toml`")
        })?;
        let repository = GitHubRepository::parse(repository)
            .map_err(|error| format!("app `{name}`: {error}"))?;
        let version = Version::parse(&package.version)
            .map_err(|error| format!("app `{name}` has an invalid version: {error}"))?;

        Ok(Self {
            authors: package.authors.clone(),
            binaries,
            description: package.description.clone(),
            license: package.license.clone(),
            name: name.clone(),
            npm_scope,
            repository,
            version,
        })
    }
}

/// Subset of `cargo metadata --format-version 1` used by releases.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetadataPackage>,
    /// The `[workspace.metadata]` table, or `null` when it is absent.
    #[serde(rename = "metadata")]
    workspace_metadata: Option<Value>,
    workspace_root: PathBuf,
}

/// Subset of one `cargo metadata` package.
#[derive(Deserialize)]
struct MetadataPackage {
    authors: Vec<String>,
    description: Option<String>,
    license: Option<String>,
    name: String,
    repository: Option<String>,
    targets: Vec<MetadataTarget>,
    version: String,
}

/// Subset of one `cargo metadata` build target.
#[derive(Deserialize)]
struct MetadataTarget {
    kind: Vec<String>,
    name: String,
}

#[cfg(test)]
#[path = "workspace_support_test.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

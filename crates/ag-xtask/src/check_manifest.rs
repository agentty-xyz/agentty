//! Check that workspace members inherit every dependency from the root
//! manifest, that no dependency enables Tokio's `full` feature, and that no
//! member declares a dependency banned for it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use serde::Deserialize;

/// Member manifest tables that declare dependencies, at the top level or under
/// a `[target.<cfg>]` table.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Fix shared by the `ag-clipboard` Wayland bans.
const WAYLAND_FIX: &str =
    "read the Wayland clipboard through the audited `wl-paste` subprocess backend instead";

/// Dependencies that a workspace member must not declare, each with the fix
/// that replaces it.
const BANNED_DEPENDENCIES: [BannedDependency; 5] = [
    BannedDependency {
        dependency: "smithay-clipboard",
        fix: WAYLAND_FIX,
        package: "ag-clipboard",
    },
    BannedDependency {
        dependency: "wayland-backend",
        fix: WAYLAND_FIX,
        package: "ag-clipboard",
    },
    BannedDependency {
        dependency: "wayland-client",
        fix: WAYLAND_FIX,
        package: "ag-clipboard",
    },
    BannedDependency {
        dependency: "wayland-protocols",
        fix: WAYLAND_FIX,
        package: "ag-clipboard",
    },
    BannedDependency {
        dependency: "wl-clipboard-rs",
        fix: WAYLAND_FIX,
        package: "ag-clipboard",
    },
];

/// Validates workspace member manifests using `cargo metadata`.
///
/// # Errors
/// Returns an error when workspace metadata or a manifest is unreadable, or
/// when a member dependency bypasses `[workspace.dependencies]`, enables
/// Tokio's `full` feature, or appears in [`BANNED_DEPENDENCIES`].
pub(crate) fn run() -> Result<(), String> {
    ManifestCheck {
        workspace: &RealWorkspace {
            cargo: env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        },
    }
    .run()
}

/// Workspace inputs inspected by the manifest check.
#[cfg_attr(test, mockall::automock)]
trait Workspace {
    /// Returns every workspace member with its manifest text.
    fn packages(&self) -> Result<Vec<Package>, String>;
}

/// One workspace member and the dependencies Cargo resolved for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Package {
    dependencies: Vec<Dependency>,
    manifest: String,
    manifest_path: PathBuf,
    name: String,
}

/// One dependency banned for one workspace member.
struct BannedDependency {
    dependency: &'static str,
    fix: &'static str,
    package: &'static str,
}

/// One declared dependency with the features Cargo enables for it, including
/// features inherited from `[workspace.dependencies]`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Dependency {
    features: Vec<String>,
    name: String,
}

/// Manifest policy, independent of Cargo and the host filesystem.
struct ManifestCheck<'workspace> {
    workspace: &'workspace dyn Workspace,
}

impl ManifestCheck<'_> {
    fn run(&self) -> Result<(), String> {
        let mut violations = Vec::new();
        for package in self.workspace.packages()? {
            violations.extend(Self::uninherited_dependencies(&package)?);
            violations.extend(Self::full_tokio(&package));
            violations.extend(Self::banned_dependencies(&package));
        }

        if violations.is_empty() {
            return Ok(());
        }

        Err(format!("Manifest violations:\n{}", violations.join("\n")))
    }

    /// Reports each dependency entry that does not set `workspace = true`.
    fn uninherited_dependencies(package: &Package) -> Result<Vec<String>, String> {
        let path = package.manifest_path.display();
        let manifest = package
            .manifest
            .parse::<toml::Table>()
            .map_err(|error| format!("Failed to parse {path}: {error}"))?;
        let mut tables = DEPENDENCY_TABLES
            .into_iter()
            .filter_map(|name| Some((name.to_string(), manifest.get(name)?)))
            .collect::<Vec<_>>();
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            for (platform, target) in targets {
                tables.extend(DEPENDENCY_TABLES.into_iter().filter_map(|name| {
                    Some((format!("target.{platform}.{name}"), target.get(name)?))
                }));
            }
        }

        let mut violations = Vec::new();
        for (table_name, table) in tables {
            let Some(entries) = table.as_table() else {
                violations.push(format!("{path}: [{table_name}] must be a table"));
                continue;
            };
            for (dependency, entry) in entries {
                if entry.get("workspace").and_then(toml::Value::as_bool) != Some(true) {
                    violations.push(format!(
                        "{path}: [{table_name}] `{dependency}` must use `workspace = true`; \
                         declare it under `[workspace.dependencies]` in the root `Cargo.toml`"
                    ));
                }
            }
        }

        Ok(violations)
    }

    /// Reports each Tokio dependency that enables the `full` feature.
    fn full_tokio(package: &Package) -> Vec<String> {
        package
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.name == "tokio" && dependency.features.iter().any(|name| name == "full")
            })
            .map(|_| {
                format!(
                    "{}: `tokio` enables `full`; enable only the Tokio features the crate uses",
                    package.manifest_path.display()
                )
            })
            .collect()
    }

    /// Reports each dependency that [`BANNED_DEPENDENCIES`] bans for the
    /// package.
    fn banned_dependencies(package: &Package) -> Vec<String> {
        package
            .dependencies
            .iter()
            .filter_map(|dependency| {
                let banned = BANNED_DEPENDENCIES.iter().find(|banned| {
                    banned.package == package.name && banned.dependency == dependency.name
                })?;

                Some(format!(
                    "{}: `{}` must not depend on `{}`; {}",
                    package.manifest_path.display(),
                    package.name,
                    dependency.name,
                    banned.fix
                ))
            })
            .collect()
    }
}

/// Host adapter; manifest decisions remain in `ManifestCheck`.
struct RealWorkspace {
    cargo: OsString,
}

impl RealWorkspace {
    /// Parses `cargo metadata` output and reads each member manifest, keeping
    /// manifest paths relative to the workspace root.
    fn parse_packages(metadata: &[u8]) -> Result<Vec<Package>, String> {
        let Metadata {
            packages,
            workspace_root,
        } = serde_json::from_slice(metadata)
            .map_err(|error| format!("Failed to parse cargo metadata: {error}"))?;

        packages
            .into_iter()
            .map(|package| {
                let manifest = fs::read_to_string(&package.manifest_path).map_err(|error| {
                    format!(
                        "Failed to read {}: {error}",
                        package.manifest_path.display()
                    )
                })?;

                Ok(Package {
                    dependencies: package.dependencies,
                    manifest,
                    manifest_path: Self::relative(&package.manifest_path, &workspace_root),
                    name: package.name,
                })
            })
            .collect()
    }

    fn relative(path: &Path, root: &Path) -> PathBuf {
        path.strip_prefix(root).unwrap_or(path).to_path_buf()
    }
}

impl Workspace for RealWorkspace {
    fn packages(&self) -> Result<Vec<Package>, String> {
        let output = Command::new(&self.cargo)
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--no-deps",
                "--format-version",
                "1",
            ])
            .output()
            .map_err(|error| format!("Failed to run cargo metadata: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        Self::parse_packages(&output.stdout)
    }
}

/// Subset of `cargo metadata --format-version 1` used by the check.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetadataPackage {
    dependencies: Vec<Dependency>,
    manifest_path: PathBuf,
    name: String,
}

#[cfg(test)]
#[path = "check_manifest_test.rs"]
mod tests;

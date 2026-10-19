//! Catalog-driven validation with conservative reuse and build coordination.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use clap::Args;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::info;

/// Arguments for coordinated workspace validation.
#[derive(Args)]
pub(crate) struct Options {
    /// Changed packages; defaults to `AGENTTY_TEST_PACKAGES`.
    #[arg(long, value_delimiter = ',')]
    packages: Vec<String>,
    /// Hook IDs from the repository catalog, executed in order.
    #[arg(
        long = "hook",
        value_delimiter = ',',
        default_value = "clippy,test-focused"
    )]
    hooks: Vec<String>,
    /// Reuse matching successful deterministic checks; fresh execution is
    /// default.
    #[arg(long)]
    reuse: bool,
}

/// Runs checks through prek without duplicating their executable definitions.
pub(crate) fn run(options: Options) -> Result<(), String> {
    Runner { host: &ProcessHost }.run(options)
}

type Ledger = BTreeMap<String, String>;

trait ValidationGuard {}
impl ValidationGuard for File {}

/// External boundaries shared by local execution and deterministic tests.
trait Host {
    fn packages(&self) -> Vec<String>;
    fn metadata(&self) -> Result<Metadata, String>;
    fn catalog(&self) -> Result<Catalog, String>;
    fn lock(&self, target: &Path) -> Result<Box<dyn ValidationGuard>, String>;
    fn load(&self, target: &Path) -> Result<Ledger, String>;
    fn fingerprint(&self, packages: &[String], hook: &str, target: &Path)
    -> Result<String, String>;
    fn execute(&self, hook: &Hook, packages: &[String], target: &Path) -> Result<(), String>;
    fn save(&self, target: &Path, ledger: &Ledger) -> Result<(), String>;
}

struct Runner<'host> {
    host: &'host dyn Host,
}

impl Runner<'_> {
    fn run(&self, options: Options) -> Result<(), String> {
        let seeds = if options.packages.is_empty() {
            self.host.packages()
        } else {
            options.packages
        };
        if seeds.is_empty() {
            return Err(
                "Set --packages or AGENTTY_TEST_PACKAGES to the changed workspace packages".into(),
            );
        }
        let metadata = self.host.metadata()?;
        let packages = metadata.affected(&seeds)?;
        let catalog = self.host.catalog()?;
        let hooks = options
            .hooks
            .iter()
            .map(|id| catalog.hook(id))
            .collect::<Result<Vec<_>, _>>()?;
        let target = metadata.target_directory;
        if target.as_os_str().is_empty()
            || target
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err("CARGO_TARGET_DIR must be a directory within the workspace".into());
        }
        let _guard = self.host.lock(&target)?;
        let mut ledger = self.host.load(&target)?;
        for hook in hooks {
            let before = self.host.fingerprint(&packages, &hook.id, &target)?;
            if options.reuse && hook.reusable() && ledger.get(&hook.id) == Some(&before) {
                info!(hook = hook.id, "Reusing matching successful validation");
                continue;
            }
            ledger.remove(&hook.id);
            self.host.save(&target, &ledger)?;
            self.host.execute(&hook, &packages, &target)?;
            if self.host.fingerprint(&packages, &hook.id, &target)? != before {
                return Err(format!(
                    "Inputs changed during {}; rerun validation for the final state",
                    hook.id
                ));
            }
            ledger.insert(hook.id, before);
            self.host.save(&target, &ledger)?;
        }

        Ok(())
    }
}

/// Workspace package graph read from `cargo metadata --no-deps`.
#[derive(Deserialize)]
pub(crate) struct Metadata {
    packages: Vec<Package>,
    target_directory: PathBuf,
    workspace_members: Vec<String>,
}

impl Metadata {
    /// Reads the workspace package graph from Cargo.
    pub(crate) fn load() -> Result<Self, String> {
        serde_json::from_slice(&ProcessHost::output(
            "cargo",
            &["metadata", "--locked", "--no-deps", "--format-version", "1"],
        )?)
        .map_err(|error| error.to_string())
    }

    /// Includes the changed packages and their transitive dependents. Cargo
    /// still compiles dependencies, but their unchanged tests are not rerun.
    pub(crate) fn affected(&self, seeds: &[String]) -> Result<Vec<String>, String> {
        let graph = self
            .packages
            .iter()
            .filter(|package| self.workspace_members.contains(&package.id))
            .map(|package| {
                let dependencies = package
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.path.is_some())
                    .map(|dependency| dependency.name.clone())
                    .collect::<BTreeSet<_>>();
                (package.name.clone(), dependencies)
            })
            .collect::<BTreeMap<_, _>>();
        for seed in seeds {
            if !graph.contains_key(seed) {
                return Err(format!("Unknown workspace package: {seed}"));
            }
        }
        let mut affected = BTreeSet::new();
        let mut pending = seeds.to_vec();
        while let Some(package) = pending.pop() {
            if affected.insert(package.clone()) {
                pending.extend(
                    graph
                        .iter()
                        .filter(|(_, dependencies)| dependencies.contains(&package))
                        .map(|(name, _)| name.clone()),
                );
            }
        }

        Ok(affected.into_iter().collect())
    }

    /// Returns the workspace packages whose directories contain `files`, or
    /// `None` when a file belongs to no package, such as the root manifest or
    /// lockfile, and so can affect the whole workspace.
    pub(crate) fn owners(&self, files: &[PathBuf], root: &Path) -> Option<Vec<String>> {
        let directories = self
            .packages
            .iter()
            .filter(|package| self.workspace_members.contains(&package.id))
            .filter_map(|package| Some((package.manifest_path.parent()?, &package.name)))
            .collect::<Vec<_>>();
        let mut owners = BTreeSet::new();
        for file in files {
            let path = root.join(file);
            let (_, owner) = directories
                .iter()
                .filter(|(directory, _)| path.starts_with(directory))
                .max_by_key(|(directory, _)| directory.components().count())?;
            owners.insert((*owner).clone());
        }

        Some(owners.into_iter().collect())
    }
}

#[derive(Deserialize)]
struct Package {
    dependencies: Vec<Dependency>,
    id: String,
    manifest_path: PathBuf,
    name: String,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    path: Option<String>,
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(default)]
    default_stages: Vec<String>,
    repos: Vec<Repository>,
}

impl Catalog {
    fn hook(&self, id: &str) -> Result<Hook, String> {
        if id == "validate" {
            return Err("The validate hook cannot invoke itself".into());
        }
        let mut hook = self
            .repos
            .iter()
            .flat_map(|repo| &repo.hooks)
            .find(|hook| hook.id == id)
            .cloned()
            .ok_or_else(|| format!("Unknown cataloged hook: {id}"))?;
        if hook.stages.is_empty() {
            hook.stages.clone_from(&self.default_stages);
        }
        if !hook
            .stages
            .iter()
            .any(|stage| matches!(stage.as_str(), "manual" | "pre-commit"))
        {
            return Err(format!("Hook {id} does not support workspace validation"));
        }

        Ok(hook)
    }
}

#[derive(Deserialize)]
struct Repository {
    hooks: Vec<Hook>,
}

#[derive(Clone, Deserialize)]
struct Hook {
    id: String,
    #[serde(default)]
    stages: Vec<String>,
}

impl Hook {
    fn reusable(&self) -> bool {
        matches!(
            self.id.as_str(),
            "clippy"
                | "rustfmt"
                | "test-focused"
                | "check-execution-boundary"
                | "check-instructions"
                | "check-migrations"
        )
    }
}

struct ProcessHost;

impl ProcessHost {
    fn output(program: &str, arguments: &[&str]) -> Result<Vec<u8>, String> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|error| format!("Could not execute {program}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "{program} failed while collecting validation inputs"
            ));
        }

        Ok(output.stdout)
    }

    /// Includes linked directories while avoiding recursive directory cycles.
    fn include_sources(
        paths: &[u8],
        target: &Path,
        include: &mut dyn FnMut(&[u8]),
    ) -> Result<(), String> {
        let mut pending = BTreeSet::new();
        for bytes in paths
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path =
                PathBuf::from(std::str::from_utf8(bytes).map_err(|error| error.to_string())?);
            if path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("Git returned a path outside the workspace".into());
            }
            pending.insert(path);
        }
        let mut directories = BTreeSet::new();
        while let Some(path) = pending.pop_first() {
            if Self::inside_target(&path, target) {
                continue;
            }
            include(path.as_os_str().as_encoded_bytes());
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    include(b"deleted");
                    continue;
                }
                Err(error) => return Err(error.to_string()),
            };
            include(format!("{:?}", metadata.permissions()).as_bytes());
            if metadata.file_type().is_symlink() {
                include(b"link");
                include(
                    fs::read_link(&path)
                        .map_err(|error| error.to_string())?
                        .as_os_str()
                        .as_encoded_bytes(),
                );
            }
            if path.is_dir() {
                include(b"directory");
                if directories.insert(path.canonicalize().map_err(|error| error.to_string())?) {
                    for entry in fs::read_dir(&path).map_err(|error| error.to_string())? {
                        pending.insert(entry.map_err(|error| error.to_string())?.path());
                    }
                }
                continue;
            }
            match fs::read(&path) {
                Ok(content) => {
                    include(b"present");
                    include(&content);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => include(b"deleted"),
                Err(error) => return Err(error.to_string()),
            }
        }

        Ok(())
    }

    /// Resolves missing linked outputs through their existing parent
    /// directories.
    fn inside_target(path: &Path, target: &Path) -> bool {
        let mut path = path.to_path_buf();
        loop {
            match path.canonicalize() {
                Ok(resolved) => return resolved.starts_with(target),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return false,
            }
            if let Ok(link) = fs::read_link(&path) {
                path = if link.is_absolute() {
                    link
                } else {
                    path.parent().unwrap_or_else(|| Path::new(".")).join(link)
                };
            } else if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                path = parent.to_path_buf();
            } else {
                return false;
            }
        }
    }
}

impl Host for ProcessHost {
    fn packages(&self) -> Vec<String> {
        std::env::var("AGENTTY_TEST_PACKAGES")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn metadata(&self) -> Result<Metadata, String> {
        let mut metadata = Metadata::load()?;
        if metadata.target_directory.is_absolute() {
            let root = std::env::current_dir().map_err(|error| error.to_string())?;
            metadata.target_directory = metadata
                .target_directory
                .strip_prefix(root)
                .map_err(|_| "Cargo target directory must be within the workspace".to_string())?
                .to_path_buf();
        }

        Ok(metadata)
    }

    fn catalog(&self) -> Result<Catalog, String> {
        serde_yaml_ng::from_slice(
            &fs::read(".pre-commit-config.yaml").map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    fn lock(&self, target: &Path) -> Result<Box<dyn ValidationGuard>, String> {
        let root = std::env::current_dir()
            .and_then(|root| root.canonicalize())
            .map_err(|error| error.to_string())?;
        let mut ancestor = target;
        let resolved = loop {
            match ancestor.canonicalize() {
                Ok(resolved) => break resolved,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."));
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        if !resolved.starts_with(&root) {
            return Err("Validation target resolves outside the workspace".into());
        }
        fs::create_dir_all(target).map_err(|error| error.to_string())?;
        if target.canonicalize().map_err(|error| error.to_string())? == root {
            return Err("CARGO_TARGET_DIR must not be the workspace root".into());
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(target.join("agentty-validation.lock"))
            .map_err(|error| error.to_string())?;
        file.lock().map_err(|error| error.to_string())?;

        Ok(Box::new(file))
    }

    fn load(&self, target: &Path) -> Result<Ledger, String> {
        match fs::read(target.join("agentty-validation.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::new()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn fingerprint(
        &self,
        packages: &[String],
        hook: &str,
        target: &Path,
    ) -> Result<String, String> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let target = target.canonicalize().map_err(|error| error.to_string())?;
        let root = std::env::current_dir()
            .and_then(|root| root.canonicalize())
            .map_err(|error| error.to_string())?;
        let relative = target
            .strip_prefix(&root)
            .map_err(|error| error.to_string())?;
        let exclusion = format!(
            ":(top,literal,exclude){}",
            relative
                .to_str()
                .ok_or("CARGO_TARGET_DIR must be valid UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let tracked = Self::output("git", &["ls-files", "--cached", "-z"])?;
        for bytes in tracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = Path::new(std::str::from_utf8(bytes).map_err(|error| error.to_string())?);
            if path.starts_with(relative) {
                return Err("CARGO_TARGET_DIR must not contain tracked source files".into());
            }
        }
        let mut hash = Sha256::new();
        let mut include = |bytes: &[u8]| {
            hash.update(bytes.len().to_le_bytes());
            hash.update(bytes);
        };
        include(b"agentty-validation-v3");
        include(relative.as_os_str().as_encoded_bytes());
        include(hook.as_bytes());
        for package in packages {
            include(package.as_bytes());
        }
        for (program, arguments) in [
            ("rustc", vec!["-Vv"]),
            ("prek", vec!["--version"]),
            ("cargo", vec!["--version"]),
            ("cargo", vec!["nextest", "--version"]),
            ("git", vec!["--version"]),
            ("git", vec!["config", "--null", "--list"]),
            ("git", vec!["rev-parse", "HEAD"]),
            (
                "git",
                vec![
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--",
                    ".",
                    &exclusion,
                ],
            ),
        ] {
            include(&Self::output(program, &arguments)?);
        }
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.sort();
        for (name, value) in environment {
            include(name.as_encoded_bytes());
            include(value.as_encoded_bytes());
        }
        let paths = Self::output(
            "git",
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                ".",
                &exclusion,
            ],
        )?;
        Self::include_sources(&paths, &target, &mut include)?;

        Ok(hash
            .finalize()
            .into_iter()
            .flat_map(|byte| {
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]
            })
            .collect())
    }

    fn execute(&self, hook: &Hook, packages: &[String], target: &Path) -> Result<(), String> {
        let stage = if hook.stages.iter().any(|stage| stage == "manual") {
            "manual"
        } else {
            "pre-commit"
        };
        let filter = packages
            .iter()
            .map(|package| format!("package(={package})"))
            .collect::<Vec<_>>()
            .join(" or ");
        let status = Command::new("prek")
            .args(["run", &hook.id, "--all-files", "--hook-stage", stage])
            .env_remove("SKIP")
            .env_remove("PREK_SKIP")
            .env("CARGO_TARGET_DIR", target)
            .env("AGENTTY_TEST_PACKAGES", packages.join(" "))
            .env("AGENTTY_TEST_FILTER", filter)
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("Validation hook {} failed", hook.id));
        }

        Ok(())
    }

    fn save(&self, target: &Path, ledger: &Ledger) -> Result<(), String> {
        let bytes = serde_json::to_vec(ledger).map_err(|error| error.to_string())?;
        let pending = target.join("agentty-validation.pending");
        fs::write(&pending, bytes).map_err(|error| error.to_string())?;
        fs::rename(pending, target.join("agentty-validation.json"))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "validation_test.rs"]
mod tests;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs, io};

use serde::Deserialize;

/// Crates that may name raw adapters: execution owners and this checker.
const ADAPTER_NAMING_CRATES: [&str; 5] = [
    "ag-agent",
    "ag-contracts",
    "ag-runtime",
    "ag-worker",
    "ag-xtask",
];

/// Agent CLI executables that only `ag-agent` may launch.
const AGENT_EXECUTABLES: [&str; 4] = ["agy", "claude", "codex", "gemini"];

/// Validates that model execution flows only through worker-owned crates.
///
/// # Errors
/// Returns an error when workspace metadata or sources are unreadable, or when
/// a dependency, adapter use, or agent CLI launch bypasses the boundary.
pub(crate) fn run() -> Result<(), String> {
    ExecutionBoundaryCheck {
        workspace: &RealWorkspace {
            cargo: env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        },
    }
    .run(Path::new("crates"))
}

/// Workspace inputs inspected by the execution boundary check.
#[cfg_attr(test, mockall::automock)]
trait Workspace {
    /// Returns every workspace package dependency, including dev and build
    /// dependencies.
    fn dependencies(&self) -> Result<Vec<Dependency>, String>;

    /// Returns non-test Rust sources under each crate's `src` directory.
    fn production_sources(&self, crates: &Path) -> Result<Vec<SourceFile>, String>;
}

/// One package-to-package dependency edge.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Dependency {
    owner: String,
    target: String,
}

/// One production Rust source file, with its path relative to `crates/`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceFile {
    crate_name: String,
    path: PathBuf,
    text: String,
}

/// Execution boundary policy, independent of Cargo and the host filesystem.
struct ExecutionBoundaryCheck<'workspace> {
    workspace: &'workspace dyn Workspace,
}

impl ExecutionBoundaryCheck<'_> {
    fn run(&self, crates: &Path) -> Result<(), String> {
        let mut violations = Vec::new();
        for dependency in self.workspace.dependencies()? {
            if !Self::allowed_dependency(&dependency.owner, &dependency.target) {
                violations.push(format!(
                    "forbidden dependency {} -> {}",
                    dependency.owner, dependency.target
                ));
            }
        }

        let mut adapter_launches = Vec::new();
        for source in self.workspace.production_sources(crates)? {
            let launches = Self::literal_command_launches(&source.text);
            if source.crate_name == "ag-agent" {
                adapter_launches.extend(launches);
                continue;
            }
            if !ADAPTER_NAMING_CRATES.contains(&source.crate_name.as_str()) {
                violations.extend(Self::adapter_usage(&source));
            }
            for executable in launches {
                if AGENT_EXECUTABLES.contains(&executable.as_str()) {
                    violations.push(format!(
                        "{}: launches agent CLI `{executable}` outside ag-agent",
                        source.path.display()
                    ));
                }
            }
        }
        violations.extend(Self::adapter_launch_drift(&adapter_launches));

        if violations.is_empty() {
            return Ok(());
        }

        Err(format!(
            "Execution boundary violations:\n{}",
            violations.join("\n")
        ))
    }

    fn allowed_dependency(owner: &str, target: &str) -> bool {
        match target {
            "ag-worker" => matches!(owner, "agentty" | "ag-store"),
            "ag-agent" => owner == "ag-runtime",
            "ag-runtime" => owner == "ag-worker",
            "ag-harness" => owner == "ag-harness-cli",
            _ => owner != "ag-contracts" || !target.starts_with("ag-") || target == "ag-protocol",
        }
    }

    /// Returns string-literal programs passed to any `Command::new`, including
    /// `std`, `tokio`, and aliased async command types.
    fn literal_command_launches(text: &str) -> Vec<String> {
        const LAUNCH: &str = "Command::new(\"";
        let compact = Self::compact(text);
        let mut launches = Vec::new();
        let mut remaining = compact.as_str();
        while let Some(start) = remaining.find(LAUNCH) {
            remaining = &remaining[start + LAUNCH.len()..];
            if let Some(end) = remaining.find('"') {
                launches.push(remaining[..end].to_string());
            }
        }

        launches
    }

    fn compact(text: &str) -> String {
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    fn adapter_usage(source: &SourceFile) -> Vec<String> {
        let compact = Self::compact(&source.text);
        let adapter_names = [
            "OneShotClient",
            "AgentChannel",
            "create_agent_channel",
            "create_app_server_client",
        ]
        .into_iter()
        .filter(|name| source.text.contains(name));
        // Runtime turn calls are owned by ag-worker. Other crates may invoke
        // the session client, but must not invoke the raw trait method or UFCS.
        let raw_calls = [
            ".run_turn(",
            ".shutdown_session(",
            "ag_worker::run_turn",
            "AgentChannel::run_turn",
            "OneShotClient::submit",
        ]
        .into_iter()
        .filter(|call| compact.contains(call));

        adapter_names
            .chain(raw_calls)
            .map(|forbidden| format!("{}: {forbidden}", source.path.display()))
            .collect()
    }

    /// Keeps `AGENT_EXECUTABLES` aligned with the CLIs `ag-agent` launches.
    fn adapter_launch_drift(adapter_launches: &[String]) -> Vec<String> {
        let unlisted = adapter_launches
            .iter()
            .filter(|launch| !AGENT_EXECUTABLES.contains(&launch.as_str()))
            .map(|launch| format!("ag-agent launches unlisted executable `{launch}`"));
        let missing = AGENT_EXECUTABLES
            .into_iter()
            .filter(|executable| !adapter_launches.iter().any(|launch| launch == executable))
            .map(|executable| format!("ag-agent no longer launches `{executable}`"));

        unlisted.chain(missing).collect()
    }
}

/// Host adapter; boundary decisions remain in `ExecutionBoundaryCheck`.
struct RealWorkspace {
    cargo: OsString,
}

impl RealWorkspace {
    fn parse_dependencies(metadata: &[u8]) -> Result<Vec<Dependency>, String> {
        let metadata: Metadata = serde_json::from_slice(metadata)
            .map_err(|error| format!("Failed to parse cargo metadata: {error}"))?;

        Ok(metadata
            .packages
            .into_iter()
            .flat_map(|package| {
                package
                    .dependencies
                    .into_iter()
                    .map(move |dependency| Dependency {
                        owner: package.name.clone(),
                        target: dependency.name,
                    })
            })
            .collect())
    }

    fn collect_sources(
        crate_name: &str,
        crates: &Path,
        directory: &Path,
        sources: &mut Vec<SourceFile>,
    ) -> Result<(), String> {
        for path in Self::read_dir(directory)? {
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            if name.ends_with("_test") || name == "test_support" {
                continue;
            }
            if path.is_dir() {
                Self::collect_sources(crate_name, crates, &path, sources)?;
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
            sources.push(SourceFile {
                crate_name: crate_name.to_string(),
                path: path.strip_prefix(crates).unwrap_or(&path).to_path_buf(),
                text,
            });
        }

        Ok(())
    }

    fn read_dir(directory: &Path) -> Result<Vec<PathBuf>, String> {
        let mut paths = fs::read_dir(directory)
            .and_then(|entries| {
                entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<io::Result<Vec<_>>>()
            })
            .map_err(|error| format!("Failed to read {}: {error}", directory.display()))?;
        paths.sort();

        Ok(paths)
    }
}

impl Workspace for RealWorkspace {
    fn dependencies(&self) -> Result<Vec<Dependency>, String> {
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

        Self::parse_dependencies(&output.stdout)
    }

    fn production_sources(&self, crates: &Path) -> Result<Vec<SourceFile>, String> {
        let mut sources = Vec::new();
        for crate_path in Self::read_dir(crates)? {
            let source_root = crate_path.join("src");
            if !source_root.is_dir() {
                continue;
            }
            let crate_name = crate_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Self::collect_sources(&crate_name, crates, &source_root, &mut sources)?;
        }

        Ok(sources)
    }
}

/// Subset of `cargo metadata --format-version 1` used by the check.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    dependencies: Vec<PackageDependency>,
    name: String,
}

#[derive(Deserialize)]
struct PackageDependency {
    name: String,
}

#[cfg(test)]
#[path = "check_execution_boundary_test.rs"]
mod tests;

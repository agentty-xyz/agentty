//! Agentty E2E runs in the pinned container image, serialized by a host lock.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::validation::error_text;

/// Workflow whose job container pins the published E2E image digest.
const WORKFLOW: &str = include_str!("../../../.github/workflows/e2e.yml");

/// Feature pages that declare a published GIF.
const FEATURE_PAGES: &str = "docs/site/content/features";

/// Published feature GIFs.
const FEATURE_GIFS: &str = "docs/site/static/features";

/// Label on every container this command starts.
const LABEL: &str = "agentty.e2e-container=1";

/// Runs the Agentty `e2e` target inside the pinned image.
///
/// `TESTTY_GIF_MODE` selects the freshness mode (default `check`),
/// `AGENTTY_TEST_FILTER` optionally narrows the run with a nextest filterset,
/// and `AGENTTY_E2E_IMAGE` replaces the pinned image with a local candidate
/// without pulling it. Runs sharing a cache directory wait for each other.
pub(crate) fn run() -> Result<(), String> {
    let settings = Settings {
        architecture: std::env::consts::ARCH,
        cache: cache_root(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))?,
        candidate: variable("AGENTTY_E2E_IMAGE"),
        filter: variable("AGENTTY_TEST_FILTER"),
        mode: variable("TESTTY_GIF_MODE").unwrap_or_else(|| "check".into()),
        pinned: pinned_image(WORKFLOW)?,
        workspace: std::env::current_dir().map_err(error_text)?,
    };

    Runner { host: &ProcessHost }.run(&settings)
}

/// Resolves the host cache shared by every local container run.
fn cache_root(xdg_cache: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    let base = match (
        xdg_cache.filter(|value| !value.is_empty()),
        home.filter(|value| !value.is_empty()),
    ) {
        (Some(xdg_cache), _) => PathBuf::from(xdg_cache),
        (None, Some(home)) => PathBuf::from(home).join(".cache"),
        (None, None) => return Err("Set HOME or XDG_CACHE_HOME for the E2E container cache".into()),
    };

    Ok(base.join("agentty-e2e"))
}

/// Returns a nonempty UTF-8 environment value.
fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Reads the digest-pinned image from the E2E workflow.
fn pinned_image(workflow: &str) -> Result<String, String> {
    let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(workflow).map_err(error_text)?;

    workflow["jobs"]["test"]["container"]["image"]
        .as_str()
        .filter(|image| image.contains("@sha256:"))
        .map(str::to_string)
        .ok_or_else(|| "The E2E workflow must pin `jobs.test.container.image` by digest".into())
}

/// Inputs resolved from the invoking environment.
struct Settings {
    architecture: &'static str,
    cache: PathBuf,
    candidate: Option<String>,
    filter: Option<String>,
    mode: String,
    pinned: String,
    workspace: PathBuf,
}

trait Guard {}
impl Guard for File {}

/// Process and filesystem boundaries of a container run.
trait Host {
    fn output(&self, program: &str, arguments: &[&str]) -> Result<String, String>;
    fn lock(&self, cache: &Path, target: &str) -> Result<Box<dyn Guard>, String>;
    fn status(&self, program: &str, arguments: &[&str]) -> Result<(), String>;
    fn pages(&self, directory: &Path) -> Result<Vec<String>, String>;
    fn is_nonempty(&self, path: &Path) -> bool;
}

struct Runner<'host> {
    host: &'host dyn Host,
}

impl Runner<'_> {
    fn run(&self, settings: &Settings) -> Result<(), String> {
        let platform = Self::platform(settings.architecture)?;
        // Run as the host user so bind-mounted outputs stay writable.
        let user = format!(
            "{}:{}",
            self.host.output("id", &["-u"])?.trim(),
            self.host.output("id", &["-g"])?.trim()
        );
        // Cargo fingerprints sources by modification time, and every
        // checkout is mounted at the same container path, so a shared build
        // directory could reuse another checkout's artifacts.
        let digest = Sha256::digest(settings.workspace.as_os_str().as_encoded_bytes());
        let key = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        let target = format!("target-{key:08x}");
        // The caches and the Podman machine are shared by every local run,
        // so hold the lock until the container exits.
        let _guard = self.host.lock(&settings.cache, &target)?;
        self.wait_for_orphans()?;
        let image = if let Some(candidate) = &settings.candidate {
            candidate
        } else {
            // An explicit platform fails on a missing variant instead of
            // emulating the other architecture.
            self.host.status(
                "podman",
                &["pull", "--platform", platform, &settings.pinned],
            )?;
            &settings.pinned
        };
        let workspace = format!(
            "type=bind,source={},target=/workspace",
            settings.workspace.display()
        );
        let cache = format!(
            "type=bind,source={},target=/cache",
            settings.cache.display()
        );
        let target_env = format!("CARGO_TARGET_DIR=/cache/{target}");
        let mode = format!("TESTTY_GIF_MODE={}", settings.mode);
        let mut arguments = vec![
            "run",
            "--rm",
            "--pull=never",
            "--label",
            LABEL,
            "--platform",
            platform,
            "--user",
            &user,
            "--mount",
            &workspace,
            "--mount",
            &cache,
        ];
        // One build job without debug symbols bounds linker memory in small
        // Podman machines.
        for variable in [
            "HOME=/cache/home",
            "CARGO_HOME=/cache/cargo",
            "PREK_HOME=/cache/prek",
            &target_env,
            "CARGO_BUILD_JOBS=1",
            "CARGO_PROFILE_DEV_DEBUG=0",
            &mode,
        ] {
            arguments.extend(["--env", variable]);
        }
        arguments.extend([
            image,
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--profile",
            "ci",
            "-p",
            "agentty",
            "--test",
            "e2e",
        ]);
        if let Some(filter) = &settings.filter {
            arguments.extend(["-E", filter]);
        }
        self.host.status("podman", &arguments)?;
        // VHS can exit successfully without finalizing a usable GIF.
        let gifs = settings.workspace.join(FEATURE_GIFS);
        let pages = self.host.pages(&settings.workspace.join(FEATURE_PAGES))?;
        let missing = pages
            .iter()
            .filter_map(|page| Self::declared_gif(page))
            .filter(|gif| !self.host.is_nonempty(&gifs.join(gif)))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "Feature GIFs are missing or empty: {}",
                missing.join(", ")
            ));
        }

        Ok(())
    }

    /// Maps the host architecture to its native image variant.
    fn platform(architecture: &str) -> Result<&'static str, String> {
        match architecture {
            "x86_64" => Ok("linux/amd64"),
            "aarch64" => Ok("linux/arm64"),
            other => Err(format!("Unsupported E2E container architecture: {other}")),
        }
    }

    /// Waits for labeled containers that a killed invocation left running
    /// after releasing the lock. They may belong to another session, so they
    /// are never stopped.
    fn wait_for_orphans(&self) -> Result<(), String> {
        let filter = format!("label={LABEL}");
        let running = self
            .host
            .output("podman", &["ps", "--quiet", "--filter", &filter])?;
        let orphans = running.split_whitespace().collect::<Vec<_>>();
        if orphans.is_empty() {
            return Ok(());
        }
        writeln!(
            io::stderr(),
            "Waiting for an earlier Agentty E2E container to exit: {}",
            orphans.join(" ")
        )
        .map_err(error_text)?;
        let mut arguments = vec!["wait", "--ignore"];
        arguments.extend(orphans);

        self.host.status("podman", &arguments)
    }

    /// Returns the GIF file name a feature page declares.
    fn declared_gif(page: &str) -> Option<&str> {
        page.lines()
            .find_map(|line| line.strip_prefix("gif = \"")?.strip_suffix('"'))
    }
}

struct ProcessHost;

impl Host for ProcessHost {
    fn output(&self, program: &str, arguments: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|error| format!("Could not execute {program}: {error}"))?;
        if !output.status.success() {
            return Err(format!("{program} failed with {}", output.status));
        }

        String::from_utf8(output.stdout).map_err(error_text)
    }

    fn lock(&self, cache: &Path, target: &str) -> Result<Box<dyn Guard>, String> {
        for directory in ["home", "cargo", "prek", target] {
            fs::create_dir_all(cache.join(directory)).map_err(error_text)?;
        }
        let path = cache.join("container.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(error_text)?;
        if file.try_lock().is_err() {
            writeln!(
                io::stderr(),
                "Waiting for another Agentty E2E container run to release {}",
                path.display()
            )
            .map_err(error_text)?;
            // A genuine locking failure surfaces from the blocking attempt.
            file.lock().map_err(error_text)?;
        }

        Ok(Box::new(file))
    }

    fn status(&self, program: &str, arguments: &[&str]) -> Result<(), String> {
        let status = Command::new(program)
            .args(arguments)
            .status()
            .map_err(|error| format!("Could not execute {program}: {error}"))?;
        if !status.success() {
            let command = arguments.first().copied().unwrap_or_default();

            return Err(format!("{program} {command} failed with {status}"));
        }

        Ok(())
    }

    fn pages(&self, directory: &Path) -> Result<Vec<String>, String> {
        let mut pages = Vec::new();
        for entry in fs::read_dir(directory).map_err(error_text)? {
            let path = entry.map_err(error_text)?.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                pages.push(fs::read_to_string(path).map_err(error_text)?);
            }
        }

        Ok(pages)
    }

    fn is_nonempty(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
    }
}

#[cfg(test)]
#[path = "e2e_container_test.rs"]
mod tests;

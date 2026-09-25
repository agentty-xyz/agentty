//! Shared fixtures for native sandbox integration tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ag_harness::bash::{BashConfig, CommandOutcome, UnsandboxedExecutor};
use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::SqliteStore;
use ag_harness::tool::ToolCall;
use ag_harness::{
    Harness, Model, ModelError, OutputSchema, Repository, Tool, ToolPolicy, TurnLimits, TurnOptions,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use super::coverage::{self, Coverage};

pub(super) struct ShellModel;

#[async_trait]
impl Model for ShellModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let response =
            if let Some(ModelMessage::ToolResult { content, .. }) = request.messages().last() {
                ModelResponse::Output(serde_json::from_str(content).map_err(ModelError::request)?)
            } else {
                ModelResponse::ToolCall(ToolCall::from_json(
                    "command".into(),
                    "bash",
                    &json!({"command": request.prompt()}).to_string(),
                    None,
                )?)
            };

        Ok(ModelCompletion::from_response(response))
    }
}

/// Executors covered by the shared execution-conformance behavior. Native
/// enforcement suites remain the sandboxed executor's qualification.
#[derive(Clone, Copy, Debug)]
pub(super) enum Selected {
    Native,
    Unsandboxed,
}

pub(super) const CONFORMANCE_EXECUTORS: [Selected; 2] = [Selected::Native, Selected::Unsandboxed];

pub(super) struct Workspace {
    coverage: Option<Coverage>,
    directory: TempDir,
}

impl Workspace {
    pub(super) fn new() -> Self {
        let directory = tempfile::tempdir().expect("workspace");
        std::fs::create_dir(directory.path().join(".git")).expect("metadata");
        std::fs::write(directory.path().join(".git/config"), "protected").expect("config");
        std::fs::write(directory.path().join("input"), "input-value").expect("input");
        std::fs::create_dir(directory.path().join("output")).expect("output");

        let coverage = Coverage::new(&directory.path().canonicalize().expect("workspace"));

        Self {
            coverage,
            directory,
        }
    }

    pub(super) fn path(&self) -> PathBuf {
        self.directory
            .path()
            .canonicalize()
            .expect("canonical workspace")
    }

    pub(super) fn harness(&self) -> Harness {
        Harness::new(ShellModel)
            .repository(Repository::new(self.path(), "/usr/bin/git").expect("repository"))
            .execution_identity(ExecutionIdentity::new("native-shell", "1").expect("identity"))
    }

    pub(super) fn options(&self, timeout: Duration, capture: usize) -> TurnOptions {
        self.options_with_runtime(&[], timeout, capture)
    }

    pub(super) fn options_with_runtime(
        &self,
        extra: &[PathBuf],
        timeout: Duration,
        capture: usize,
    ) -> TurnOptions {
        let launcher = self.launcher();
        let configuration = BashConfig::new(
            launcher,
            "/bin/bash".into(),
            "native-runtime-v1".into(),
            timeout,
            capture,
        )
        .expect("configuration")
        .with_host_information()
        .with_write("output".into())
        .expect("write grant");

        TurnOptions::new(
            schema(),
            ToolPolicy::default().allow(Tool::Bash),
            TurnLimits::default(),
        )
        .with_bash(with_runtime(configuration, extra))
    }

    pub(super) fn executor_options(
        &self,
        selected: Selected,
        timeout: Duration,
        capture: usize,
    ) -> TurnOptions {
        match selected {
            Selected::Native => self.options(timeout, capture),
            Selected::Unsandboxed => {
                let configuration = BashConfig::for_executor(
                    Arc::new(UnsandboxedExecutor::without_isolation()),
                    "/bin/bash".into(),
                    "unsandboxed-runtime-v1".into(),
                    timeout,
                    capture,
                )
                .expect("executor configuration")
                .with_host_information()
                .with_write("output".into())
                .expect("write grant");

                TurnOptions::new(
                    schema(),
                    ToolPolicy::default().allow(Tool::Bash),
                    TurnLimits::default(),
                )
                .with_bash(configuration)
            }
        }
    }

    pub(super) fn launcher(&self) -> PathBuf {
        self.coverage
            .as_ref()
            .map_or_else(coverage::launcher, Coverage::launcher)
    }

    /// With the shared `output` write grant, both platforms persist the outer
    /// launcher profile and the inner profile written inside the sandbox.
    pub(super) fn assert_launcher_profiles(&self) {
        self.assert_phase_profiles(&["outer-", "inner-"]);
    }

    pub(super) fn assert_phase_profiles(&self, phases: &[&str]) {
        if let Some(coverage) = &self.coverage {
            let profiles = coverage.profiles();
            for phase in phases {
                assert!(
                    profiles.iter().any(|path| path
                        .file_name()
                        .expect("profile")
                        .to_string_lossy()
                        .starts_with(phase)
                        && path.metadata().expect("profile metadata").len() > 0),
                    "missing {phase} launcher profile: {profiles:?}"
                );
            }
        }
    }

    pub(super) async fn run(&self, command: &str) -> CommandOutcome {
        let output = self
            .harness()
            .turn(command, self.options(Duration::from_secs(10), 1024))
            .await
            .expect("turn");

        serde_json::from_value(output.into_output()).expect("command result")
    }
}

pub(super) fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object"})).expect("schema")
}

/// Grants the trusted fixture runtime without scanning the runner's entire
/// system library and executable trees inside each command's deadline.
pub(super) fn with_runtime(mut configuration: BashConfig, extra: &[PathBuf]) -> BashConfig {
    #[cfg(target_os = "linux")]
    {
        configuration = configuration
            .with_linux_bubblewrap("/usr/bin/bwrap".into())
            .expect("Bubblewrap");
    }
    for path in runtime_reads(extra) {
        configuration = configuration.with_read(path).expect("runtime grant");
    }

    configuration
}

/// The read closure required to run the trusted shell and fixtures inside the
/// sandbox, shared by policy construction and wire-level launcher tests.
pub(super) fn runtime_reads(extra: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::from([coverage::launcher()]);
    paths.extend(extra.iter().cloned());
    #[cfg(target_os = "linux")]
    {
        paths.extend(["/bin/bash", "/bin/cat", "/bin/sleep"].map(PathBuf::from));
        for executable in paths.clone() {
            // Host binaries, fixtures compiled by this suite, and fixture
            // shell scripts reach ldd; scripts carry no dynamic dependencies.
            let output = std::process::Command::new("/usr/bin/ldd")
                .arg(&executable)
                .env_clear()
                .env("LC_ALL", "C")
                .output()
                .expect("inspect trusted runtime dependencies");
            let libraries = String::from_utf8(output.stdout).expect("dependency paths");
            let diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
            if libraries.contains("not a dynamic executable")
                || diagnostics.contains("not a dynamic executable")
            {
                continue;
            }
            assert!(
                output.status.success() && !libraries.contains("not found"),
                "ldd {}: {libraries}{diagnostics}",
                executable.display(),
            );
            paths.extend(
                libraries
                    .split_whitespace()
                    .filter(|path| path.starts_with('/'))
                    .map(PathBuf::from),
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    paths.insert("/bin".into());

    paths
}

pub(super) struct NativeFixture {
    directory: TempDir,
}

impl NativeFixture {
    pub(super) fn build() -> Self {
        let directory = tempfile::tempdir().expect("native fixture");
        let source = directory.path().join("fixture.c");
        std::fs::write(&source, include_str!("../sandbox_fixture.c")).expect("source");
        let compiler = std::process::Command::new("/usr/bin/cc")
            .arg(&source)
            .arg("-o")
            .arg(directory.path().join("fixture"))
            .env("TMPDIR", directory.path())
            .output()
            .expect("native compiler");
        assert!(
            compiler.status.success(),
            "{}",
            String::from_utf8_lossy(&compiler.stderr)
        );

        Self { directory }
    }

    pub(super) fn executable(&self) -> PathBuf {
        self.directory
            .path()
            .join("fixture")
            .canonicalize()
            .expect("fixture executable")
    }

    pub(super) fn library(&self) -> PathBuf {
        let library = self.directory.path().join("loader.so");
        let compiled = std::process::Command::new("/usr/bin/cc")
            .arg(self.directory.path().join("fixture.c"))
            .args(["-DLOADER_LIBRARY", "-fPIC"])
            .arg(if cfg!(target_os = "macos") {
                "-dynamiclib"
            } else {
                "-shared"
            })
            .arg("-o")
            .arg(&library)
            .env("TMPDIR", self.directory.path())
            .output()
            .expect("compile loader fixture");
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );

        library.canonicalize().expect("compiled library")
    }

    pub(super) fn options(&self, workspace: &Workspace) -> TurnOptions {
        let options = workspace.options(Duration::from_secs(10), 1024);
        let configuration = options
            .bash()
            .expect("Bash config")
            .clone()
            .with_read(self.executable())
            .expect("fixture grant");

        options.with_bash(configuration)
    }
}

/// Kills one detached macOS descendant when the test scope ends.
///
/// Linux callers never observe host PIDs: their markers contain namespace
/// PIDs, and the retained supervisor owns that cleanup.
#[cfg(target_os = "macos")]
pub(super) struct Descendant(pub(super) i32);

#[cfg(target_os = "macos")]
impl Drop for Descendant {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.0) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

/// Waits for a marker written through a write grant or the unsandboxed
/// executor.
pub(super) async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("native marker");
}

pub(super) async fn fault_store(path: &Path) -> (Arc<SqliteStore>, sqlx::SqlitePool) {
    let store = Arc::new(SqliteStore::open(path).await.expect("SQLite"));
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path))
            .await
            .expect("fault injection connection");

    (store, pool)
}

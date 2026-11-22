//! Bash consumes the shared supervisor over the selected executor and
//! retains its journal and control.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[cfg(feature = "native-sandbox")]
use super::contract::BashExecutor;
use super::contract::{
    Enforcement, ExecutionCommand, ExecutionControl, ExecutionError, ExecutionPolicy,
    ExecutionResult, Executor, Grants, Limits, MainExit, ReadScope, Termination,
};
#[cfg(feature = "native-sandbox")]
use super::native::Native;
use super::supervisor::{Monotonic, Supervisor};
use crate::TurnError;
use crate::bash::{BashArguments, BashConfig, BashError, Capability};
use crate::command_journal::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination,
};
use crate::command_settlement::Commands;
use crate::reservation::WriteJournal;

/// `PATH` excluded commands search when the host grants none, instead of
/// Bash's compiled default, which ends in the working directory.
const DEFAULT_EXCLUDED_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Variables of the harness process that excluded commands inherit, besides
/// `LC_*`: identity, locale, and terminal, which the host controls.
const EXCLUDED_HOST_VARIABLES: [&str; 5] = ["HOME", "USER", "LOGNAME", "LANG", "TERM"];

pub(crate) struct BashTool {
    cleanup_scope: CommandCleanupScope,
    commands: Commands,
    configuration: BashConfig,
    /// Runs the commands the configuration excludes from the sandbox
    /// without isolation; `None` when it excludes none or isolates nothing.
    excluded: Option<Box<BashTool>>,
    executor: Arc<dyn Executor>,
    /// Host read grants followed by the system directories present on this
    /// host when the configuration grants them, resolved once per turn.
    external_reads: Vec<PathBuf>,
    journal: Option<WriteJournal>,
    root: PathBuf,
}

impl BashTool {
    pub(crate) fn new(
        configuration: BashConfig,
        root: PathBuf,
        commands: Commands,
        journal: Option<WriteJournal>,
    ) -> Result<Self, BashError> {
        if !configuration.snapshot.host_information {
            return Err(BashError::Unavailable);
        }
        #[cfg(feature = "native-sandbox")]
        let selected: Arc<dyn BashExecutor> = match &configuration.executor {
            Some(executor) => Arc::clone(executor),
            None => Arc::new(Native {
                configuration: configuration.clone(),
            }),
        };
        // Without the native sandbox, `BashConfig::for_executor` is the only
        // constructor, so the executor is always host-selected.
        #[cfg(not(feature = "native-sandbox"))]
        let selected = configuration
            .executor
            .clone()
            .ok_or(BashError::Unavailable)?;
        let cleanup_scope = selected.cleanup_scope();
        let supervisor =
            Supervisor::new(selected, Arc::new(Monotonic)).map_err(|_| BashError::Unavailable)?;
        let mut external_reads = configuration.snapshot.external_reads.clone();
        if configuration.snapshot.grants(Capability::SystemReads) {
            external_reads.extend(super::system::reads());
        }
        let excluded = (!configuration.snapshot.excluded_commands.is_empty()
            && configuration.enforcement() != Enforcement::None)
            .then(|| {
                Self::excluded_configuration(&configuration, &root).and_then(|excluded| {
                    Self::new(excluded, root.clone(), commands.clone(), journal.clone())
                })
            })
            .transpose()?
            .map(Box::new);

        Ok(Self {
            cleanup_scope,
            commands,
            configuration,
            excluded,
            executor: Arc::new(supervisor),
            external_reads,
            journal,
            root,
        })
    }

    /// Runs `arguments`: outside the sandbox, through its resolved
    /// executable, when the configuration excludes its command and
    /// `unsandboxed` says the approval handler allowed that.
    pub(crate) async fn execute(
        &self,
        arguments: &BashArguments,
        call_id: &str,
        unsandboxed: bool,
    ) -> Result<CommandOutcome, TurnError> {
        if let Some((excluded, executable, words)) = self.excluded_launch(arguments, unsandboxed) {
            let words = words.into_iter().map(OsString::from).collect();

            return Box::pin(excluded.run(executable.to_path_buf(), words, arguments, call_id))
                .await;
        }
        let words = vec![
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            arguments.command().into(),
        ];

        // Commands are supervised in a boxed future, keeping each turn's
        // future small.
        Box::pin(self.run(
            self.configuration.snapshot.bash.clone(),
            words,
            arguments,
            call_id,
        ))
        .await
    }

    /// Returns the exact argument vector `arguments` runs with outside the
    /// sandbox once approved, the resolved executable first, or `None` when
    /// they stay inside it.
    pub(crate) fn excluded_command(&self, arguments: &BashArguments) -> Option<Vec<String>> {
        self.excluded.as_ref()?;
        let (executable, words) = self.configuration.excluded_program(arguments.command())?;

        Some(
            std::iter::once(executable.to_string_lossy().into_owned())
                .chain(words)
                .collect(),
        )
    }

    /// Returns whether `arguments` runs under an executor that enforces
    /// isolation, so a failure can come from the sandbox.
    pub(crate) fn enforces(&self, arguments: &BashArguments, unsandboxed: bool) -> bool {
        self.configuration.enforcement() != Enforcement::None
            && self.excluded_launch(arguments, unsandboxed).is_none()
    }

    /// Returns the unsandboxed tool, the resolved executable, and the words
    /// it runs with when `arguments` leave the sandbox.
    fn excluded_launch(
        &self,
        arguments: &BashArguments,
        unsandboxed: bool,
    ) -> Option<(&Self, &Path, Vec<String>)> {
        let excluded = self.excluded.as_deref().filter(|_| unsandboxed)?;
        let (executable, words) = self.configuration.excluded_program(arguments.command())?;

        Some((excluded, executable, words))
    }

    /// Returns the configuration excluded commands run with: no isolation,
    /// no temporary directory, and only a host-controlled environment: a
    /// `PATH` without relative entries or directories commands can write,
    /// the identity and locale variables of the harness process, and the
    /// values passed for excluded commands.
    ///
    /// # Errors
    /// Returns [`BashError::InvalidPolicy`] when an excluded executable or
    /// an environment value lies inside the workspace, the temporary
    /// directory, or a writable root, where a command could plant what it
    /// names.
    fn excluded_configuration(
        configuration: &BashConfig,
        root: &Path,
    ) -> Result<BashConfig, BashError> {
        let snapshot = &configuration.snapshot;
        let writable = std::iter::once(root.to_path_buf())
            .chain(snapshot.temporary_directory.iter().cloned())
            .chain(snapshot.writable_roots.iter().cloned())
            .flat_map(|path| {
                let resolved = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

                [path, resolved]
            })
            .collect::<Vec<_>>();
        let inside_writable = |path: &Path| writable.iter().any(|area| path.starts_with(area));
        if snapshot
            .excluded_commands
            .values()
            .any(|executable| inside_writable(executable))
        {
            return Err(BashError::InvalidPolicy);
        }
        let host_path = configuration
            .environment
            .get("PATH")
            .map_or(DEFAULT_EXCLUDED_PATH, String::as_str);
        // A directory that does not exist yet could be created later inside
        // a writable place, so only existing ones are kept.
        let path = std::env::join_paths(std::env::split_paths(host_path).filter(|directory| {
            directory.is_absolute()
                && !inside_writable(directory)
                && std::fs::canonicalize(directory)
                    .is_ok_and(|resolved| resolved.is_dir() && !inside_writable(&resolved))
        }))
        .unwrap_or_default();
        // `std::env::vars` panics on a value that is not Unicode; skip those.
        let mut environment = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(name, _)| {
                EXCLUDED_HOST_VARIABLES.contains(&name.as_str()) || name.starts_with("LC_")
            })
            .chain(configuration.excluded_environment.clone())
            .collect::<std::collections::BTreeMap<_, _>>();
        // A value naming a path commands can write would let them plant
        // what the unsandboxed program reads.
        if environment.values().any(|value| {
            std::env::split_paths(value).any(|entry| {
                entry.is_absolute()
                    && (inside_writable(&entry) || inside_writable(&resolve_nearest(&entry)))
            })
        }) {
            return Err(BashError::InvalidPolicy);
        }
        environment.insert("PATH".into(), path.to_string_lossy().into_owned());
        let mut excluded = configuration.unsandboxed();
        excluded.environment = environment;
        excluded.snapshot.environment_names = excluded.environment.keys().cloned().collect();
        excluded.snapshot.temporary_directory = None;

        Ok(excluded)
    }

    /// Runs `executable` with `words`, recording `arguments`' command.
    async fn run(
        &self,
        executable: PathBuf,
        words: Vec<OsString>,
        arguments: &BashArguments,
        call_id: &str,
    ) -> Result<CommandOutcome, TurnError> {
        let configuration = &self.configuration.snapshot;
        let grants = Grants {
            environment: self
                .configuration
                .environment
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                .chain(
                    configuration
                        .temporary_directory
                        .iter()
                        .map(|path| (OsString::from("TMPDIR"), path.clone().into_os_string())),
                )
                .collect(),
            external_reads: self.external_reads.clone(),
            git_writes: configuration.grants(Capability::GitWrites),
            host_information: configuration.host_information,
            network: configuration.grants(Capability::Network),
            hidden_paths: configuration.hidden_paths.clone(),
            read_scope: if configuration.grants(Capability::FullDiskRead) {
                ReadScope::FullDisk
            } else {
                ReadScope::Granted
            },
            temporary_directory: configuration.temporary_directory.clone(),
            unix_sockets: configuration.unix_sockets.clone(),
            workspace_writes: configuration.workspace_writes.clone(),
            writable_roots: configuration.writable_roots.clone(),
        };
        let policy = ExecutionPolicy::new(self.root.clone(), vec![self.root.join(".git")], grants)
            .map_err(|_| BashError::InvalidPolicy)?;
        let command = ExecutionCommand::new(executable, words, ".".into())
            .map_err(|_| BashError::InvalidArguments)?;
        let limits = Limits::new(
            Instant::now(),
            configuration.timeout,
            configuration.capture_bytes,
        )
        .map_err(|_| BashError::InvalidPolicy)?;
        let prepared = self
            .executor
            .prepare(command, policy, limits)
            .map_err(|_| BashError::Unavailable)?;
        let control: Arc<dyn ExecutionControl> = prepared.control.into();
        let _cancellation = Cancel(Arc::clone(&control));
        let operation = self.commands.register(control, self.journal.clone());
        let lease = self.commands.retain();
        let intent = CommandIntent {
            call_id: call_id.into(),
            command: arguments.command().into(),
            policy: self.configuration.fingerprint(),
            workspace: self.root.clone(),
        };
        let cleanup_scope = self.cleanup_scope;
        let journal = self.journal.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let id = match journal.as_ref() {
                Some(journal) => Some(journal.command_intent(&intent).await.map_err(|source| {
                    TurnError::CommandJournal {
                        outcome: None,
                        source: Box::new(source),
                    }
                })?),
                None => None,
            };
            operation.admitted(id);
            let result = prepared.execution.run().await;
            let outcome = command_outcome(&result, cleanup_scope);
            operation.observed(outcome.clone());
            operation
                .persist()
                .await
                .map_err(|source| TurnError::CommandJournal {
                    outcome: Some(Box::new(outcome.clone())),
                    source: Box::new(source),
                })?;
            if outcome.cleanup_failed || outcome.execution_failure.is_some() {
                return Err(TurnError::CommandFailed {
                    outcome: Box::new(outcome),
                });
            }

            Ok(outcome)
        })
        .await
        .map_err(|_| BashError::Execution)?
    }
}

/// Returns `path` with its nearest existing ancestor resolved, links, `..`,
/// and the file system's own letter case included, followed by the rest of
/// it, so a path that does not exist yet still compares as where it would
/// be created.
fn resolve_nearest(path: &Path) -> PathBuf {
    path.ancestors()
        .find_map(|ancestor| {
            let base = std::fs::canonicalize(ancestor).ok()?;

            Some(
                path.strip_prefix(ancestor)
                    .unwrap_or(Path::new(""))
                    .components()
                    .fold(base, |resolved, part| resolved.join(part)),
            )
        })
        // `/` always resolves, so an absolute path never reaches this.
        .unwrap_or_default()
}

struct Cancel(Arc<dyn ExecutionControl>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn command_outcome(result: &ExecutionResult, cleanup_scope: CommandCleanupScope) -> CommandOutcome {
    let stdout = result.output.stdout();
    let stderr = result.output.stderr();

    CommandOutcome {
        cleanup_failed: result.cleanup_failure.is_some(),
        cleanup_scope,
        execution_failure: result.execution_failure.map(|error| match error {
            ExecutionError::Unsupported => BashError::Unavailable,
            ExecutionError::Setup | ExecutionError::Process | ExecutionError::Supervision => {
                BashError::Execution
            }
            ExecutionError::Cleanup | ExecutionError::CleanupUnconfirmed => BashError::Cleanup,
        }),
        exit_code: if let MainExit::Code(code) = result.main_exit {
            Some(code)
        } else {
            None
        },
        signal: if let MainExit::Signal(signal) = result.main_exit {
            Some(signal)
        } else {
            None
        },
        stderr: String::from_utf8_lossy(&stderr.head).into_owned(),
        stderr_omitted_bytes: stderr.omitted_bytes,
        stderr_tail: String::from_utf8_lossy(&stderr.tail).into_owned(),
        stdout: String::from_utf8_lossy(&stdout.head).into_owned(),
        stdout_omitted_bytes: stdout.omitted_bytes,
        stdout_tail: String::from_utf8_lossy(&stdout.tail).into_owned(),
        termination: match result.termination {
            Termination::Completed => CommandTermination::Completed,
            Termination::Deadline => CommandTermination::Deadline,
            Termination::Cancelled => CommandTermination::Cancelled,
            Termination::Failed => CommandTermination::Failed,
        },
        truncated: result.output.truncated(),
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;

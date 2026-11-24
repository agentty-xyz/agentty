//! Repository-level git command execution and metadata lookups, such as the
//! remote URL, main repository root, and main checkout working tree.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use ag_telemetry::{Context, Outcome, Span, TraceContextExt};
use tokio::process::Command as AsyncCommand;
use tokio::task::{JoinError, spawn_blocking};
use tokio::time;

use super::error::GitError;

/// Maximum time one local asynchronous git subprocess may run.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time one remote-transfer git subprocess may run.
///
/// Pushes, fetches, pulls, and remote lookups depend on network speed,
/// transfer size, credential helpers, and remote hooks, so they need a wider
/// bound than local commands.
const NETWORK_GIT_COMMAND_TIMEOUT: Duration = Duration::from_mins(5);

/// Git subcommands that talk to a remote and use
/// [`NETWORK_GIT_COMMAND_TIMEOUT`].
const NETWORK_GIT_SUBCOMMANDS: [&str; 4] = ["fetch", "ls-remote", "pull", "push"];

/// Known git subcommands exported verbatim as `git.subcommand` on
/// `git.command` spans. Any other subcommand, such as an alias, is exported
/// as `other`, keeping the attribute bounded.
const TRACED_GIT_SUBCOMMANDS: [&str; 56] = [
    "add",
    "am",
    "apply",
    "branch",
    "cat-file",
    "check-ignore",
    "checkout",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "commit-tree",
    "config",
    "describe",
    "diff",
    "diff-index",
    "diff-tree",
    "fetch",
    "for-each-ref",
    "gc",
    "grep",
    "hash-object",
    "hook",
    "init",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "merge",
    "merge-base",
    "merge-tree",
    "mv",
    "pull",
    "push",
    "read-tree",
    "rebase",
    "remote",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "show",
    "show-ref",
    "stash",
    "status",
    "switch",
    "symbolic-ref",
    "tag",
    "update-index",
    "update-ref",
    "version",
    "worktree",
    "write-tree",
];

/// One asynchronous git invocation with owned process inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AsyncGitCommand {
    /// Git arguments excluding the executable name.
    pub(super) arguments: Vec<String>,
    /// Environment overrides applied after noninteractive defaults.
    pub(super) environment: Vec<(String, String)>,
    /// Repository or worktree used as the process working directory.
    pub(super) repo_path: PathBuf,
}

impl AsyncGitCommand {
    /// Builds one git command without environment overrides.
    pub(super) fn new(repo_path: PathBuf, arguments: Vec<String>) -> Self {
        Self {
            arguments,
            environment: Vec::new(),
            repo_path,
        }
    }

    /// Applies environment overrides to this command.
    pub(super) fn with_environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = environment;

        self
    }

    /// Returns the runtime bound for this command: remote-transfer
    /// subcommands get [`NETWORK_GIT_COMMAND_TIMEOUT`], and every other
    /// command gets [`GIT_COMMAND_TIMEOUT`].
    fn timeout(&self) -> Duration {
        let is_network_command = self
            .subcommand()
            .is_some_and(|subcommand| NETWORK_GIT_SUBCOMMANDS.contains(&subcommand));

        if is_network_command {
            NETWORK_GIT_COMMAND_TIMEOUT
        } else {
            GIT_COMMAND_TIMEOUT
        }
    }

    /// Returns the git subcommand. See [`git_subcommand`].
    fn subcommand(&self) -> Option<&str> {
        git_subcommand(self.arguments.iter().map(String::as_str))
    }
}

/// Returns the git subcommand, skipping leading `-c <name>=<value>`
/// configuration overrides and other global options.
fn git_subcommand<'argument>(
    arguments: impl IntoIterator<Item = &'argument str>,
) -> Option<&'argument str> {
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "-c" {
            arguments.next();

            continue;
        }

        if !argument.starts_with('-') {
            return Some(argument);
        }
    }

    None
}

/// Captured output from one asynchronous git subprocess.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AsyncGitCommandOutput {
    /// Process exit code, or `None` when terminated by a signal.
    pub(super) exit_code: Option<i32>,
    /// Captured standard error bytes.
    pub(super) stderr: Vec<u8>,
    /// Captured standard output bytes.
    pub(super) stdout: Vec<u8>,
}

impl AsyncGitCommandOutput {
    /// Returns whether the process exited successfully.
    pub(super) fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Mockable boundary for cancellable asynchronous git subprocesses.
#[cfg_attr(test, mockall::automock)]
pub(super) trait AsyncGitCommandRunner: Send + Sync {
    /// Runs one owned git command and captures its output.
    fn run(
        &self,
        command: AsyncGitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<AsyncGitCommandOutput, GitError>> + Send>>;
}

/// Production asynchronous git runner backed by `tokio::process`.
pub(super) struct ProcessAsyncGitCommandRunner;

impl AsyncGitCommandRunner for ProcessAsyncGitCommandRunner {
    fn run(
        &self,
        command: AsyncGitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<AsyncGitCommandOutput, GitError>> + Send>> {
        Box::pin(async move {
            let timeout = command.timeout();

            run_git_command_with_timeout(command, timeout).await
        })
    }
}

/// Returns the origin repository URL normalized to HTTPS form when possible.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(url) on success, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if the remote URL cannot be read via `git remote get-url`.
pub(crate) async fn repo_url(repo_path: PathBuf) -> Result<String, GitError> {
    let remote = run_git_command(
        repo_path,
        vec![
            "remote".to_string(),
            "get-url".to_string(),
            "origin".to_string(),
        ],
        "Git remote get-url failed".to_string(),
    )
    .await?;

    Ok(normalize_repo_url(remote.trim()))
}

/// Resolves the main repository root for a repository or linked worktree.
///
/// Uses `git rev-parse --git-dir --git-common-dir`, normalizes both paths to
/// absolute form, detects whether `repo_path` is a linked worktree (`git-dir`
/// differs from `git-common-dir`), and then returns the shared repository
/// root.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(path) containing the main repository root, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub(crate) async fn main_repo_root(repo_path: PathBuf) -> Result<PathBuf, GitError> {
    match resolve_shared_repo(&repo_path).await? {
        SharedRepo::Working(path) | SharedRepo::Bare(path) => Ok(path),
    }
}

/// Resolves the main working checkout for a repository or linked worktree.
///
/// Returns `Some(path)` with the main working checkout root for non-bare shared
/// repositories, and `None` when the shared repository is bare because a bare
/// repository has no main working checkout.
///
/// # Arguments
/// * `repo_path` - Path to a git repository or worktree
///
/// # Returns
/// Ok(Some(path)) with the main working checkout, Ok(None) when the shared
/// repository is bare, Err([`GitError`]) on failure.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub(crate) async fn main_checkout_working_tree(
    repo_path: PathBuf,
) -> Result<Option<PathBuf>, GitError> {
    spawn_blocking_in_context(move || main_checkout_working_tree_sync(&repo_path)).await?
}

/// Resolves the main working checkout for `repo_path` in synchronous code.
///
/// Returns `Some(path)` for non-bare shared repositories and `None` for bare
/// shared repositories, which have no main working checkout.
pub(super) fn main_checkout_working_tree_sync(
    repo_path: &Path,
) -> Result<Option<PathBuf>, GitError> {
    match resolve_shared_repo_sync(repo_path)? {
        SharedRepo::Working(path) => Ok(Some(path)),
        SharedRepo::Bare(_) => Ok(None),
    }
}

/// Classifies the shared repository backing a worktree.
pub(super) enum SharedRepo {
    /// Non-bare shared repository. The path is the main working checkout root.
    Working(PathBuf),
    /// Bare shared repository. The path is the bare common git directory, which
    /// has no main working checkout but is a valid administrative working
    /// directory for `git worktree` and branch commands.
    Bare(PathBuf),
}

/// Resolves the shared repository backing `repo_path`.
///
/// Reads the git and common git directories, detects whether the shared
/// repository is bare by running `git rev-parse --is-bare-repository` inside
/// the common git directory, and returns [`SharedRepo::Bare`] with the bare
/// common git directory when bare. Otherwise returns [`SharedRepo::Working`]
/// with the main working checkout root, matching the async administrative-root
/// resolution for non-bare layouts.
///
/// # Errors
/// Returns an error if git metadata cannot be queried from `repo_path`.
pub(super) fn resolve_shared_repo_sync(repo_path: &Path) -> Result<SharedRepo, GitError> {
    let (git_dir, git_common_dir) = git_directory_paths(repo_path)?;

    // Probe the common git directory directly by running git there, so the
    // shared repository's bareness is detected even from a linked worktree
    // (where `--is-bare-repository` reports the worktree, not the shared repo).
    // Passing the directory through `current_dir` keeps non-UTF-8 paths intact,
    // unlike converting it to a `--git-dir` string argument.
    let is_bare = run_git_command_sync(
        &git_common_dir,
        &["rev-parse", "--is-bare-repository"],
        "Git rev-parse --is-bare-repository failed",
    )?;
    if is_bare.trim() == "true" {
        return Ok(SharedRepo::Bare(git_common_dir));
    }

    if git_dir == git_common_dir {
        return Ok(SharedRepo::Working(repo_root_from_git_dir(
            repo_path, &git_dir,
        )?));
    }

    Ok(SharedRepo::Working(repo_root_from_git_dir(
        repo_path,
        &git_common_dir,
    )?))
}

/// Resolves the git directory path for a repository root or worktree root.
pub(super) fn resolve_git_dir(repo_dir: &Path) -> Option<PathBuf> {
    let dot_git = repo_dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if dot_git.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        let git_dir_line = content.lines().find(|line| line.starts_with("gitdir:"))?;
        let git_dir_path = git_dir_line.trim_start_matches("gitdir:").trim();
        let git_dir = PathBuf::from(git_dir_path);

        if git_dir.is_absolute() {
            return Some(git_dir);
        }

        return Some(repo_dir.join(git_dir));
    }

    None
}

/// Runs a cancellable git command and returns stdout text.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `error_context` - Prefix used for command failure messages
///
/// # Returns
/// The command stdout on success.
///
/// # Errors
/// Returns [`GitError::CommandTimedOut`] if the command exceeds its runtime
/// bound, or [`GitError::CommandFailed`] if spawning fails or the command
/// exits with a non-zero status.
pub(super) async fn run_git_command(
    repo_path: PathBuf,
    args: Vec<String>,
    error_context: String,
) -> Result<String, GitError> {
    let command_runner = ProcessAsyncGitCommandRunner;

    run_git_command_with_runner(
        AsyncGitCommand::new(repo_path, args),
        &error_context,
        &command_runner,
    )
    .await
}

/// Runs one asynchronous git command through an injected runner.
pub(super) async fn run_git_command_with_runner(
    command: AsyncGitCommand,
    error_context: &str,
    command_runner: &dyn AsyncGitCommandRunner,
) -> Result<String, GitError> {
    let git_invocation = format_git_invocation_from_strings(&command.arguments);
    let output = command_runner.run(command).await?;
    if !output.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);

        return Err(GitError::CommandFailed {
            command: git_invocation,
            stderr: format!("{error_context}: {detail}"),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs blocking git work on Tokio's blocking pool inside the caller's trace
/// context, so its `git.command` spans join the initiating trace.
///
/// # Errors
/// Returns the [`JoinError`] when the blocking task panics or is canceled.
pub(super) async fn spawn_blocking_in_context<Work, T>(work: Work) -> Result<T, JoinError>
where
    Work: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let context = Context::current();

    spawn_blocking(move || {
        let _context = context.attach();

        work()
    })
    .await
}

/// Content-free `git.command` span for one git subprocess.
///
/// Records only beneath an active trace, so untraced background polling
/// starts no traces. Arguments, paths, and output text are never exported.
struct GitCommandSpan(Option<Span>);

impl GitCommandSpan {
    /// Starts a span for one git invocation when the current context is
    /// traced, exporting only its allowlisted subcommand.
    fn start<'argument>(arguments: impl IntoIterator<Item = &'argument str>) -> Self {
        if !Context::current().has_active_span() {
            return Self(None);
        }

        let span = Span::child("git.command");
        let subcommand = git_subcommand(arguments)
            .and_then(|subcommand| {
                TRACED_GIT_SUBCOMMANDS
                    .into_iter()
                    .find(|known| *known == subcommand)
            })
            .unwrap_or("other");
        span.attribute("git.subcommand", subcommand);

        Self(Some(span))
    }

    /// Completes the span for a process that ran to exit. A nonzero exit
    /// still completes, because callers decide whether it is a failure.
    fn exited(self, exit_code: Option<i32>, output_bytes: usize) {
        let Some(span) = self.0 else {
            return;
        };

        if let Some(exit_code) = exit_code {
            span.attribute("process.exit.code", i64::from(exit_code));
        }
        span.attribute("agentty.timed_out", false);
        span.attribute(
            "agentty.output.bytes",
            i64::try_from(output_bytes).unwrap_or(i64::MAX),
        );
        span.finish(Outcome::Completed);
    }

    /// Fails the span when the process timed out or could not be spawned.
    fn failed(self, timed_out: bool) {
        let Some(span) = self.0 else {
            return;
        };

        span.attribute("agentty.timed_out", timed_out);
        span.fail(if timed_out { "timeout" } else { "spawn" });
    }
}

/// Runs a cancellable git subprocess with an explicit runtime bound.
async fn run_git_command_with_timeout(
    command: AsyncGitCommand,
    timeout: Duration,
) -> Result<AsyncGitCommandOutput, GitError> {
    let git_invocation = format_git_invocation_from_strings(&command.arguments);
    let span = GitCommandSpan::start(command.arguments.iter().map(String::as_str));
    let mut process = AsyncCommand::new("git");
    process
        .args(&command.arguments)
        .current_dir(&command.repo_path)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    apply_non_interactive_environment_async(&mut process, &command.arguments);
    for (key, value) in &command.environment {
        process.env(key, value);
    }

    let output = match time::timeout(timeout, process.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            span.failed(false);

            return Err(GitError::CommandFailed {
                command: git_invocation,
                stderr: error.to_string(),
            });
        }
        Err(_) => {
            span.failed(true);

            return Err(GitError::CommandTimedOut {
                command: git_invocation,
                timeout,
            });
        }
    };
    span.exited(
        output.status.code(),
        output.stdout.len().saturating_add(output.stderr.len()),
    );

    Ok(AsyncGitCommandOutput {
        exit_code: output.status.code(),
        stderr: output.stderr,
        stdout: output.stdout,
    })
}

/// Runs a git command in `repo_path` and returns stdout text.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `error_context` - Human-readable label prepended to the stderr detail on
///   failure (e.g. `"Failed to read squash merge diff"`)
///
/// # Returns
/// The command stdout on success.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] with the concrete git invocation in
/// `command` and the `error_context` plus stderr/stdout detail in `stderr`.
pub(super) fn run_git_command_sync(
    repo_path: &Path,
    args: &[&str],
    error_context: &str,
) -> Result<String, GitError> {
    let output = run_git_command_output_sync(repo_path, args)?;
    if !output.status.success() {
        let detail = command_output_detail(&output.stdout, &output.stderr);
        let git_invocation = format_git_invocation(args);

        return Err(GitError::CommandFailed {
            command: git_invocation,
            stderr: format!("{error_context}: {detail}"),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Formats the full git invocation string from command arguments.
fn format_git_invocation(args: &[&str]) -> String {
    if args.is_empty() {
        return "git".to_string();
    }

    format!("git {}", args.join(" "))
}

/// Formats a git invocation from owned command arguments.
pub(super) fn format_git_invocation_from_strings(args: &[String]) -> String {
    let argument_refs = args.iter().map(String::as_str).collect::<Vec<_>>();

    format_git_invocation(&argument_refs)
}

/// Runs a git command in `repo_path` and returns raw process output.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
///
/// # Returns
/// The process output, including status, stdout, and stderr.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] if spawning the command fails.
pub(super) fn run_git_command_output_sync(
    repo_path: &Path,
    args: &[&str],
) -> Result<Output, GitError> {
    run_git_command_output_with_env_sync(repo_path, args, &[] as &[(&str, &str)])
}

/// Runs a git command in `repo_path` with environment overrides and returns
/// raw process output.
///
/// Applies non-interactive defaults (`GIT_TERMINAL_PROMPT=0`,
/// `GCM_INTERACTIVE=never`, and SSH batch mode) and closes stdin so
/// credential failures do not block waiting for terminal input.
/// Caller-provided `environment` pairs are then applied and can override
/// these defaults.
///
/// # Arguments
/// * `repo_path` - Path to the git repository or worktree
/// * `args` - Git command arguments
/// * `environment` - Environment variables applied to the git process
///
/// # Returns
/// The process output, including status, stdout, and stderr.
///
/// # Errors
/// Returns [`GitError::CommandFailed`] if spawning the command fails.
pub(super) fn run_git_command_output_with_env_sync<Key, Value>(
    repo_path: &Path,
    args: &[&str],
    environment: &[(Key, Value)],
) -> Result<Output, GitError>
where
    Key: AsRef<std::ffi::OsStr>,
    Value: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo_path)
        .stdin(Stdio::null());
    apply_non_interactive_environment(&mut command, args);

    for (key, value) in environment {
        command.env(key, value);
    }

    let span = GitCommandSpan::start(args.iter().copied());
    match command.output() {
        Ok(output) => {
            span.exited(
                output.status.code(),
                output.stdout.len().saturating_add(output.stderr.len()),
            );

            Ok(output)
        }
        Err(error) => {
            span.failed(false);

            Err(GitError::CommandFailed {
                command: format_git_invocation(args),
                stderr: error.to_string(),
            })
        }
    }
}

/// Graft file Git reads instead of `info/grafts`: a path below a file,
/// which can never exist, so Git reads no grafts and warns about none.
const NO_GRAFT_FILE: &str = "/dev/null/grafts";

/// Hooks directory host Git commands search instead of the configured one:
/// a file, below which no hook can exist, so Git runs none.
pub(super) const NO_HOOKS_DIRECTORY: &str = "/dev/null";

/// Git subcommands that never run a hook or another configured program, so
/// they read the configuration as it is, including the hooks directory
/// that the pre-commit readiness check resolves.
const CONFIGURATION_READERS: [&str; 2] = ["config", "rev-parse"];

/// Disables optional index writes and interactive prompts for Git subprocesses.
/// Required locks for staging, commits, and other mutations remain enabled.
/// See [`trusted_history_environment`] for the history Git reads and
/// [`host_configuration`] for the programs it runs.
fn apply_non_interactive_environment(command: &mut Command, args: &[&str]) {
    let git_ssh_command = batch_mode_ssh_command(std::env::var("GIT_SSH_COMMAND").ok());

    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", git_ssh_command)
        .envs(trusted_history_environment())
        .envs(host_configuration(
            std::env::var("GIT_CONFIG_COUNT").ok(),
            disables_hooks(args.iter().copied()),
        ));
}

/// Disables optional index writes and interactive prompts for async Git
/// running `args`.
/// See [`trusted_history_environment`] for the history Git reads and
/// [`host_configuration`] for the programs it runs.
fn apply_non_interactive_environment_async(command: &mut AsyncCommand, args: &[String]) {
    let git_ssh_command = batch_mode_ssh_command(std::env::var("GIT_SSH_COMMAND").ok());

    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_SSH_COMMAND", git_ssh_command)
        .envs(trusted_history_environment())
        .envs(host_configuration(
            std::env::var("GIT_CONFIG_COUNT").ok(),
            disables_hooks(args.iter().map(String::as_str)),
        ));
}

/// Returns Git's environment configuration, appended after the
/// `configured_count` pairs the caller's environment already passes, that
/// keeps host Git from running programs a session can control: the
/// [`SUBMODULE_ISOLATION_CONFIGURATION`], no `core.fsmonitor` daemon, and,
/// when `disable_hooks` is set, no hooks.
///
/// A session can write its worktree, so a hooks directory inside it, such
/// as one a hook manager configures there, or a hook manager reading its
/// configuration from the worktree, would run session-written commands with
/// the host's access. The commit hooks Agentty relies on run inside the
/// session's sandbox instead. The environment overrides every configuration
/// file, including a user's global `submodule.recurse=true`; an explicit
/// `git -c` still overrides it.
fn host_configuration(
    configured_count: Option<String>,
    disable_hooks: bool,
) -> Vec<(String, String)> {
    let first = configured_count
        .and_then(|count| count.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut entries = SUBMODULE_ISOLATION_CONFIGURATION.to_vec();
    entries.push(("core.fsmonitor", "false"));
    if disable_hooks {
        entries.push(("core.hooksPath", NO_HOOKS_DIRECTORY));
    }
    let mut environment = entries
        .iter()
        .enumerate()
        .flat_map(|(offset, (key, value))| {
            let index = first.saturating_add(offset);

            [
                (format!("GIT_CONFIG_KEY_{index}"), (*key).to_string()),
                (format!("GIT_CONFIG_VALUE_{index}"), (*value).to_string()),
            ]
        })
        .collect::<Vec<_>>();
    environment.push((
        "GIT_CONFIG_COUNT".to_string(),
        first.saturating_add(entries.len()).to_string(),
    ));

    environment
}

/// Returns whether host Git runs `arguments` without the configured hooks:
/// always, except for subcommands that never run one. Even a hook the
/// session cannot write, such as a `pre-push` hook in the shared Git
/// directory, runs in the worktree and commonly runs whatever the worktree
/// configures, so pushes run no hooks either.
fn disables_hooks<'argument>(arguments: impl IntoIterator<Item = &'argument str>) -> bool {
    !git_subcommand(arguments).is_some_and(|subcommand| CONFIGURATION_READERS.contains(&subcommand))
}

/// Configuration every host Git command runs with, because a session may
/// have left a repository at a submodule path whose hooks and configuration
/// would otherwise run: commands such as checkout, reset, fetch, and rebase
/// never recurse into submodules, and commits, including those a rebase
/// makes, never print a status, which would run `git status` inside them.
const SUBMODULE_ISOLATION_CONFIGURATION: [(&str, &str); 2] =
    [("submodule.recurse", "false"), ("commit.status", "false")];

/// Makes Git read the history the objects record, ignoring `refs/replace`
/// and grafts, which an agent working in a session can write: otherwise a
/// replacement could hide content from Agentty's review diff while a push
/// still sends the real objects.
fn trusted_history_environment() -> [(&'static str, &'static str); 2] {
    [
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", NO_GRAFT_FILE),
    ]
}

/// Appends SSH batch mode to the configured `GIT_SSH_COMMAND`, or uses plain
/// `ssh` in batch mode when none is configured.
fn batch_mode_ssh_command(configured_command: Option<String>) -> String {
    match configured_command {
        Some(configured_command) => format!("{configured_command} -o BatchMode=yes"),
        None => "ssh -o BatchMode=yes".to_string(),
    }
}

/// Extracts the best human-readable error detail from command output.
pub(super) fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr_text = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr_text.is_empty() {
        return stderr_text;
    }

    let stdout_text = String::from_utf8_lossy(stdout).trim().to_string();
    if !stdout_text.is_empty() {
        return stdout_text;
    }

    "Unknown git error".to_string()
}

/// Resolves the shared repository through cancellable async git commands.
async fn resolve_shared_repo(repo_path: &Path) -> Result<SharedRepo, GitError> {
    let (git_dir, git_common_dir) = git_directory_paths_async(repo_path).await?;
    let is_bare = run_git_command(
        git_common_dir.clone(),
        vec!["rev-parse".to_string(), "--is-bare-repository".to_string()],
        "Git rev-parse --is-bare-repository failed".to_string(),
    )
    .await?;
    if is_bare.trim() == "true" {
        return Ok(SharedRepo::Bare(git_common_dir));
    }

    let shared_git_dir = if git_dir == git_common_dir {
        git_dir
    } else {
        git_common_dir
    };
    let repo_root = repo_root_from_git_dir_async(repo_path, &shared_git_dir).await?;

    Ok(SharedRepo::Working(repo_root))
}

/// Converts SSH-style GitHub remotes into HTTPS while preserving other URLs.
fn normalize_repo_url(remote: &str) -> String {
    let trimmed = remote.trim_end_matches(".git");
    if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        return format!("https://github.com/{path}");
    }

    if let Some(path) = trimmed.strip_prefix("ssh://git@github.com/") {
        return format!("https://github.com/{path}");
    }

    trimmed.to_string()
}

/// Reads absolute git and common git directory paths for `repo_path`.
async fn git_directory_paths_async(repo_path: &Path) -> Result<(PathBuf, PathBuf), GitError> {
    let stdout = run_git_command(
        repo_path.to_path_buf(),
        vec![
            "rev-parse".to_string(),
            "--git-dir".to_string(),
            "--git-common-dir".to_string(),
        ],
        "Git rev-parse failed".to_string(),
    )
    .await?;

    parse_git_directory_paths(repo_path, &stdout)
}

/// Reads absolute git and common git directory paths synchronously.
fn git_directory_paths(repo_path: &Path) -> Result<(PathBuf, PathBuf), GitError> {
    let stdout = run_git_command_sync(
        repo_path,
        &["rev-parse", "--git-dir", "--git-common-dir"],
        "Git rev-parse failed",
    )?;

    parse_git_directory_paths(repo_path, &stdout)
}

/// Parses the two paths emitted by `git rev-parse`.
fn parse_git_directory_paths(
    repo_path: &Path,
    stdout: &str,
) -> Result<(PathBuf, PathBuf), GitError> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let git_dir = lines
        .next()
        .ok_or_else(|| GitError::OutputParse("Git rev-parse output missing git-dir".to_string()))?;
    let git_common_dir = lines.next().ok_or_else(|| {
        GitError::OutputParse("Git rev-parse output missing git-common-dir".to_string())
    })?;

    Ok((
        normalize_git_dir_path(repo_path, git_dir),
        normalize_git_dir_path(repo_path, git_common_dir),
    ))
}

/// Converts a git directory path (typically `.git`) into repository root.
async fn repo_root_from_git_dir_async(
    repo_path: &Path,
    git_dir: &Path,
) -> Result<PathBuf, GitError> {
    if let Some(repo_root) = repo_root_from_dot_git_dir(git_dir) {
        return Ok(repo_root);
    }

    let root = run_git_command(
        repo_path.to_path_buf(),
        vec!["rev-parse".to_string(), "--show-toplevel".to_string()],
        "Git rev-parse --show-toplevel failed".to_string(),
    )
    .await?;

    parse_repo_root(&root)
}

/// Converts a git directory path into a repository root synchronously.
fn repo_root_from_git_dir(repo_path: &Path, git_dir: &Path) -> Result<PathBuf, GitError> {
    if let Some(repo_root) = repo_root_from_dot_git_dir(git_dir) {
        return Ok(repo_root);
    }

    let root = run_git_command_sync(
        repo_path,
        &["rev-parse", "--show-toplevel"],
        "Git rev-parse --show-toplevel failed",
    )?;

    parse_repo_root(&root)
}

/// Returns the parent of a conventional `.git` directory when applicable.
///
/// Returns `None` for any other directory name. A path ending in a `.git`
/// component always has a parent, so the conventional case cannot fail.
fn repo_root_from_dot_git_dir(git_dir: &Path) -> Option<PathBuf> {
    if git_dir.file_name() != Some(std::ffi::OsStr::new(".git")) {
        return None;
    }

    git_dir.parent().map(Path::to_path_buf)
}

/// Parses a non-empty `git rev-parse --show-toplevel` response.
fn parse_repo_root(root: &str) -> Result<PathBuf, GitError> {
    let root = root.trim().to_string();
    if root.is_empty() {
        return Err(GitError::OutputParse(
            "Git rev-parse --show-toplevel returned empty output".to_string(),
        ));
    }

    Ok(PathBuf::from(root))
}

/// Normalizes a git metadata path into absolute form for path comparisons.
fn normalize_git_dir_path(repo_path: &Path, git_path: &str) -> PathBuf {
    let git_path = PathBuf::from(git_path);
    let git_path = if git_path.is_absolute() {
        git_path
    } else {
        repo_path.join(git_path)
    };

    std::fs::canonicalize(&git_path).unwrap_or(git_path)
}

#[cfg(test)]
#[path = "repo_test.rs"]
mod tests;

//! Shell commands run through a host-selected executor.
//!
//! Bash is the only tool that needs per-turn configuration: add `bash` to
//! the turn's `ToolSet` and attach a [`BashConfig`] with
//! `TurnOptions::with_bash`. `BashConfig::native` and `BashConfig::new`
//! select the native sandbox with the `native-sandbox` feature, subject to
//! the harness's `crate::sandbox::Sandbox` choice where it cannot run;
//! [`BashConfig::for_executor`] selects a host [`BashExecutor`] such as
//! [`UnsandboxedExecutor`], which enforces no isolation. Each executor
//! reports its [`Enforcement`], and the model-facing
//! tool description states it. Command intents are journaled before spawning
//! and surface as [`CommandRecord`]s.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub use crate::command_journal::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandRecord, CommandTermination,
};
pub use crate::execution::{
    BashExecutor, BashProcess, Enforcement, ExecutionAccess, ExecutionCommand, ExecutionError,
    ExecutionPolicy, MainExit, OutputStream, ProcessEvent, UnsandboxedExecutor,
};

/// Identity recorded for the default native sandbox executor.
pub(crate) const NATIVE_EXECUTOR: &str = "native";

/// Isolation the default native sandbox enforces on every supported platform
/// unless the configuration grants networking; where it cannot run, the
/// harness's sandbox choice decides instead.
pub(crate) const NATIVE_ENFORCEMENT: Enforcement = Enforcement::FileSystemAndNetwork;

/// Environment variable naming the granted temporary directory.
const TEMPORARY_DIRECTORY_VARIABLE: &str = "TMPDIR";

/// Most entries one configuration accepts in each path list and in its
/// excluded commands.
const MAX_PATH_GRANTS: usize = 64;
/// Longest branch name [`BashConfig::with_git_writes`] accepts.
const MAX_BRANCH_BYTES: usize = 255;

/// Shell syntax that keeps a command from leaving the sandbox: operators,
/// quotes, escapes, expansions, globs, history, and comments, whose meaning
/// the harness would have to reproduce to show the exact arguments.
const EXCLUDED_SYNTAX: [char; 21] = [
    ';', '&', '|', '<', '>', '(', ')', '$', '`', '\\', '\'', '"', '*', '?', '[', ']', '{', '}',
    '~', '!', '#',
];

/// Immutable host capabilities for Bash. No external reads or environment
/// values are inherited. Runtime libraries and executables require read
/// grants: [`Self::with_system_reads`] grants the platform's system
/// directories and [`Self::with_read`] anything else, such as toolchains.
/// Grants and denials state policy; their enforcement is the selected
/// executor's documented scope, and an unenforcing executor such as
/// [`crate::bash::UnsandboxedExecutor`] applies none of them.
/// The default native executor requires a helper of the matching version
/// at a trusted location outside the command workspace: the host executable
/// calling `crate::sandbox::dispatch`, or the `ag-harness-sandbox` binary;
/// [`BashConfig::for_executor`] selects an explicit host executor instead.
#[derive(Clone)]
pub struct BashConfig {
    pub(crate) enforcement: Enforcement,
    pub(crate) environment: BTreeMap<String, String>,
    /// [`Self::with_excluded_environment`] values.
    pub(crate) excluded_environment: BTreeMap<String, String>,
    pub(crate) executor: Option<Arc<dyn BashExecutor>>,
    pub(crate) snapshot: BashPolicySnapshot,
}

impl BashConfig {
    /// Suggested `capture_bytes`: the combined stdout and stderr bytes one
    /// command result keeps, split between the start and end of each stream.
    pub const DEFAULT_CAPTURE_BYTES: usize = 8 * 1024;
    /// Largest accepted `capture_bytes`, the same size as the bound on one
    /// encoded `read` result. It caps captured output bytes only: a `bash`
    /// result's encoding, with escaping and outcome fields, can exceed it, and
    /// `bash` results are not deferred by the per-round result share, so a
    /// larger capture spends more of the context window on each command.
    pub const MAX_CAPTURE_BYTES: usize = crate::tool::MAX_TOOL_RESULT_BYTES;

    /// Configures the default native sandbox executor with trusted launcher
    /// and Bash executables and a nonsecret policy revision. Change the
    /// revision whenever executable contents or granted environment values
    /// change. Paths must be absolute and outside the workspace; launch
    /// performs filesystem validation. `capture_bytes` bounds the output
    /// each command returns, from 1 to [`Self::MAX_CAPTURE_BYTES`]; use
    /// [`Self::DEFAULT_CAPTURE_BYTES`] unless the host needs another bound.
    ///
    /// Requires the `native-sandbox` feature; without it, select an executor
    /// with [`Self::for_executor`].
    ///
    /// # Errors
    /// Rejects invalid identities, paths, deadlines, and capture bounds.
    #[cfg(feature = "native-sandbox")]
    pub fn new(
        launcher: PathBuf,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        if !valid_path(&launcher, true) {
            return Err(BashError::InvalidPolicy);
        }

        Self::with_snapshot(
            Some(launcher),
            None,
            NATIVE_EXECUTOR.to_string(),
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Configures the default native sandbox executor as [`Self::new`] does,
    /// with this host executable as the launcher: the host must call
    /// [`crate::sandbox::dispatch`] first in `main`. Without that call, or
    /// when the executable is inside the workspace, the harness's
    /// [`crate::sandbox::Sandbox`] choice decides what runs instead.
    ///
    /// Requires the `native-sandbox` feature.
    ///
    /// # Errors
    /// Rejects invalid paths, revisions, deadlines, and capture bounds.
    #[cfg(feature = "native-sandbox")]
    pub fn native(
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        Self::with_snapshot(
            None,
            None,
            NATIVE_EXECUTOR.to_string(),
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Configures an explicit host-selected executor instead of the default
    /// native sandbox launcher. Selection is always explicit: the harness
    /// never falls back to another executor or consults the environment. The
    /// executor identity is recorded in durable policy snapshots and
    /// host-request fingerprints; hosts must supply a new identity or
    /// revision when executor behavior changes. `capture_bytes` is bounded as
    /// for [`Self::new`].
    ///
    /// # Errors
    /// Rejects invalid executor identities, paths, deadlines, and capture
    /// bounds.
    pub fn for_executor(
        executor: Arc<dyn BashExecutor>,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        let identity = executor.identity().to_string();
        if identity.trim().is_empty()
            || identity.len() > 256
            || identity.contains('\0')
            || identity == NATIVE_EXECUTOR
        {
            return Err(BashError::InvalidPolicy);
        }

        Self::with_snapshot(
            None,
            Some(executor),
            identity,
            bash,
            revision,
            timeout,
            capture_bytes,
        )
    }

    /// Validates the shared policy fields; `executor` is `None` only for the
    /// native sandbox, whose `launcher` is `None` when the dispatching host
    /// executable launches it.
    fn with_snapshot(
        launcher: Option<PathBuf>,
        executor: Option<Arc<dyn BashExecutor>>,
        identity: String,
        bash: PathBuf,
        revision: String,
        timeout: Duration,
        capture_bytes: usize,
    ) -> Result<Self, BashError> {
        if !valid_path(&bash, true)
            || revision.trim().is_empty()
            || revision.len() > 256
            || timeout.is_zero()
            || timeout > Duration::from_secs(3600)
            || capture_bytes == 0
            || capture_bytes > Self::MAX_CAPTURE_BYTES
        {
            return Err(BashError::InvalidPolicy);
        }
        let enforcement = executor
            .as_ref()
            .map_or(NATIVE_ENFORCEMENT, |executor| executor.enforcement());

        Ok(Self {
            enforcement,
            environment: BTreeMap::new(),
            excluded_environment: BTreeMap::new(),
            executor,
            snapshot: BashPolicySnapshot {
                bash,
                capabilities: BTreeSet::new(),
                capture_bytes,
                environment_names: Vec::new(),
                excluded_commands: BTreeMap::new(),
                excluded_environment_names: Vec::new(),
                executor: identity,
                external_reads: Vec::new(),
                git_branch: None,
                hidden_paths: Vec::new(),
                host_information: false,
                launcher,
                linux_bubblewrap: None,
                revision,
                temporary_directory: None,
                timeout,
                unix_sockets: Vec::new(),
                workspace_writes: Vec::new(),
                writable_roots: Vec::new(),
            },
        })
    }

    /// Grants recursive external read access, including runtime resources.
    /// The native sandbox grants the directory by path, without inspecting
    /// its contents, so a grant as large as a toolchain costs nothing per
    /// command; links inside it resolve to their targets, which stay subject
    /// to the policy. The path must exist, and it must neither contain the
    /// workspace nor resolve inside it.
    ///
    /// # Errors
    /// Rejects relative paths. Native preparation checks actual filesystem
    /// state.
    pub fn with_read(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) {
            return Err(BashError::InvalidPolicy);
        }
        if self.snapshot.external_reads.len() >= MAX_PATH_GRANTS {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.external_reads.push(path);

        Ok(self)
    }

    /// Grants read access to the whole filesystem except hidden paths, as
    /// the sandboxes of coding agents such as Claude Code and Codex do;
    /// without it, reads are limited to the workspace and the explicit read
    /// grants. It covers [`Self::with_system_reads`], every toolchain, and
    /// user configuration such as `~/.gitconfig`, so a coding session needs
    /// no per-root read grants. On Linux the root is mounted read-only with
    /// a private `/proc` and a minimal `/dev`. Writes still need grants.
    ///
    /// Besides the paths [`Self::with_hidden_path`] names, it always hides
    /// `/run/secrets` and these credential stores and browser profiles in
    /// the home directory the account database records, and in `HOME` of
    /// the harness and of the commands where it differs: the keychains,
    /// cookies, Thunderbird, and Firefox, Chrome, Chromium, Brave, and Edge
    /// profiles; `~/.ssh`, `~/.aws`, `~/.azure`, `~/.gnupg`, `~/.config/gh`,
    /// `~/.config/hub`, `~/.config/gcloud`, Git's stored credentials,
    /// `~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.gem/credentials`, Cargo's
    /// credentials, Maven's and Gradle's settings, `~/.docker/config.json`,
    /// `~/.kube/config`, Terraform's and Vault's tokens, the Codex and
    /// Claude Code credentials, and shell histories. The same stores are
    /// hidden where `CARGO_HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
    /// `GNUPGHOME`, `DOCKER_CONFIG`, `KUBECONFIG`, `GH_CONFIG_DIR`,
    /// `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, the AWS, Google, and npm
    /// configuration variables, or `HISTFILE` of the harness or the
    /// commands move them, unless they lie in the session's own
    /// directories. Preparation refuses full-disk reads when the account
    /// has no home directory.
    ///
    /// It also hides every other entry in the parent of the
    /// [`Self::with_temporary_directory`] directory, such as other
    /// sessions' temporary directories, so preparation refuses a temporary
    /// directory whose parent contains the workspace or a home directory:
    /// give each session its own directory inside a dedicated parent. Read
    /// grants inside a hidden directory are hidden too. Tools that need a
    /// credential get it another way, such as a token passed with
    /// [`Self::with_environment`].
    ///
    /// # Errors
    /// Rejects a host executor that enforces isolation but does not declare
    /// [`BashExecutor::supports_hidden_paths`], since it would expose the
    /// hidden credential stores.
    pub fn with_full_disk_read(mut self) -> Result<Self, BashError> {
        self.require_hidden_paths()?;
        self.snapshot.capabilities.insert(Capability::FullDiskRead);

        Ok(self)
    }

    /// Hides an absolute path from commands: they can neither read nor
    /// write it or anything beneath it, even under a broader read grant,
    /// such as [`Self::with_full_disk_read`]. Use it for secrets, such as a
    /// project's `.env` file. A writable directory inside a hidden path
    /// stays accessible.
    ///
    /// Preparation resolves links and refuses a hidden path that contains
    /// the workspace or lies inside a writable directory, where a command
    /// could rename one of its parents to read it. A path missing at launch
    /// stays hidden on macOS, where the rule names the path; on Linux,
    /// which mounts an empty read-only directory or file over each hidden
    /// path, a missing path cannot be created by commands anyway, because
    /// it is not inside a writable directory.
    ///
    /// # Errors
    /// Rejects relative paths, traversal, `/`, a path given twice, more
    /// than 64 hidden paths, and a host executor that enforces isolation
    /// but does not declare [`BashExecutor::supports_hidden_paths`].
    pub fn with_hidden_path(mut self, path: PathBuf) -> Result<Self, BashError> {
        self.require_hidden_paths()?;
        if !valid_path(&path, true)
            || path.parent().is_none()
            || self.snapshot.hidden_paths.contains(&path)
            || self.snapshot.hidden_paths.len() >= MAX_PATH_GRANTS
        {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.hidden_paths.push(path);

        Ok(self)
    }

    /// Grants reads and writes beneath an existing absolute directory
    /// outside the workspace and every home directory, like the writable
    /// roots of Codex and the write allowlist of Claude Code.
    ///
    /// A command can plant code in a writable directory that host tools run
    /// later, outside the sandbox, so granting a root is equivalent to
    /// letting commands run code on the host the next time a host tool
    /// reads it. Even tool caches are not safe: Cargo, Go, and Git use
    /// cached entries without re-verifying them. Preparation therefore
    /// refuses a root that overlaps a home directory (the account
    /// database's, or `HOME` of the harness or the commands), a system or
    /// toolchain directory such as `/usr`, `/opt`, `/etc`, `/Library`, or
    /// `/home/linuxbrew`, the workspace (grant those with
    /// [`Self::with_write`]), or its Git metadata, and refuses every root
    /// when the account has no home directory. Point tool caches at the
    /// temporary directory through their environment instead, for example
    /// `CARGO_HOME`, `npm_config_cache`, `GOMODCACHE`, `GOCACHE`, and
    /// `PIP_CACHE_DIR`.
    ///
    /// macOS keeps every `.git` entry inside a root read-only; Linux
    /// protects only the workspace repository's own Git metadata, so a
    /// repository nested in a root is writable there.
    ///
    /// # Errors
    /// Rejects relative paths, traversal, `/`, Git metadata components, a
    /// root given twice, and more than 64 roots.
    pub fn with_writable_root(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true)
            || path.parent().is_none()
            || has_git_component(&path)
            || self.snapshot.writable_roots.contains(&path)
            || self.snapshot.writable_roots.len() >= MAX_PATH_GRANTS
        {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.writable_roots.push(path);

        Ok(self)
    }

    /// Lets commands connect to the Unix-domain socket at an absolute path,
    /// such as an `ssh-agent` or `gpg-agent` socket; every other
    /// Unix-domain socket stays denied. The path is resolved now and the
    /// grant pinned to the socket it names, so a link changed later cannot
    /// redirect it; grant a Docker socket only by naming it. Preparation
    /// refuses a socket inside the workspace or a writable directory, where
    /// a command could replace it.
    ///
    /// Only macOS can allow sockets by path. Linux cannot tell sockets
    /// apart, so they stay denied there, as in Claude Code's sandbox, and
    /// the harness reports
    /// [`crate::sandbox::SandboxUnavailableReason::UnixSocketsUnsupported`]:
    /// a warning on each turn, or a refused turn under
    /// [`crate::sandbox::IfUnavailable::Refuse`].
    ///
    /// # Errors
    /// Rejects relative paths, traversal, a path that does not name an
    /// existing socket, a socket given twice, and more than 64 sockets.
    pub fn with_unix_socket(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) || self.snapshot.unix_sockets.len() >= MAX_PATH_GRANTS {
            return Err(BashError::InvalidPolicy);
        }
        let socket = std::fs::canonicalize(path)
            .ok()
            .filter(|socket| {
                std::fs::metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
            })
            .filter(|socket| !self.snapshot.unix_sockets.contains(socket))
            .ok_or(BashError::InvalidPolicy)?;
        self.snapshot.unix_sockets.push(socket);

        Ok(self)
    }

    /// Lets simple commands of the program at the absolute path
    /// `executable` run outside the sandbox, with the harness process's own
    /// access, once the host's [`crate::approval::ApprovalHandler`]
    /// explicitly approves each one with
    /// [`crate::approval::Decision::AllowUnsandboxed`], like Claude Code's
    /// `excludedCommands`, which still ask for permission. Such calls are
    /// marked [`crate::tool::ToolEffects::ExecutesUnsandboxed`] and carry the
    /// exact argument vector in [`crate::approval::ApprovalRequest::command`].
    /// Without an approval handler, or with a plain
    /// [`crate::approval::Decision::Allow`], the commands run inside the
    /// sandbox.
    ///
    /// Excluding a program trusts it fully. It can run any code its
    /// arguments name, such as `git -c core.fsmonitor=./x status`, `gh
    /// extension install`, or `docker run -v /:/host`. It also follows
    /// configuration it finds in the workspace, which commands can write
    /// and the approval does not show, such as a `compose.yaml`,
    /// `.cargo/config.toml`, or `.npmrc`. An approval must show the exact
    /// argument vector and warn about both.
    ///
    /// A command qualifies when its first word is the executable's file
    /// name and it consists only of words of printable ASCII separated by
    /// spaces and tabs, without quotes, escapes, expansions, globs, or
    /// operators: none of `;`, `&`, `|`, `<`, `>`, `(`, `)`, `$`, backquote,
    /// backslash, `'`, `"`, `*`, `?`, `[`, `]`, `{`, `}`, `~`, `!`, or `#`.
    /// The harness executes the executable resolved now directly with those
    /// words as arguments, with no shell, so workspace file names cannot
    /// become arguments. It runs in the workspace with a minimal environment
    /// the host controls: a `PATH` of the existing absolute directories
    /// outside the places commands can write, `HOME`, `USER`, `LOGNAME`,
    /// `LANG`, `LC_*`, and `TERM` from the harness process, and the values
    /// [`Self::with_excluded_environment`] passes; never the values
    /// [`Self::with_environment`] grants or the temporary directory. The
    /// harness refuses the turn when the executable or one of those values
    /// lies inside the workspace, the temporary directory, or a writable
    /// root.
    /// The excluded command's record names the unsandboxed executor. It
    /// applies only to a configuration whose executor enforces isolation,
    /// and the model-facing `bash` description names the program.
    ///
    /// # Errors
    /// Rejects a relative path, a path that does not name an executable
    /// regular file, a file name other than 1–64 ASCII letters, digits, `.`,
    /// `_`, `+`, or `-` not starting with `-`, a program named twice, and
    /// more than 64 programs.
    pub fn with_excluded_command(mut self, executable: PathBuf) -> Result<Self, BashError> {
        let program = executable
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|program| {
                program.len() <= 64
                    && !program.starts_with('-')
                    && program
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            })
            .map(str::to_owned)
            .filter(|program| {
                valid_path(&executable, true)
                    && !self.snapshot.excluded_commands.contains_key(program)
                    && self.snapshot.excluded_commands.len() < MAX_PATH_GRANTS
            });
        let resolved = std::fs::canonicalize(executable).ok().filter(|resolved| {
            std::fs::metadata(resolved)
                .is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
        });
        let (Some(program), Some(resolved)) = (program, resolved) else {
            return Err(BashError::InvalidPolicy);
        };
        self.snapshot.excluded_commands.insert(program, resolved);

        Ok(self)
    }

    /// Grants read access to the platform's system directories, which Bash,
    /// Git, and developer toolchains need to start: on macOS `/bin`, `/sbin`,
    /// `/usr`, `/System`, `/Library/Apple`, `/Library/Developer`, the
    /// selected Xcode and its
    /// license record,
    /// `/private/etc`, and the shell and time zone selections under
    /// `/private/var`; on Linux `/bin`, `/sbin`, `/usr`, `/lib*`, and `/etc`,
    /// with a private `/proc` and a minimal `/dev`. Paths missing on the host
    /// are skipped. Toolchains outside them, such as `~/.rustup` or
    /// `/opt/homebrew`, still need [`Self::with_read`], and so do user
    /// configuration files tools read once `HOME` is granted, such as
    /// `~/.gitconfig`, without which Git fails; [`Self::with_full_disk_read`]
    /// covers all of them.
    #[must_use]
    pub fn with_system_reads(mut self) -> Self {
        self.snapshot.capabilities.insert(Capability::SystemReads);

        self
    }

    /// Grants writes beneath an existing workspace-relative directory. Git
    /// metadata existing at launch remains protected unless
    /// [`Self::with_git_writes`] grants it; a repository the command itself
    /// creates inside a grant is the command's own output. Links inside the
    /// grant resolve to their targets, which stay subject to the policy:
    /// one that leaves the granted directories cannot be written through.
    /// Hard links that already exist inside the grant are files of the
    /// grant, and commands cannot create new ones to files outside it.
    /// Writes are never rolled back. Native Linux enforcement adds Landlock
    /// rules inside the launcher and fails closed before execution on kernels
    /// without the required ABI (Linux 6.2); macOS additionally denies
    /// metadata by name pattern, including names created after launch.
    ///
    /// # Errors
    /// Rejects absolute paths, traversal, and Git metadata components.
    pub fn with_write(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, false) || has_git_component(&path) {
            return Err(BashError::InvalidPolicy);
        }
        if self.snapshot.workspace_writes.len() >= MAX_PATH_GRANTS {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.workspace_writes.push(path);

        Ok(self)
    }

    /// Grants reads and writes beneath an existing absolute directory outside
    /// the workspace and exports it to commands as `TMPDIR`. Choose one
    /// directory per session, such as one the host creates under the system
    /// temporary directory, so commands never share temporary files with
    /// other sessions. It must not overlap the workspace or its Git metadata.
    /// The path is part of the policy fingerprint that identifies host
    /// requests and command records, so derive it from a stable session
    /// identity rather than choosing a fresh random name at each start.
    ///
    /// # Errors
    /// Rejects relative paths, a second temporary directory, and a
    /// configuration that already grants a `TMPDIR` value.
    pub fn with_temporary_directory(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true)
            || self.snapshot.temporary_directory.is_some()
            || self.environment.contains_key(TEMPORARY_DIRECTORY_VARIABLE)
        {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.temporary_directory = Some(path);

        Ok(self)
    }

    /// Grants the Git writes that committing to `branch` needs: the object
    /// store, the `refs/heads/<branch>` reference and its reflog, and, for a
    /// linked worktree, the worktree's own administration directory with its
    /// index, `HEAD`, and per-worktree logs and references. For a main
    /// worktree, macOS also opens the state files at the top of its Git
    /// directory, such as its index and `HEAD`. Every other reference,
    /// including other branches, tags, `refs/stash`, and `packed-refs`, stays
    /// read-only, so commands cannot create branches, stash, or pack
    /// references; Git reports those refusals as a failure to lock the
    /// reference, before it changes anything else. Configuration, hooks,
    /// worktree links, submodule metadata, and rebase and cherry-pick state
    /// stay read-only too, because Git run later outside the sandbox would
    /// otherwise execute or follow what a command wrote there, and so do
    /// object alternates, commit-graph files, grafts, and the shallow
    /// boundary, which would change the history Git shows without changing
    /// the objects it pushes. Repositories using the `reftable` reference
    /// backend keep every reference read-only.
    ///
    /// `HEAD` and the branch reference stay writable, so commands can point
    /// either at another branch. A host that runs Git on the worktree after
    /// a command, such as to commit its changes, must first check that
    /// `HEAD` still resolves to `refs/heads/<branch>`, following every
    /// symbolic reference, or its own write lands on the other branch.
    ///
    /// Linux confines writes by directory, so there the directory holding
    /// the branch stays writable: siblings of `wt/session`, such as
    /// `wt/other`, can still be moved, while `main` cannot. Choose a nested
    /// branch name to keep the base branch read-only, and keep checking
    /// other branches in hosts that review history. The launcher creates
    /// that directory and its reflog directory when missing. On Linux only a
    /// linked worktree can stage and commit: Landlock cannot keep a main
    /// worktree's Git directory from gaining a `commondir` link, so there
    /// only its object store and branch are writable, and commands are
    /// refused while a write grant contains a Git directory, such as
    /// `with_write(".")` in a main worktree. A linked worktree whose
    /// repository enables `extensions.worktreeConfig` needs its
    /// `config.worktree` to exist.
    ///
    /// An executor other than the native sandbox that cannot keep these
    /// entries read-only must refuse a policy granting Git writes.
    ///
    /// # Errors
    /// Rejects a `branch` that Git would not accept as a branch name, or
    /// that holds a `"`.
    pub fn with_git_writes(mut self, branch: String) -> Result<Self, BashError> {
        if !valid_branch(&branch) {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.git_branch = Some(branch);

        Ok(self)
    }

    /// Grants one environment value. The policy revision identifies its value
    /// for durable recovery; snapshots retain names and revision, never values.
    /// Hosts must change the revision when a value changes.
    ///
    /// # Errors
    /// Rejects empty, duplicate, NUL-containing, or invalid variable names,
    /// and `TMPDIR` once [`Self::with_temporary_directory`] set it.
    pub fn with_environment(mut self, name: String, value: String) -> Result<Self, BashError> {
        if (name == TEMPORARY_DIRECTORY_VARIABLE && self.snapshot.temporary_directory.is_some())
            || !valid_variable(&self.environment, &name, &value)
        {
            return Err(BashError::InvalidPolicy);
        }
        self.environment.insert(name, value);
        self.snapshot.environment_names = self.environment.keys().cloned().collect();

        Ok(self)
    }

    /// Passes one environment value to the commands
    /// [`Self::with_excluded_command`] runs outside the sandbox, such as a
    /// `GH_TOKEN` for an excluded `gh`. Excluded commands get none of the
    /// values [`Self::with_environment`] grants: only these, a sanitized
    /// `PATH`, and `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_*`, and `TERM`
    /// from the harness process. The policy revision identifies the value;
    /// snapshots retain only its name.
    ///
    /// # Errors
    /// Rejects empty, duplicate, NUL-containing, or invalid variable names,
    /// and `PATH`, which the harness sets.
    pub fn with_excluded_environment(
        mut self,
        name: String,
        value: String,
    ) -> Result<Self, BashError> {
        if name == "PATH" || !valid_variable(&self.excluded_environment, &name, &value) {
            return Err(BashError::InvalidPolicy);
        }
        self.excluded_environment.insert(name, value);
        self.snapshot.excluded_environment_names =
            self.excluded_environment.keys().cloned().collect();

        Ok(self)
    }

    /// Explicitly grants host-information exposure. Required by every
    /// executor; commands cannot conceal all host details. On macOS the
    /// native executor additionally grants filesystem metadata and
    /// root-directory enumeration needed by the qualified Bash runtime. File
    /// contents still require separate read grants.
    #[must_use]
    pub fn with_host_information(mut self) -> Self {
        self.snapshot.host_information = true;

        self
    }

    /// Selects a trusted native Linux Bubblewrap executable in place of the
    /// default `/usr/bin/bwrap`. No PATH lookup is used.
    ///
    /// Requires the `native-sandbox` feature.
    ///
    /// # Errors
    /// Rejects relative paths; preparation validates the executable.
    #[cfg(feature = "native-sandbox")]
    pub fn with_linux_bubblewrap(mut self, path: PathBuf) -> Result<Self, BashError> {
        if !valid_path(&path, true) {
            return Err(BashError::InvalidPolicy);
        }
        self.snapshot.linux_bubblewrap = Some(path);

        Ok(self)
    }

    /// Grants network access, which the policy denies by default. The native
    /// sandbox then allows IP connections, listeners, and name resolution:
    /// on macOS through Seatbelt network rules, with the system services
    /// that resolve names and verify TLS certificates, that still deny
    /// Unix-domain sockets other than the system resolver's and those
    /// [`Self::with_unix_socket`] grants; on Linux by sharing the
    /// host network namespace while seccomp still refuses every socket
    /// family but IPv4 and IPv6. An executor's [`Enforcement`] describes the
    /// isolation it applies to policies without this grant; with it, the
    /// configuration reports at most [`Enforcement::FileSystem`], so the
    /// model-facing description states that network access is not
    /// restricted. A host executor that cannot open the network it otherwise
    /// denies must refuse a policy granting it.
    #[must_use]
    pub fn with_network(mut self) -> Self {
        self.snapshot.capabilities.insert(Capability::Network);
        if self.enforcement == Enforcement::FileSystemAndNetwork {
            self.enforcement = Enforcement::FileSystem;
        }

        self
    }

    /// Returns the isolation the selected executor reported when this
    /// configuration was built, which the model-facing `bash` tool
    /// description states. Reading it once keeps every advertisement built
    /// from one configuration identical.
    pub(crate) fn enforcement(&self) -> Enforcement {
        self.enforcement
    }

    /// Returns the resolved executable of the program
    /// [`Self::with_excluded_command`] excludes and the arguments to run it
    /// with, without a shell, when `command` is a simple command invoking
    /// it: plain words, which need no expansion.
    pub(crate) fn excluded_program(&self, command: &str) -> Option<(&Path, Vec<String>)> {
        // Printable ASCII only, so an approval displays exactly the words
        // that run: no Unicode spaces, line separators, bidirectional
        // overrides, or zero-width characters.
        if command.chars().any(|character| {
            !(character.is_ascii_graphic() || character == ' ' || character == '\t')
                || EXCLUDED_SYNTAX.contains(&character)
        }) {
            return None;
        }
        let mut words = command
            .split([' ', '\t'])
            .filter(|word| !word.is_empty())
            .map(str::to_owned);
        let executable = words
            .next()
            .and_then(|program| self.snapshot.excluded_commands.get(&program))?;

        Some((executable.as_path(), words.collect()))
    }

    /// Fails for a host executor that enforces isolation but would ignore
    /// hidden paths.
    fn require_hidden_paths(&self) -> Result<(), BashError> {
        match &self.executor {
            Some(executor)
                if executor.enforcement() != Enforcement::None
                    && !executor.supports_hidden_paths() =>
            {
                Err(BashError::InvalidPolicy)
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn fingerprint(&self) -> Value {
        json!(self.snapshot)
    }

    /// Returns this native configuration launched through the checked
    /// `launch` executables. Host-request and turn-option fingerprints come
    /// from the configuration the host gave; each command intent records
    /// this effective one, naming the executables that ran it.
    #[cfg(feature = "native-sandbox")]
    pub(crate) fn launched_through(&self, launch: crate::execution::NativeLaunch) -> Self {
        let mut configuration = self.clone();
        configuration.snapshot.launcher = Some(launch.launcher);
        configuration.snapshot.linux_bubblewrap = launch.bubblewrap;

        configuration
    }

    /// Returns this configuration with its commands run without isolation,
    /// which the model-facing `bash` description states and each command
    /// intent records through the unsandboxed executor's identity: the
    /// native fallback, and the configuration excluded commands run with.
    pub(crate) fn unsandboxed(&self) -> Self {
        let executor = UnsandboxedExecutor::without_isolation();
        let mut configuration = self.clone();
        configuration.snapshot.executor = executor.identity().to_string();
        configuration.snapshot.launcher = None;
        configuration.snapshot.linux_bubblewrap = None;
        configuration.enforcement = executor.enforcement();
        configuration.executor = Some(Arc::new(executor));

        configuration
    }
}

impl fmt::Debug for BashConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BashConfig")
            .field("policy", &self.snapshot)
            .finish_non_exhaustive()
    }
}

/// Executor instances compare through their recorded snapshot identity, so
/// equal policies with separately constructed equivalent executors are equal.
impl PartialEq for BashConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enforcement == other.enforcement
            && self.environment == other.environment
            && self.excluded_environment == other.excluded_environment
            && self.snapshot == other.snapshot
    }
}

impl Eq for BashConfig {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BashPolicySnapshot {
    pub(crate) bash: PathBuf,
    /// Capabilities granted beyond paths and environment values. Grants
    /// added after the first snapshots are skipped on write while unset,
    /// so recorded fingerprints of earlier policies stay stable.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) capabilities: BTreeSet<Capability>,
    pub(crate) capture_bytes: usize,
    pub(crate) environment_names: Vec<String>,
    /// [`BashConfig::with_excluded_command`] programs by name, with their
    /// resolved executables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) excluded_commands: BTreeMap<String, PathBuf>,
    /// [`BashConfig::with_excluded_environment`] names, never values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) excluded_environment_names: Vec<String>,
    /// Selected executor identity. Legacy snapshots predate executor
    /// selection and decode as the native default; the native identity is
    /// skipped on write so their recorded fingerprints stay stable.
    #[serde(
        default = "native_executor_identity",
        skip_serializing_if = "is_native_executor"
    )]
    pub(crate) executor: String,
    pub(crate) external_reads: Vec<PathBuf>,
    /// The branch [`BashConfig::with_git_writes`] grants. Snapshots recorded
    /// before branch-scoped Git writes hold [`Capability::GitWrites`]
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) git_branch: Option<String>,
    /// [`BashConfig::with_hidden_path`] paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) hidden_paths: Vec<PathBuf>,
    pub(crate) host_information: bool,
    pub(crate) launcher: Option<PathBuf>,
    pub(crate) linux_bubblewrap: Option<PathBuf>,
    pub(crate) revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) temporary_directory: Option<PathBuf>,
    pub(crate) timeout: Duration,
    /// [`BashConfig::with_unix_socket`] sockets, resolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) unix_sockets: Vec<PathBuf>,
    pub(crate) workspace_writes: Vec<PathBuf>,
    /// [`BashConfig::with_writable_root`] paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) writable_roots: Vec<PathBuf>,
}

impl BashPolicySnapshot {
    /// Returns whether the configuration grants `capability`.
    pub(crate) fn grants(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// A capability a configuration grants beyond paths and environment values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Capability {
    /// [`BashConfig::with_full_disk_read`].
    FullDiskRead,
    /// Git writes without a branch, which snapshots recorded before
    /// [`BashConfig::with_git_writes`] took one still hold; kept only so
    /// they decode, and never granted.
    GitWrites,
    /// [`BashConfig::with_network`].
    Network,
    /// [`BashConfig::with_system_reads`].
    SystemReads,
}

fn native_executor_identity() -> String {
    NATIVE_EXECUTOR.to_string()
}

fn is_native_executor(identity: &str) -> bool {
    identity == NATIVE_EXECUTOR
}

/// Validated shell source. Working directory, executable, grants, deadline,
/// and output budget are selected exclusively by the host.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "RawArguments")]
pub struct BashArguments {
    command: String,
}

impl BashArguments {
    /// Accepts 1–65536 bytes of nonempty, NUL-free shell source.
    ///
    /// # Errors
    /// Returns an error for invalid or oversized source before spawning.
    pub fn new(command: String) -> Result<Self, BashError> {
        if command.trim().is_empty() || command.len() > 65536 || command.contains('\0') {
            return Err(BashError::InvalidArguments);
        }

        Ok(Self { command })
    }

    /// Returns the validated shell source. Do not include it in telemetry.
    pub fn command(&self) -> &str {
        &self.command
    }
}

impl fmt::Debug for BashArguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BashArguments")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArguments {
    command: String,
}

impl TryFrom<RawArguments> for BashArguments {
    type Error = BashError;

    fn try_from(value: RawArguments) -> Result<Self, Self::Error> {
        Self::new(value.command)
    }
}

/// Shell execution or policy failure without command content or secrets.
#[derive(Clone, Copy, Debug, Deserialize, Error, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BashError {
    /// Invalid host policy.
    #[error("invalid Bash sandbox policy")]
    InvalidPolicy,
    /// Invalid shell source.
    #[error("invalid Bash arguments")]
    InvalidArguments,
    /// The selected executor cannot execute the requested policy or platform.
    #[error("Bash executor unavailable for this policy or platform")]
    Unavailable,
    /// Preparation, spawning, or supervision failed.
    #[error("sandboxed Bash execution failed")]
    Execution,
    /// Cleanup remains unconfirmed and requires owner-scoped reconciliation.
    #[error("sandboxed Bash cleanup remains unresolved")]
    Cleanup,
}

#[cfg(test)]
#[path = "bash_test.rs"]
mod tests;

fn valid_path(path: &Path, absolute: bool) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();

    !bytes.is_empty()
        && bytes.len() <= 4096
        && !bytes.contains(&0)
        && path.is_absolute() == absolute
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

/// Returns whether `environment` accepts `name` set to `value`: a valid,
/// new name and a value within the shared size bounds.
fn valid_variable(environment: &BTreeMap<String, String>, name: &str, value: &str) -> bool {
    !name.is_empty()
        && !name.contains(['=', '\0'])
        && !value.contains('\0')
        && name.len() <= 256
        && environment.len() < 64
        && environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            .saturating_add(name.len())
            .saturating_add(value.len())
            <= 65536
        && !environment.contains_key(name)
}

/// Returns whether `branch` is a branch name Git accepts, following
/// `git check-ref-format --branch`, without a `"`, which no sandbox profile
/// can quote, and at most [`MAX_BRANCH_BYTES`] long.
fn valid_branch(branch: &str) -> bool {
    let has_valid_characters = !branch.chars().any(|character| {
        character.is_ascii_control()
            || matches!(
                character,
                ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '"'
            )
    });
    // Case-insensitive file systems would also take `x.LOCK` for a lock file.
    let has_valid_components = branch.split('/').all(|component| {
        !component.is_empty()
            && !component.starts_with('.')
            && !Path::new(component)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"))
    });

    has_valid_characters
        && has_valid_components
        && branch.len() <= MAX_BRANCH_BYTES
        && branch != "@"
        && !branch.starts_with('-')
        && !branch.ends_with('.')
        && !branch.contains("..")
        && !branch.contains("@{")
}

fn has_git_component(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(b".git")
    })
}

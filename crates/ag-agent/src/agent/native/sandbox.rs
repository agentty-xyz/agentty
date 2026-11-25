//! Native sandbox grants for `bash` and the sandbox notices a run reports.
//!
//! Commands read the whole disk except credential stores, write only the
//! worktree's files and their session's temporary directory, never the
//! worktree's Git metadata, which Agentty commits to after each turn, and
//! keep network access, like the sandboxes of other coding agents. Where
//! the native sandbox cannot run, session commands run without it and the
//! run reports why so the session view can tell the user how to fix it,
//! while utility prompts refuse to run commands at all.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ag_contracts::{HostCacheSharing, TurnNotice};
use ag_harness::RunEventKind;
use ag_harness::bash::{BashConfig, BashError};
use ag_harness::event::Item;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};

use super::cargo_cache::TrustedCargoCache;
use super::config::{agentty_home, chatgpt_auth_file};

/// How session turns run `bash`: inside the native sandbox, and without it,
/// with a warning the session shows, on a host where it cannot run.
pub(crate) const SESSION_SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Unsandboxed,
};
/// How utility prompts run `bash`: only inside the native sandbox. They
/// report no notices, so where the sandbox cannot run they fail with its
/// remedy instead of running commands unseen.
pub(crate) const UTILITY_SANDBOX: Sandbox = Sandbox::Native {
    if_unavailable: IfUnavailable::Refuse,
};
/// Subdirectory of the temporary directory that commands use as
/// `CARGO_HOME`.
const CARGO_HOME_DIRECTORY: &str = "cargo";
/// Host Cargo configuration files, newest name first.
const CARGO_CONFIG_FILES: [&str; 2] = ["config.toml", "config"];
/// Entries of the sandbox `CARGO_HOME` that Cargo reads as configuration or
/// credentials, replaced before every turn whatever commands left there.
const CARGO_TRUSTED_FILES: [&str; 4] = ["config.toml", "config", "credentials.toml", "credentials"];
/// Entry of the temporary directory that a planted directory in place of a
/// trusted Cargo file moves to before it is removed.
const DISCARDED_CARGO_ENTRY: &str = ".agentty-discarded-cargo-entry";
/// Directories of the host's Cargo home that the sandbox `CARGO_HOME`
/// links to when sessions share the host's caches: downloaded crates,
/// registry indexes, and Git dependencies. Its programs (`bin`), its
/// configuration and credentials, and its install records are never
/// shared.
const CARGO_SHARED_DIRECTORIES: [&str; 2] = ["registry", "git"];
/// Cargo's cache lock files, shared with the directories so a session's
/// Cargo and the host's take the same locks before downloading or
/// extracting into them. `.global-cache`, Cargo's record of when each entry
/// was last used, stays per session, so its automatic cleaning would judge
/// shared entries by this session's use alone; sharing turns it off with
/// [`CARGO_AUTO_CLEAN`], leaving the shared cache to the host's own Cargo.
const CARGO_SHARED_LOCKS: [&str; 2] = [".package-cache", ".package-cache-mutate"];
/// Cargo's `cache.auto-clean-frequency` setting and the value that keeps a
/// session's Cargo from cleaning a shared cache.
const CARGO_AUTO_CLEAN: (&str, &str) = ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never");
/// Tool caches redirected into the temporary directory, by variable and
/// subdirectory: commands cannot write the shared caches in the home
/// directory, which host tools would later trust.
const CACHE_ENVIRONMENT: [(&str, &str); 6] = [
    ("CARGO_HOME", CARGO_HOME_DIRECTORY),
    ("GOCACHE", "go-build"),
    ("GOMODCACHE", "go-mod"),
    ("PIP_CACHE_DIR", "pip"),
    ("XDG_CACHE_HOME", "cache"),
    ("npm_config_cache", "npm"),
];
/// Directories of Agentty's data directory hidden from commands: the stored
/// provider API keys and sign-ins, the application database, with every
/// session's transcript and the remote-control identity, and the pasted
/// images of every session.
const AGENTTY_PRIVATE_DIRECTORIES: [&str; 3] = ["auth", "db", "tmp"];
/// Name the harness's user-facing remedies use for the host application.
const APPLICATION: &str = "the application";

/// Where the sandbox `CARGO_HOME` keeps downloaded crates and Git
/// dependencies. Resolved once per turn, so its sources stay mutually
/// exclusive: nothing ever writes a per-session or project cache into a
/// registry linked to the host's.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CargoCacheSource {
    /// Real directories of the session's own `CARGO_HOME`, with nothing
    /// copied in.
    PerSession,
    /// Real directories of the session's own `CARGO_HOME`, with copies of
    /// what the project's trusted cache holds for the worktree.
    Project(TrustedCargoCache),
    /// The host's Cargo home, whose registry, Git dependencies, and cache
    /// locks the sandbox `CARGO_HOME` links to.
    Host(PathBuf),
}

/// The places one run's commands may write and the Agentty files they may
/// not read.
pub(crate) struct CommandSandbox {
    /// Where the sandbox `CARGO_HOME` keeps its caches this turn.
    cargo: CargoCacheSource,
    /// Paths hidden from commands: the harness histories, the `ChatGPT`
    /// sign-in, and Agentty's private directories.
    hidden: Vec<PathBuf>,
    /// The host's dependency cache directories commands share.
    host_cache_directories: Vec<PathBuf>,
    /// The host's cache lock files commands share.
    host_cache_files: Vec<PathBuf>,
    /// Host caches the turn asked to share but keeps per session, named for
    /// the user.
    not_shared: Vec<String>,
    /// Cache variables pointing at the host's caches in place of their
    /// subdirectories of the temporary directory.
    shared_environment: BTreeMap<&'static str, PathBuf>,
    /// The session's own temporary directory, inside the hidden data root:
    /// `TMPDIR` and the tool caches.
    temporary_directory: PathBuf,
}

impl CommandSandbox {
    /// Prepares `temporary_directory`, which the host created inside
    /// `data_root` and commands may have written on earlier turns, for
    /// commands run in `workspace`: makes its `CARGO_HOME` a real directory,
    /// replaces the Cargo configuration and credential files commands could
    /// have planted with a link to the host's configuration, arranges its
    /// Cargo caches, and returns the sandbox, which also hides every
    /// project's trusted Cargo cache.
    ///
    /// With `host_caches` shared, the sandbox `CARGO_HOME` links to the
    /// host's Cargo registry, Git dependencies, and cache locks, which are
    /// created when missing, and the Go module and build caches, pip's
    /// cache, and npm's cache that already exist are granted in place. A
    /// cache is shared only where the native sandbox accepts it (see
    /// [`ShareLimits`]), which keeps it from refusing every command; the
    /// others stay per session and [`Self::notice`] names them. Otherwise,
    /// or when the host's Cargo caches cannot be shared, the session's own
    /// `CARGO_HOME` receives copies of what the project's `cargo_cache`,
    /// when there is one, holds for `workspace`'s `Cargo.lock`, and links
    /// an earlier turn made to the host's caches are removed.
    ///
    /// # Errors
    /// Returns an error when the Cargo directory cannot be made safe.
    pub(crate) async fn prepare(
        data_root: &Path,
        temporary_directory: PathBuf,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
        cargo_cache: Option<TrustedCargoCache>,
        host_caches: HostCacheSharing,
    ) -> Result<Self, String> {
        let host_config = Self::host_cargo_config(workspace, environment).await;
        if let Some(cargo_cache) = &cargo_cache {
            // The cache root must exist before the sandbox starts, so that
            // Bubblewrap masks it even when a fetch creates it meanwhile.
            let _ = tokio::fs::create_dir_all(cargo_cache.hidden_path()).await;
        }
        let mut sandbox = Self::new(data_root, temporary_directory.clone(), environment);
        if let Some(cargo_cache) = &cargo_cache {
            sandbox.hidden.push(cargo_cache.hidden_path().to_path_buf());
        }
        let host_cargo = Self::host_cargo_home(environment).filter(|_| host_caches.is_shared());
        let directory = temporary_directory;
        let worktree = workspace.to_path_buf();
        // The hidden paths include the data root, which holds the temporary
        // directory.
        let taken = std::iter::once(workspace.to_path_buf())
            .chain(sandbox.hidden.iter().cloned())
            .collect::<Vec<_>>();
        let limits_environment = environment.clone();
        let project_cache = cargo_cache.clone();
        let (cargo, limits) = tokio::task::spawn_blocking(move || {
            let mut limits = ShareLimits::new(&limits_environment, &taken);
            // Cargo's entries claim their place only when all are shared.
            let mut cargo_limits = limits.clone();
            let cargo = match host_cargo {
                Some(home) if Self::prepare_host_cargo(&home, &mut cargo_limits) => {
                    limits = cargo_limits;

                    CargoCacheSource::Host(home)
                }
                _ => project_cache.map_or(CargoCacheSource::PerSession, CargoCacheSource::Project),
            };

            Self::reset_cargo_home(&directory, host_config.as_deref(), &cargo, &worktree)
                .map(|()| (cargo, limits))
        })
        .await
        .map_err(std::io::Error::other)
        .flatten()
        .map_err(|error| format!("Failed to prepare the sandbox Cargo directory: {error}"))?;
        if let CargoCacheSource::Host(home) = &cargo {
            sandbox.host_cache_directories = CARGO_SHARED_DIRECTORIES
                .iter()
                .map(|name| home.join(name))
                .collect();
            sandbox.host_cache_files = CARGO_SHARED_LOCKS
                .iter()
                .map(|name| home.join(name))
                .collect();
        } else if host_caches.is_shared() {
            sandbox.not_shared.push("Cargo".to_string());
        }
        sandbox.cargo = cargo;
        if host_caches.is_shared() {
            sandbox.share_tool_caches(environment, limits).await;
        }

        Ok(sandbox)
    }

    /// Returns whether commands share any of the host's caches.
    pub(crate) fn shares_host_caches(&self) -> bool {
        !self.host_cache_directories.is_empty() || !self.host_cache_files.is_empty()
    }

    /// Returns the notice naming the host caches the turn asked to share
    /// but keeps per session, if any.
    pub(crate) fn notice(&self) -> Option<TurnNotice> {
        (!self.not_shared.is_empty()).then(|| TurnNotice::HostCachesNotShared {
            caches: self.not_shared.clone(),
        })
    }

    /// Describes commands that write `temporary_directory` and cannot read
    /// `data_root`, other than that directory, the `ChatGPT` sign-in
    /// `environment` names, or Agentty's stored API keys, application
    /// database and pasted images, which hold every provider key, every
    /// session's transcript and the remote-control identity.
    pub(crate) fn new(
        data_root: &Path,
        temporary_directory: PathBuf,
        environment: &BTreeMap<String, String>,
    ) -> Self {
        let agentty_files = agentty_home(environment)
            .into_iter()
            .flat_map(|home| AGENTTY_PRIVATE_DIRECTORIES.map(|directory| home.join(directory)));
        let hidden = std::iter::once(data_root.to_path_buf())
            .chain(chatgpt_auth_file(environment))
            .chain(agentty_files)
            .filter_map(|path| std::path::absolute(path).ok())
            .collect();

        Self {
            cargo: CargoCacheSource::PerSession,
            hidden,
            host_cache_directories: Vec::new(),
            host_cache_files: Vec::new(),
            not_shared: Vec::new(),
            shared_environment: BTreeMap::new(),
            temporary_directory,
        }
    }

    /// Adds Agentty's grants to `config`: full-disk reads except credential
    /// stores and the hidden paths; writes to the worktree, whose Git
    /// metadata the harness keeps read-only, and the temporary directory,
    /// which also holds the tool caches, or the host's caches the sandbox
    /// shares; and network access. No Unix-domain socket is granted, not
    /// even the SSH agent's, whose keys would sign for any host the network
    /// reaches; Agentty commits and pushes outside the sandbox itself.
    ///
    /// # Errors
    /// Returns the grant the harness rejects.
    pub(crate) fn grant(&self, config: BashConfig) -> Result<BashConfig, BashError> {
        let mut config = config
            .with_full_disk_read()?
            .with_write(PathBuf::from("."))?
            .with_network()
            .with_temporary_directory(self.temporary_directory.clone())?;
        for path in &self.hidden {
            config = config.with_hidden_path(path.clone())?;
        }
        for path in &self.host_cache_directories {
            config = config.with_host_cache_directory(path.clone())?;
        }
        for path in &self.host_cache_files {
            config = config.with_host_cache_file(path.clone())?;
        }
        for (name, directory) in CACHE_ENVIRONMENT {
            let value = self
                .shared_environment
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.temporary_directory.join(directory));
            config = config.with_environment(name.to_string(), value.display().to_string())?;
        }
        if matches!(self.cargo, CargoCacheSource::Host(_)) {
            let (name, value) = CARGO_AUTO_CLEAN;
            config = config.with_environment(name.to_string(), value.to_string())?;
        }

        Ok(config)
    }

    /// Grants the Go, pip, and npm caches of the host that `limits` allow
    /// and points their variables there. A missing cache stays per session
    /// silently; one that exists but is a link or file, or lies where
    /// `limits` refuse, such as a cache under a home's `Library` or inside
    /// another shared cache, stays per session and is named in the notice.
    /// Links are never followed, since a command could have planted one.
    async fn share_tool_caches(
        &mut self,
        environment: &BTreeMap<String, String>,
        mut limits: ShareLimits,
    ) {
        for (variable, name, cache) in Self::host_tool_caches(environment, std::env::consts::OS) {
            let Ok(metadata) = tokio::fs::symlink_metadata(&cache).await else {
                continue;
            };
            let resolved = tokio::fs::canonicalize(&cache).await.ok();
            // The resolved path is granted, which the sandbox requires to
            // match the directory's own name on disk, letter case included.
            match resolved {
                Some(resolved) if metadata.is_dir() && limits.claim(&resolved) => {
                    self.host_cache_directories.push(resolved.clone());
                    self.shared_environment.insert(variable, resolved);
                }
                _ => self.not_shared.push(name.to_string()),
            }
        }
    }

    /// Returns the host's Cargo home: an absolute `CARGO_HOME`, else
    /// `~/.cargo`.
    fn host_cargo_home(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
        environment
            .get("CARGO_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| Path::new(home).join(".cargo"))
            })
    }

    /// Makes the host Cargo home's shared directories and lock files exist,
    /// as Cargo itself would, and returns whether each is a real directory
    /// or a regular file with one link, not a link itself, that `limits`
    /// allow, so the sandbox can link to it. The entries shared are claimed
    /// in `limits`.
    fn prepare_host_cargo(home: &Path, limits: &mut ShareLimits) -> bool {
        // Nothing is created where the sandbox could not share it. The home
        // itself holds `bin`, usually on `PATH`, so only its shared entries
        // must stay clear of the other limits.
        limits.in_home(&ShareLimits::resolve_nearest(home))
            && std::fs::create_dir_all(home).is_ok()
            && CARGO_SHARED_DIRECTORIES
                .iter()
                .all(|name| Self::host_entry(&home.join(name), true, limits))
            && CARGO_SHARED_LOCKS
                .iter()
                .all(|name| Self::host_entry(&home.join(name), false, limits))
    }

    /// Creates the missing directory or empty file at `path` and returns
    /// whether it is one, without following a final link, with a single
    /// link for a file, and whether `limits` allow where it resolves,
    /// claiming it there.
    fn host_entry(path: &Path, directory: bool, limits: &mut ShareLimits) -> bool {
        Self::create_host_entry(path, directory)
            && std::fs::canonicalize(path).is_ok_and(|resolved| limits.claim(&resolved))
    }

    /// Creates the missing directory or empty file at `path` and returns
    /// whether it is one, without following a final link, with a single
    /// link for a file.
    fn create_host_entry(path: &Path, directory: bool) -> bool {
        if std::fs::symlink_metadata(path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            // A failure leaves the entry missing, which the check refuses.
            let _ = if directory {
                std::fs::create_dir(path)
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map(drop)
            };
        }

        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            if directory {
                metadata.is_dir()
            } else {
                // Writing a file with another link would write that name too.
                metadata.is_file() && metadata.nlink() == 1
            }
        })
    }

    /// Returns where the host keeps the Go module and build caches, pip's
    /// cache, and npm's cache on `platform`, by the variable that moves
    /// each: its value when absolute, else the tool's documented default.
    /// The variables Agentty sets go unread in the commands' environment,
    /// so `XDG_CACHE_HOME`, which covers every tool, is never shared. Each
    /// cache comes with its variable and its name for the user.
    fn host_tool_caches(
        environment: &BTreeMap<String, String>,
        platform: &str,
    ) -> Vec<(&'static str, &'static str, PathBuf)> {
        let absolute = |name: &str| {
            environment
                .get(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let home = absolute("HOME");
        let xdg_cache = absolute("XDG_CACHE_HOME");
        // Go's `os.UserCacheDir`: `~/Library/Caches` on macOS, else
        // `XDG_CACHE_HOME` or `~/.cache`.
        let user_cache = if platform == "macos" {
            home.as_ref().map(|home| home.join("Library/Caches"))
        } else {
            xdg_cache
                .clone()
                .or_else(|| home.as_ref().map(|home| home.join(".cache")))
        };
        let go_path = environment
            .get("GOPATH")
            .and_then(|value| std::env::split_paths(value).next())
            .filter(|path| path.is_absolute())
            .or_else(|| home.as_ref().map(|home| home.join("go")));
        let caches = [
            (
                "GOMODCACHE",
                "Go modules",
                absolute("GOMODCACHE").or_else(|| go_path.map(|path| path.join("pkg/mod"))),
            ),
            (
                "GOCACHE",
                "Go build",
                absolute("GOCACHE").or_else(|| user_cache.map(|cache| cache.join("go-build"))),
            ),
            (
                "PIP_CACHE_DIR",
                "pip",
                absolute("PIP_CACHE_DIR").or_else(|| {
                    // pip follows `XDG_CACHE_HOME` on every platform.
                    xdg_cache.map(|cache| cache.join("pip")).or_else(|| {
                        let base = if platform == "macos" {
                            "Library/Caches/pip"
                        } else {
                            ".cache/pip"
                        };
                        home.as_ref().map(|home| home.join(base))
                    })
                }),
            ),
            (
                "npm_config_cache",
                "npm",
                absolute("npm_config_cache")
                    .or_else(|| absolute("NPM_CONFIG_CACHE"))
                    .or_else(|| home.as_ref().map(|home| home.join(".npm"))),
            ),
        ];

        caches
            .into_iter()
            .filter_map(|(variable, name, cache)| Some((variable, name, cache?)))
            .collect()
    }

    /// Makes the `CARGO_HOME` of `temporary_directory` a real directory
    /// without the Cargo configuration and credential files commands may
    /// have planted, then links `host_config` into it, and arranges its
    /// caches for `source`: links to the host's, or copies of what the
    /// project's cache holds for `worktree`'s `Cargo.lock`.
    ///
    /// Only `temporary_directory` is resolved by path: commands cannot
    /// replace it, because its parent is hidden from them. Its Cargo
    /// directory is opened once without following links and every later
    /// change is made relative to that handle, so a link that a surviving
    /// command swaps in meanwhile cannot redirect them outside.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being made safe.
    fn reset_cargo_home(
        temporary_directory: &Path,
        host_config: Option<&Path>,
        source: &CargoCacheSource,
        worktree: &Path,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(temporary_directory)?;
        let temporary = fs::open(temporary_directory, Self::directory_flags(), Mode::empty())?;
        let planted = fs::statat(&temporary, CARGO_HOME_DIRECTORY, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| !FileType::from_raw_mode(stat.st_mode).is_dir());
        if planted {
            fs::unlinkat(&temporary, CARGO_HOME_DIRECTORY, AtFlags::empty())?;
        }
        // An existing directory is fine; any other failure fails the open.
        let _ = fs::mkdirat(&temporary, CARGO_HOME_DIRECTORY, Mode::RWXU);
        let cargo = fs::openat(
            &temporary,
            CARGO_HOME_DIRECTORY,
            Self::directory_flags(),
            Mode::empty(),
        )?;

        Self::clear_cargo_home(temporary_directory, &temporary, &cargo, host_config)?;
        Self::link_shared_cargo(temporary_directory, &temporary, &cargo, source)?;
        // Copied only after the links are gone, so nothing lands in the
        // host's registry.
        if let CargoCacheSource::Project(cargo_cache) = source {
            cargo_cache.copy_into(&cargo, worktree);
        }

        Ok(())
    }

    /// Returns the flags that open a directory handle without following a
    /// final link.
    pub(super) fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    /// Removes the trusted Cargo files from the open `cargo` directory and
    /// links `host_config` in their place. A planted directory is first
    /// moved out of `cargo` into `temporary`, the handle of
    /// `temporary_directory`, so its removal resolves no command-writable
    /// path but its own name.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps an entry from being removed.
    fn clear_cargo_home(
        temporary_directory: &Path,
        temporary: &OwnedFd,
        cargo: &OwnedFd,
        host_config: Option<&Path>,
    ) -> std::io::Result<()> {
        for name in CARGO_TRUSTED_FILES {
            match fs::statat(cargo, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode).is_dir() => {
                    Self::discard(temporary_directory, temporary, cargo, name)?;
                }
                Ok(_) => fs::unlinkat(cargo, name, AtFlags::empty())?,
                Err(_) => {}
            }
        }
        if let Some(config) = host_config {
            // Best effort: without the link only those settings are lost.
            let _ = fs::symlinkat(config, cargo, CARGO_CONFIG_FILES[0]);
        }

        Ok(())
    }

    /// Moves the directory `name` out of the open `cargo` directory into
    /// `temporary`, the handle of `temporary_directory`, and removes it
    /// there, so its removal resolves no command-writable path but its own
    /// name.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being removed.
    fn discard(
        temporary_directory: &Path,
        temporary: &OwnedFd,
        cargo: &OwnedFd,
        name: &str,
    ) -> std::io::Result<()> {
        let discarded = temporary_directory.join(DISCARDED_CARGO_ENTRY);
        Self::remove_discarded(&discarded)?;
        fs::renameat(cargo, name, temporary, DISCARDED_CARGO_ENTRY)?;

        Self::remove_discarded(&discarded)
    }

    /// Links the shared cache entries of the open `cargo` directory to
    /// those of the host's Cargo home with [`CargoCacheSource::Host`],
    /// replacing whatever commands or an earlier turn left there. With
    /// either other source, it removes only links, so a
    /// session that stops sharing falls back to its own caches and keeps
    /// those it already has.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps an entry from being replaced.
    fn link_shared_cargo(
        temporary_directory: &Path,
        temporary: &OwnedFd,
        cargo: &OwnedFd,
        source: &CargoCacheSource,
    ) -> std::io::Result<()> {
        for name in CARGO_SHARED_DIRECTORIES.iter().chain(&CARGO_SHARED_LOCKS) {
            let existing = fs::statat(cargo, *name, AtFlags::SYMLINK_NOFOLLOW)
                .ok()
                .map(|stat| FileType::from_raw_mode(stat.st_mode));
            let host = match source {
                CargoCacheSource::Host(host) => Some(host),
                CargoCacheSource::PerSession | CargoCacheSource::Project(_) => None,
            };
            match (existing, host) {
                (Some(FileType::Directory), Some(_)) => {
                    Self::discard(temporary_directory, temporary, cargo, name)?;
                }
                (Some(FileType::Symlink), _) | (Some(_), Some(_)) => {
                    fs::unlinkat(cargo, *name, AtFlags::empty())?;
                }
                _ => {}
            }
            if let Some(host) = host {
                fs::symlinkat(host.join(name), cargo, *name)?;
            }
        }

        Ok(())
    }

    /// Removes `path`, an entry of the temporary directory, whatever it is:
    /// a link is removed itself, and the standard library removes a
    /// directory tree without following the links inside it.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps `path` from being removed.
    fn remove_discarded(path: &Path) -> std::io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(_) => Ok(()),
        }
    }

    /// Returns the host's Cargo configuration file, unless Cargo finds it
    /// anyway: it reads `.cargo/config.toml` in every ancestor of its
    /// working directory, which covers the default `~/.cargo` above a
    /// workspace inside the home directory.
    async fn host_cargo_config(
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Option<PathBuf> {
        let cargo_home = Self::host_cargo_home(environment)?;
        let found_from_workspace = cargo_home.file_name() == Some(".cargo".as_ref())
            && cargo_home
                .parent()
                .is_some_and(|parent| workspace.starts_with(parent));
        if found_from_workspace {
            return None;
        }
        for name in CARGO_CONFIG_FILES {
            let config = cargo_home.join(name);
            if tokio::fs::metadata(&config)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Some(config);
            }
        }

        None
    }
}

/// Where a host cache may lie for the native sandbox to share it, mirroring
/// the write-grant rules the sandbox enforces: strictly inside the home
/// directory, which keeps it out of the system and toolchain directories,
/// but outside the home's `Library`, which holds the login items and
/// preferences macOS runs and reads; clear of every directory on `PATH`,
/// where planted programs would run; without a `.git` component; and
/// overlapping neither the workspace, the hidden paths, which include
/// Agentty's data directory with the temporary directory, nor another
/// shared cache.
#[derive(Clone)]
struct ShareLimits {
    /// The resolved home directory, when known.
    home: Option<PathBuf>,
    /// The resolved absolute directories on `PATH`.
    program_directories: Vec<PathBuf>,
    /// The resolved paths a cache must not overlap: the workspace, the
    /// hidden paths, and the caches already shared.
    taken: Vec<PathBuf>,
}

impl ShareLimits {
    /// Reads the limits from the host environment `environment`, with
    /// `taken` the paths no cache may overlap.
    fn new(environment: &BTreeMap<String, String>, taken: &[PathBuf]) -> Self {
        let resolve = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
        let home = environment
            .get("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(resolve);
        let program_directories = environment
            .get("PATH")
            .into_iter()
            .flat_map(std::env::split_paths)
            .filter(|directory| directory.is_absolute())
            .map(resolve)
            .collect();
        let taken = taken
            .iter()
            .map(|path| Self::resolve_nearest(path))
            .collect();

        Self {
            home,
            program_directories,
            taken,
        }
    }

    /// Returns whether the resolved cache path `cache` may be shared and,
    /// when it may, records it so no later cache overlaps it.
    fn claim(&mut self, cache: &Path) -> bool {
        let allowed = self.allows(cache);
        if allowed {
            self.taken.push(cache.to_path_buf());
        }

        allowed
    }

    /// Returns the absolute `path` with links resolved, or, when it is
    /// missing, its nearest existing ancestor resolved followed by the rest
    /// of it; an empty path when nothing resolves.
    fn resolve_nearest(path: &Path) -> PathBuf {
        path.ancestors()
            .find_map(|ancestor| {
                let base = std::fs::canonicalize(ancestor).ok()?;

                Some(base.join(path.strip_prefix(ancestor).ok()?))
            })
            .unwrap_or_default()
    }

    /// Returns whether the resolved cache path `cache` may be shared.
    fn allows(&self, cache: &Path) -> bool {
        let git = cache.components().any(|part| {
            part.as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        });

        self.in_home(cache)
            && !git
            && !self
                .program_directories
                .iter()
                .chain(&self.taken)
                .any(|limit| cache.starts_with(limit) || limit.starts_with(cache))
    }

    /// Returns whether the resolved `path` lies strictly inside the home
    /// directory but outside its `Library`.
    fn in_home(&self, path: &Path) -> bool {
        self.home.as_ref().is_some_and(|home| {
            path.starts_with(home) && path != home && !path.starts_with(home.join("Library"))
        })
    }
}

/// Runs the native sandbox helper when a harness started this process as
/// one and returns its exit code, which `main` must return at once;
/// otherwise returns `None` and makes this executable the helper of every
/// [`super::NativeHarnessConfig`] turn.
///
/// Call it first in `main`, before creating a runtime, threads, or anything
/// else, because a helper process must not run host code. Without the
/// call, commands run without the sandbox and every turn reports it.
#[must_use = "a helper process must return this exit code from `main` at once"]
pub fn dispatch_sandbox_helper() -> Option<ExitCode> {
    ag_harness::sandbox::dispatch()
}

/// Returns what `event` reports about the sandbox, if anything.
pub(crate) fn notice(event: &RunEventKind) -> Option<TurnNotice> {
    match event {
        RunEventKind::SandboxUnavailable(warning) => Some(unavailable(warning)),
        RunEventKind::ItemCompleted {
            item: Item::ToolCall(call),
            ..
        } if call.likely_sandbox_denial => Some(TurnNotice::LikelySandboxDenial),
        _ => None,
    }
}

/// Words `warning` for Agentty's users: whether commands still run
/// sandboxed, what is missing, and the fix, naming Agentty.
fn unavailable(warning: &SandboxWarning) -> TurnNotice {
    let message = match warning.reason {
        SandboxUnavailableReason::UnixSocketsUnsupported => warning.message.clone(),
        _ => format!("Commands run without the sandbox. {}", warning.message),
    };

    TurnNotice::SandboxUnavailable {
        message,
        remedy: warning.remedy.replace(APPLICATION, "Agentty"),
    }
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;

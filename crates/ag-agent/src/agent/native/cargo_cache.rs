//! Per-project Cargo download cache that sandboxed session commands reuse
//! without ever reaching its files through their own paths.
//!
//! Agentty fills the cache outside the sandbox with a fetch that runs no
//! model output and reads no session-written manifest or configuration: it
//! parses the worktree's `Cargo.lock` as data, keeps only its crates.io
//! packages, and runs `cargo fetch` for a synthetic workspace that pins
//! exactly those. Session commands cannot read the cache at all. The fetch
//! records the archives it was asked for and stored only after it ends,
//! and skips projects whose Cargo configuration replaces crates.io. Preparing
//! a session's sandbox then copies the recorded archives its own lockfile
//! names into the session's `CARGO_HOME`, each verified against the
//! lockfile's checksum, so a session that edits its copies, or runs without
//! the sandbox, changes nothing another session uses.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt as _;

use super::cargo_lockfile::{LockedPackage, Lockfile};
use super::config::is_credential;
use super::sandbox::CommandSandbox;

/// Hexadecimal characters of the project key: the start of the SHA-256 of
/// the project's main checkout path.
const PROJECT_KEY_LENGTH: usize = 32;
/// Hexadecimal characters of the toolchain and configuration variant.
const VARIANT_LENGTH: usize = 16;
/// Cache subdirectory that the fetch uses as `CARGO_HOME`, which sessions
/// copy from.
const HOME_DIRECTORY: &str = "home";
/// Cache subdirectory hidden from session commands: the synthetic
/// workspace and the fetch's target directory.
const PRIVATE_DIRECTORY: &str = "private";
/// Subdirectory of the private directory holding the synthetic workspace,
/// where the fetch runs.
const WORK_DIRECTORY: &str = "work";
/// Subdirectory of the private directory that the fetch uses as
/// `CARGO_TARGET_DIR`, so that Cargo writes nothing elsewhere.
const TARGET_DIRECTORY: &str = "target";
/// Cache file naming the last finished fetch.
const GENERATION_FILE: &str = "generation";
/// First line of a generation file.
const GENERATION_HEADER: &str = "agentty-cargo-cache 2";
/// File of a session's `CARGO_HOME` naming the generation and lockfile it
/// last copied.
const SESSION_GENERATION_FILE: &str = ".agentty-cache-generation";
/// Suffix of a session copy until its checksum is verified.
const PARTIAL_SUFFIX: &str = ".agentty-partial";
/// Host Cargo configuration file name inside the cache `CARGO_HOME`.
const CONFIG_FILE: &str = "config.toml";
/// Cargo configuration file names in a `.cargo` directory, both read.
const CARGO_CONFIG_FILES: [&str; 2] = ["config.toml", "config"];
/// Largest Cargo configuration file read.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// Deadline for one Cargo run: the version check or the fetch.
const CARGO_TIMEOUT: Duration = Duration::from_secs(300);
/// Registry directories of a `CARGO_HOME` holding crate archives.
const REGISTRY_CACHE: &str = "registry/cache";
/// Registry index directories of a `CARGO_HOME`.
const REGISTRY_INDEX: &str = "registry/index";
/// Index file that names a sparse registry's download location.
const INDEX_CONFIG: &str = "config.json";
/// Host variables never passed to the fetch: agent sockets, prompts, and
/// commands that could hand it credentials, and the variables it sets.
const WITHHELD_VARIABLES: [&str; 8] = [
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "GIT_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
];
/// Prefixes of host variables never passed to the fetch: Git configuration
/// and credential helpers, and Cargo registry credentials.
const WITHHELD_PREFIXES: [&str; 5] = [
    "CARGO_CREDENTIAL_",
    "CARGO_REGISTRIES_",
    "CARGO_REGISTRY_",
    "GCM_",
    "GIT_CONFIG",
];

/// The Cargo download cache of one project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedCargoCache {
    /// Directory of every project's cache, hidden from session commands.
    cache_root: PathBuf,
    /// This project's cache.
    root: PathBuf,
}

impl TrustedCargoCache {
    /// Returns the cache, under `cache_root`, of the project whose main
    /// checkout is `main_checkout`.
    pub fn for_project(cache_root: &Path, main_checkout: &Path) -> Self {
        let digest = hex::encode(Sha256::digest(main_checkout.as_os_str().as_bytes()));

        Self {
            cache_root: cache_root.to_path_buf(),
            root: cache_root.join(&digest[..PROJECT_KEY_LENGTH]),
        }
    }

    /// Fetches the crates.io packages that `worktree`'s `Cargo.lock` pins
    /// and the cache lacks, calling `on_fetch` first when it fetches.
    ///
    /// Only the lockfile is read, as data. The fetch runs the host's
    /// `cargo` executable, which `environment`'s `PATH` resolves for a bare
    /// name, with that environment minus credentials, agent sockets, and
    /// Git configuration, the host's Cargo configuration, the cache as
    /// `CARGO_HOME`, and a synthetic workspace that pins exactly those
    /// packages, so that no path, Git, or other registry source is reached.
    /// Cargo authenticates only with tokens, of which it gets none. A
    /// change of Cargo version or host configuration fetches again.
    ///
    /// Nothing is fetched when the Cargo configuration the fetch would read
    /// replaces crates.io, which could send it to a mirror that serves
    /// crates by the user's credentials, or includes other configuration
    /// files; such projects keep downloading per session.
    ///
    /// # Errors
    /// Returns an error when the cache cannot be written, Cargo fails or
    /// times out, or the fetch left a package uncached; what was fetched
    /// stays recorded.
    pub async fn refresh(
        &self,
        cargo: &str,
        worktree: &Path,
        environment: &BTreeMap<String, String>,
        on_fetch: impl FnOnce() + Send,
    ) -> io::Result<()> {
        let lockfile_worktree = worktree.to_path_buf();
        let lockfile = tokio::task::spawn_blocking(move || Lockfile::read(&lockfile_worktree))
            .await
            .map_err(io::Error::other)?;
        let Some(lockfile) = lockfile.filter(|lockfile| !lockfile.packages.is_empty()) else {
            return Ok(());
        };
        let home = self.root.join(HOME_DIRECTORY);
        let private = self.root.join(PRIVATE_DIRECTORY);
        for directory in [
            &home,
            &private.join(WORK_DIRECTORY),
            &private.join(TARGET_DIRECTORY),
        ] {
            tokio::fs::create_dir_all(directory).await?;
        }
        let host_config = CommandSandbox::host_cargo_config(&private, environment).await;
        let work = private.join(WORK_DIRECTORY);
        let configs = work
            .ancestors()
            .flat_map(|directory| {
                CARGO_CONFIG_FILES.map(|name| directory.join(".cargo").join(name))
            })
            .chain(host_config.clone())
            .collect::<Vec<_>>();
        if tokio::task::spawn_blocking(move || Self::replaces_crates_io(&configs))
            .await
            .map_err(io::Error::other)?
        {
            tracing::debug!("Cargo configuration replaces crates.io; not caching dependencies");

            return Ok(());
        }
        let fetch_environment = self.fetch_environment(environment);
        let variant =
            Self::variant(cargo, &private, &fetch_environment, host_config.as_deref()).await?;
        let previous = Generation::load(&self.root).await;
        let covered = previous
            .as_ref()
            .filter(|generation| generation.variant == variant)
            .map(|generation| generation.missing.clone());
        if let Some(missing) = covered
            && Self::uncached(&home, &lockfile.packages, &missing)
                .await
                .is_empty()
        {
            return Ok(());
        }

        on_fetch();
        self.fetch(cargo, &lockfile, &fetch_environment, host_config.as_deref())
            .await?;
        let missing = Self::uncached(&home, &lockfile.packages, &[]).await;
        let scan_home = home.clone();
        let requested = lockfile.packages.clone();
        let mut files =
            tokio::task::spawn_blocking(move || Generation::scan(&scan_home, &requested))
                .await
                .map_err(io::Error::other)?;
        let number = previous.as_ref().map_or(0, |generation| generation.number) + 1;
        let earlier = previous.filter(|generation| generation.variant == variant);
        files.extend(earlier.into_iter().flat_map(|generation| generation.files));
        files.sort();
        files.dedup();
        Generation {
            files,
            missing: missing.clone(),
            number,
            variant,
        }
        .write(&self.root)
        .await?;
        if !missing.is_empty() {
            return Err(io::Error::other(format!(
                "cargo fetch left {} packages uncached",
                missing.len()
            )));
        }

        Ok(())
    }

    /// Returns the directory of every project's cache, hidden from session
    /// commands: Agentty copies what sessions need, so commands neither read
    /// what the fetch stored nor hold Cargo's locks on it.
    pub(crate) fn hidden_path(&self) -> &Path {
        &self.cache_root
    }

    /// Copies into `cargo`, the open handle of a session's `CARGO_HOME`,
    /// the recorded archives and index metadata that `worktree`'s
    /// `Cargo.lock` names, keeping every entry the session already has.
    ///
    /// Each archive is copied under a temporary name, kept only when its
    /// SHA-256 matches the lockfile's checksum, and then linked into place,
    /// so a truncated or tampered file never appears under the name Cargo
    /// trusts. Commands may have rewritten `cargo` on earlier turns, so
    /// every change is made relative to handles opened without following
    /// links: a link a command planted in place of a directory skips the
    /// files below it. Nothing happens before a fetch finished or while the
    /// session already copied the current generation for the same
    /// lockfile. Failures are ignored: a missed entry is downloaded by the
    /// session.
    pub(crate) fn copy_into(&self, cargo: &OwnedFd, worktree: &Path) {
        let Some(generation) = Generation::read(&self.root) else {
            return;
        };
        let Some(lockfile) = Lockfile::read(worktree) else {
            return;
        };
        let marker = format!("{}\n{}", generation.number, lockfile.digest);
        if Self::session_generation(cargo).as_deref() == Some(marker.as_str()) {
            return;
        }
        let home = self.root.join(HOME_DIRECTORY);
        let recorded = generation
            .files
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut archives = HashMap::<&str, Vec<(&str, &str)>>::new();
        let mut indexes = BTreeSet::new();
        for file in &generation.files {
            if file.starts_with(REGISTRY_CACHE)
                && let Some((parent, name)) = file.rsplit_once('/')
            {
                archives.entry(name).or_default().push((file, parent));
            }
            if let Some(index) = file.strip_prefix(REGISTRY_INDEX)
                && let Some(directory) = index.trim_start_matches('/').split('/').next()
            {
                indexes.insert(directory);
            }
        }
        let mut wanted = indexes
            .iter()
            .map(|index| format!("{REGISTRY_INDEX}/{index}/{INDEX_CONFIG}"))
            .collect::<Vec<_>>();
        let mut directories = SessionDirectories::new(cargo);
        for package in &lockfile.packages {
            let name = package.archive_name();
            for (file, parent) in archives.get(name.as_str()).into_iter().flatten() {
                if let Some(directory) = directories.open(parent) {
                    let _ =
                        Self::copy_archive(&home.join(file), directory, &name, &package.checksum);
                }
            }
            wanted.extend(
                indexes
                    .iter()
                    .map(|index| format!("{REGISTRY_INDEX}/{index}/{}", package.index_entry())),
            );
        }
        for file in wanted
            .iter()
            .filter(|file| recorded.contains(file.as_str()))
        {
            if let Some((parent, name)) = file.rsplit_once('/')
                && let Some(directory) = directories.open(parent)
            {
                let _ = Self::copy_absent(&home.join(file), directory, name);
            }
        }
        let _ = Self::record_session_generation(cargo, &marker);
    }

    /// Returns the host `environment` the fetch runs with: without
    /// credentials, agent sockets, prompts, and Git configuration, and with
    /// the cache as `CARGO_HOME` and target directory, Cargo's own Git
    /// client, and no terminal prompts.
    fn fetch_environment(
        &self,
        environment: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let private = self.root.join(PRIVATE_DIRECTORY);
        let mut fetch_environment = environment
            .iter()
            .filter(|(name, _)| {
                !WITHHELD_VARIABLES.contains(&name.as_str())
                    && !WITHHELD_PREFIXES
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
                    && !is_credential(name)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in [
            (
                "CARGO_HOME",
                self.root.join(HOME_DIRECTORY).display().to_string(),
            ),
            (
                "CARGO_TARGET_DIR",
                private.join(TARGET_DIRECTORY).display().to_string(),
            ),
            ("CARGO_NET_GIT_FETCH_WITH_CLI", "false".to_string()),
            // Only tokens, from the cache's own `CARGO_HOME`, which holds
            // none, or from variables, all withheld.
            (
                "CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS",
                "cargo:token".to_string(),
            ),
            ("GIT_TERMINAL_PROMPT", "0".to_string()),
        ] {
            fetch_environment.insert(name.to_string(), value);
        }

        fetch_environment
    }

    /// Returns whether one of the Cargo configuration files `configs`
    /// replaces crates.io or includes other files. A file that exists but
    /// cannot be read or parsed counts as replacing it.
    fn replaces_crates_io(configs: &[PathBuf]) -> bool {
        configs.iter().any(|config| {
            let Ok(file) = std::fs::File::open(config) else {
                return config.exists();
            };
            let mut contents = String::new();
            let parsed = std::io::Read::take(file, MAX_CONFIG_BYTES)
                .read_to_string(&mut contents)
                .ok()
                .and_then(|_| contents.parse::<toml::Table>().ok());

            parsed.is_none_or(|table| {
                table.contains_key("include")
                    || table
                        .get("source")
                        .and_then(|source| source.get("crates-io"))
                        .is_some_and(|crates_io| crates_io.get("replace-with").is_some())
            })
        })
    }

    /// Returns the variant that a fetch with `cargo`'s version and the
    /// host's Cargo configuration `host_config` produces: Cargo names its
    /// registry directories by version and configured sources.
    ///
    /// # Errors
    /// Returns an error when `cargo -V` cannot run, fails, or times out.
    async fn variant(
        cargo: &str,
        directory: &Path,
        environment: &BTreeMap<String, String>,
        host_config: Option<&Path>,
    ) -> io::Result<String> {
        let version = Self::run_cargo(cargo, "-V", directory, environment).await?;
        let mut hasher = Sha256::new();
        hasher.update(&version);
        if let Some(config) = host_config {
            hasher.update(config.as_os_str().as_bytes());
            let mut contents = Vec::new();
            tokio::fs::File::open(config)
                .await?
                .take(MAX_CONFIG_BYTES)
                .read_to_end(&mut contents)
                .await?;
            hasher.update(&contents);
        }

        Ok(hex::encode(hasher.finalize())[..VARIANT_LENGTH].to_string())
    }

    /// Returns the archive names of `packages` that `home` holds in no
    /// registry directory, except those listed as `missing` already.
    async fn uncached(home: &Path, packages: &[LockedPackage], missing: &[String]) -> Vec<String> {
        let registry_cache = home.join(REGISTRY_CACHE);
        let wanted = packages
            .iter()
            .map(LockedPackage::archive_name)
            .filter(|archive| !missing.contains(archive))
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            let registries = Generation::directories(&registry_cache);
            wanted
                .into_iter()
                .filter(|archive| {
                    !registries.iter().any(|registry| {
                        std::fs::symlink_metadata(registry.join(archive))
                            .is_ok_and(|metadata| metadata.is_file())
                    })
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Writes the synthetic workspace for `lockfile` and runs `cargo fetch`
    /// in it with the host's configuration `host_config` linked into the
    /// cache `CARGO_HOME`.
    ///
    /// The synthetic lockfile may still need updating, for example where
    /// the worktree enabled features the synthetic workspace does not, so
    /// Cargo runs without `--locked`; the exact version pins keep every
    /// package at its locked version, and only the synthetic lockfile
    /// changes.
    ///
    /// # Errors
    /// Returns an error when the workspace cannot be written, or Cargo
    /// cannot start, fails, or times out.
    async fn fetch(
        &self,
        cargo: &str,
        lockfile: &Lockfile,
        environment: &BTreeMap<String, String>,
        host_config: Option<&Path>,
    ) -> io::Result<()> {
        let work = self.root.join(PRIVATE_DIRECTORY).join(WORK_DIRECTORY);
        tokio::fs::write(work.join("Cargo.toml"), lockfile.synthetic_manifest()).await?;
        tokio::fs::write(work.join("Cargo.lock"), lockfile.synthetic_lockfile()).await?;
        tokio::fs::write(work.join("lib.rs"), "").await?;
        let link = self.root.join(HOME_DIRECTORY).join(CONFIG_FILE);
        match tokio::fs::remove_file(&link).await {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        if let Some(config) = host_config {
            tokio::fs::symlink(config, &link).await?;
        }
        Self::run_cargo(cargo, "fetch", &work, environment)
            .await
            .map(drop)
    }

    /// Runs `cargo` with the one argument `command` in `directory` with
    /// only `environment`, no input, and its errors discarded, within the
    /// cargo deadline, and returns its standard output.
    ///
    /// # Errors
    /// Returns an error when Cargo cannot start, fails, or times out.
    async fn run_cargo(
        cargo: &str,
        command: &str,
        directory: &Path,
        environment: &BTreeMap<String, String>,
    ) -> io::Result<Vec<u8>> {
        let output = tokio::process::Command::new(cargo)
            .arg(command)
            .current_dir(directory)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(CARGO_TIMEOUT, output)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "cargo {command} failed with {}",
                output.status
            )));
        }

        Ok(output.stdout)
    }

    /// Copies `source`, an archive of the cache, to `name` in the session
    /// `directory` unless an entry of that name exists, keeping it only when
    /// its SHA-256 is `checksum`.
    ///
    /// # Errors
    /// Returns the filesystem error, or a mismatch, that keeps it out.
    fn copy_archive(
        source: &Path,
        directory: &OwnedFd,
        name: &str,
        checksum: &str,
    ) -> io::Result<()> {
        if fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
            return Ok(());
        }
        let partial = format!(".{name}{PARTIAL_SUFFIX}");
        let _ = fs::unlinkat(directory, partial.as_str(), AtFlags::empty());
        let mut source = std::fs::File::open(source)?;
        let mut destination = std::fs::File::from(fs::openat(
            directory,
            partial.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        )?);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            destination.write_all(&buffer[..read])?;
        }
        drop(destination);
        let verified = hex::encode(hasher.finalize()) == checksum;
        let linked = if verified {
            fs::linkat(
                directory,
                partial.as_str(),
                directory,
                name,
                AtFlags::empty(),
            )
            .map_err(io::Error::from)
        } else {
            Err(io::Error::from(io::ErrorKind::InvalidData))
        };
        let _ = fs::unlinkat(directory, partial.as_str(), AtFlags::empty());

        linked
    }

    /// Returns what the session behind `cargo` last copied.
    fn session_generation(cargo: &OwnedFd) -> Option<String> {
        let descriptor = fs::openat(
            cargo,
            SESSION_GENERATION_FILE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        let mut contents = String::new();
        std::fs::File::from(descriptor)
            .take(256)
            .read_to_string(&mut contents)
            .ok()?;

        Some(contents)
    }

    /// Records that the session behind `cargo` copied `marker`.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being recorded.
    fn record_session_generation(cargo: &OwnedFd, marker: &str) -> io::Result<()> {
        // Commands may have planted a FIFO here: never block on opening it,
        // and write only a regular file.
        let descriptor = fs::openat(
            cargo,
            SESSION_GENERATION_FILE,
            OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        if !FileType::from_raw_mode(fs::fstat(&descriptor)?.st_mode).is_file() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        fs::ftruncate(&descriptor, 0)?;

        std::fs::File::from(descriptor).write_all(marker.as_bytes())
    }

    /// Copies `source`, a file of the cache, to `name` in `directory` unless
    /// an entry of that name exists.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being copied.
    fn copy_absent(source: &Path, directory: &OwnedFd, name: &str) -> io::Result<()> {
        let mut source = std::fs::File::open(source)?;
        let descriptor = fs::openat(
            directory,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        )?;
        io::copy(&mut source, &mut std::fs::File::from(descriptor))?;

        Ok(())
    }
}

/// The record of one finished fetch.
#[derive(Debug, Eq, PartialEq)]
struct Generation {
    /// Files of the cache `CARGO_HOME` sessions copy, relative to it.
    files: Vec<String>,
    /// Archive names the fetch was asked for but did not store.
    missing: Vec<String>,
    /// Number of finished fetches so far.
    number: u64,
    /// Cargo version and host configuration the fetch ran with.
    variant: String,
}

impl Generation {
    /// Reads the generation of the cache at `root`, or `None` before its
    /// first finished fetch.
    fn read(root: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(root.join(GENERATION_FILE)).ok()?)
    }

    /// Reads the generation of the cache at `root` without blocking.
    async fn load(root: &Path) -> Option<Self> {
        Self::parse(
            &tokio::fs::read_to_string(root.join(GENERATION_FILE))
                .await
                .ok()?,
        )
    }

    /// Parses the contents of a generation file.
    fn parse(contents: &str) -> Option<Self> {
        let mut lines = contents.lines();
        if lines.next()? != GENERATION_HEADER {
            return None;
        }
        let number = lines.next()?.parse().ok()?;
        let variant = lines.next()?.to_string();
        let mut files = Vec::new();
        let mut missing = Vec::new();
        for line in lines {
            if let Some(file) = line.strip_prefix("file ")
                && Self::is_relative_file(file)
            {
                files.push(file.to_string());
            } else if let Some(archive) = line.strip_prefix("missing ") {
                missing.push(archive.to_string());
            }
        }

        Some(Self {
            files,
            missing,
            number,
            variant,
        })
    }

    /// Lists the files of `home`, the cache `CARGO_HOME`, that sessions
    /// may copy for the `requested` packages: their crate archives, their
    /// index entries, and each index's configuration, as regular files with
    /// plain relative paths.
    fn scan(home: &Path, requested: &[LockedPackage]) -> Vec<String> {
        let mut files = Vec::new();
        for registry in Self::directories(&home.join(REGISTRY_CACHE)) {
            files.extend(
                requested
                    .iter()
                    .map(|package| registry.join(package.archive_name())),
            );
        }
        for index in Self::directories(&home.join(REGISTRY_INDEX)) {
            files.push(index.join(INDEX_CONFIG));
            files.extend(
                requested
                    .iter()
                    .map(|package| index.join(package.index_entry())),
            );
        }
        files.retain(|file| {
            std::fs::symlink_metadata(file).is_ok_and(|metadata| metadata.is_file())
        });

        let mut relative = files
            .iter()
            .filter_map(|file| file.strip_prefix(home).ok()?.to_str())
            .filter(|file| Self::is_relative_file(file))
            .map(str::to_string)
            .collect::<Vec<_>>();
        relative.sort();

        relative
    }

    /// Returns the subdirectories of `directory`, not following links.
    fn directories(directory: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
            .map(|entry| entry.path())
            .collect()
    }

    /// Returns whether `file` is a plain relative path of named components,
    /// as the generation file can hold it.
    fn is_relative_file(file: &str) -> bool {
        !file.is_empty()
            && !file.contains('\n')
            && file
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
    }

    /// Writes this generation for the cache at `root` through a temporary
    /// file of its own, replacing the old one at once, so concurrent
    /// writers never interleave.
    ///
    /// # Errors
    /// Returns the filesystem error that keeps it from being written.
    async fn write(&self, root: &Path) -> io::Result<()> {
        let mut contents = format!("{GENERATION_HEADER}\n{}\n{}\n", self.number, self.variant);
        for file in &self.files {
            let _ = writeln!(contents, "file {file}");
        }
        for archive in &self.missing {
            let _ = writeln!(contents, "missing {archive}");
        }
        let root = root.to_path_buf();

        tokio::task::spawn_blocking(move || {
            let mut pending = tempfile::NamedTempFile::new_in(&root)?;
            pending.write_all(contents.as_bytes())?;
            pending
                .persist(root.join(GENERATION_FILE))
                .map(drop)
                .map_err(io::Error::from)
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// Directory handles inside a session's `CARGO_HOME`, opened one component
/// at a time without following links.
struct SessionDirectories<'a> {
    cargo: &'a OwnedFd,
    opened: HashMap<String, OwnedFd>,
}

impl<'a> SessionDirectories<'a> {
    /// Starts at `cargo`, the open session `CARGO_HOME`.
    fn new(cargo: &'a OwnedFd) -> Self {
        Self {
            cargo,
            opened: HashMap::new(),
        }
    }

    /// Returns the handle of `relative`, creating each missing component,
    /// or `None` when a component is not a real directory.
    fn open(&mut self, relative: &str) -> Option<&OwnedFd> {
        let mut prefix_end = 0_usize;
        for component in relative.split('/') {
            let parent_prefix = &relative[..prefix_end.saturating_sub(1)];
            prefix_end += component.len() + 1;
            let prefix = &relative[..prefix_end - 1];
            if self.opened.contains_key(prefix) {
                continue;
            }
            let parent = if parent_prefix.is_empty() {
                self.cargo
            } else {
                self.opened.get(parent_prefix)?
            };
            // An existing directory is fine; anything else fails the open.
            let _ = fs::mkdirat(parent, component, Mode::RWXU);
            let opened = fs::openat(
                parent,
                component,
                CommandSandbox::directory_flags(),
                Mode::empty(),
            )
            .ok()?;
            self.opened.insert(prefix.to_string(), opened);
        }

        self.opened.get(relative)
    }
}

#[cfg(test)]
#[path = "cargo_cache_test.rs"]
mod tests;

//! Per-project Cargo download cache that sandboxed session commands reuse
//! without ever reaching its files through their own paths.
//!
//! Agentty fills the cache outside the sandbox with a fetch that runs no
//! model output and reads no manifest, configuration, or credentials: it
//! parses the worktree's `Cargo.lock` as data, keeps only its crates.io
//! packages, and runs `cargo fetch` for a synthetic workspace that pins
//! exactly those, in a fresh directory, with a configuration-free
//! `CARGO_HOME` and an allowlisted environment. Session commands cannot
//! read the cache at all. The fetch records the canonical crates.io
//! archives it was asked for and stored only after it ends. Preparing
//! a session's sandbox then copies the recorded archives its own lockfile
//! names into the session's `CARGO_HOME`, each verified against the
//! lockfile's checksum, so a session that edits its copies, or runs without
//! the sandbox, changes nothing another session uses.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use sha2::{Digest as _, Sha256};

use super::cargo_lockfile::{LockedPackage, Lockfile};
use super::sandbox::CommandSandbox;

/// Hexadecimal characters of the project key: the start of the SHA-256 of
/// the project's main checkout path.
const PROJECT_KEY_LENGTH: usize = 32;
/// Hexadecimal characters of the toolchain variant.
const VARIANT_LENGTH: usize = 16;
/// Cache subdirectory that the fetch uses as `CARGO_HOME`, which sessions
/// copy from.
const HOME_DIRECTORY: &str = "home";
/// Prefix of the temporary directory holding the synthetic workspace,
/// where the fetch runs.
const WORK_PREFIX: &str = "agentty-cargo-fetch-";
/// Subdirectory of the work directory that the fetch uses as
/// `CARGO_TARGET_DIR`, so that Cargo writes nothing elsewhere.
const TARGET_DIRECTORY: &str = "target";
/// Cache file naming the last finished fetch.
const GENERATION_FILE: &str = "generation";
/// First line of a generation file.
const GENERATION_HEADER: &str = "agentty-cargo-cache 3";
/// File of a session's `CARGO_HOME` naming the generation and lockfile it
/// last copied.
const SESSION_GENERATION_FILE: &str = ".agentty-cache-generation";
/// Suffix of a session copy until its checksum is verified.
const PARTIAL_SUFFIX: &str = ".agentty-partial";
/// Files of a `CARGO_HOME` that Cargo reads as configuration or
/// credentials, which the cache `CARGO_HOME` must not hold.
const CARGO_HOME_CONFIGURATION: [&str; 4] =
    ["config.toml", "config", "credentials.toml", "credentials"];
/// Files above a working directory that Cargo reads as configuration, and
/// that `rustup` reads to pick a toolchain.
const ANCESTOR_CONFIGURATION: [&str; 4] = [
    ".cargo/config.toml",
    ".cargo/config",
    "rust-toolchain.toml",
    "rust-toolchain",
];
/// Deadline for one Cargo run: the version check or the fetch.
const CARGO_TIMEOUT: Duration = Duration::from_secs(300);
/// Registry directories of a `CARGO_HOME` holding crate archives.
const REGISTRY_CACHE: &str = "registry/cache";
/// Registry index directories of a `CARGO_HOME`.
const REGISTRY_INDEX: &str = "registry/index";
/// Index file that names a sparse registry's download location.
const INDEX_CONFIG: &str = "config.json";
/// Largest index configuration read.
const MAX_INDEX_CONFIG_BYTES: u64 = 64 * 1024;
/// Prefix of the registry directories Cargo names after crates.io.
const CRATES_IO_DIRECTORY_PREFIX: &str = "index.crates.io-";
/// Download location crates.io's index configuration names.
const CRATES_IO_DOWNLOADS: &str = "https://static.crates.io/crates";
/// Host variables the fetch receives, all others withheld: the executable
/// search path, the home and toolchain `rustup` needs, and the proxies and
/// certificate authorities the network may require.
const ALLOWED_VARIABLES: [&str; 14] = [
    "CARGO_HTTP_CAINFO",
    "CARGO_HTTP_PROXY",
    "HOME",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "PATH",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "http_proxy",
    "https_proxy",
    "no_proxy",
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
    /// name, for a synthetic workspace that pins exactly those packages, so
    /// that no path, Git, or other registry source is reached. It reads no
    /// Cargo configuration and no credentials at all: it runs in a fresh
    /// directory under the first of `environment`'s `XDG_RUNTIME_DIR` and
    /// `TMPDIR` whose every ancestor only root or the current user can
    /// change, so that no other user can plant configuration above it (a
    /// shared `/tmp` does not qualify); it is skipped without such a
    /// directory, or when one of
    /// its ancestors holds Cargo configuration or a toolchain file, with
    /// the cache as a `CARGO_HOME` that holds neither, and with only the
    /// allowed host variables: `PATH`, `HOME`, the `rustup` home and
    /// toolchain, and proxies and certificate authorities. Only archives
    /// from the canonical crates.io registry are recorded, so a host whose
    /// network needs a crates.io mirror gets no shared cache, and its
    /// sessions keep downloading per session. A new Cargo version fetches
    /// again.
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
        tokio::fs::create_dir_all(&home).await?;
        for name in CARGO_HOME_CONFIGURATION {
            match tokio::fs::remove_file(home.join(name)).await {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        let candidates = ["XDG_RUNTIME_DIR", "TMPDIR"]
            .iter()
            .filter_map(|name| environment.get(*name).map(PathBuf::from))
            .collect::<Vec<_>>();
        let work = tokio::task::spawn_blocking(move || {
            let Some(parent) = candidates
                .into_iter()
                .find(|parent| Self::is_private(parent))
            else {
                return io::Result::Ok(None);
            };
            let work = tempfile::Builder::new()
                .prefix(WORK_PREFIX)
                .tempdir_in(parent)?;

            Ok((!Self::reads_configuration(work.path())).then_some(work))
        })
        .await
        .map_err(io::Error::other)??;
        let Some(work) = work else {
            tracing::debug!("No private directory free of Cargo configuration; not caching");

            return Ok(());
        };
        let fetch_environment = self.fetch_environment(environment, work.path());
        let variant = Self::variant(cargo, work.path(), &fetch_environment).await?;
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
        Self::fetch(cargo, &lockfile, work.path(), &fetch_environment).await?;
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

    /// Returns the host `environment` the fetch runs with: only the allowed
    /// variables, plus the cache as `CARGO_HOME` and a target directory in
    /// `work`.
    fn fetch_environment(
        &self,
        environment: &BTreeMap<String, String>,
        work: &Path,
    ) -> BTreeMap<String, String> {
        let mut fetch_environment = environment
            .iter()
            .filter(|(name, _)| ALLOWED_VARIABLES.contains(&name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        fetch_environment.insert(
            "CARGO_HOME".to_string(),
            self.root.join(HOME_DIRECTORY).display().to_string(),
        );
        fetch_environment.insert(
            "CARGO_TARGET_DIR".to_string(),
            work.join(TARGET_DIRECTORY).display().to_string(),
        );

        fetch_environment
    }

    /// Returns whether `directory` resolves to a path whose every ancestor
    /// is owned by root or the current user and writable by neither its
    /// group nor others, so that no other user can create entries above
    /// it.
    fn is_private(directory: &Path) -> bool {
        let Ok(directory) = std::fs::canonicalize(directory) else {
            return false;
        };
        let user = rustix::process::geteuid().as_raw();

        directory.ancestors().all(|ancestor| {
            std::fs::metadata(ancestor).is_ok_and(|metadata| {
                (metadata.uid() == 0 || metadata.uid() == user) && metadata.mode() & 0o022 == 0
            })
        })
    }

    /// Returns whether `work` or one of its ancestors holds Cargo
    /// configuration or a toolchain file, which Cargo or `rustup` would
    /// read when run in `work`.
    fn reads_configuration(work: &Path) -> bool {
        work.ancestors().any(|directory| {
            ANCESTOR_CONFIGURATION
                .iter()
                .any(|name| std::fs::symlink_metadata(directory.join(name)).is_ok())
        })
    }

    /// Returns the variant that a fetch with `cargo`'s version produces:
    /// Cargo has named its registry directories differently across
    /// versions.
    ///
    /// # Errors
    /// Returns an error when `cargo -V` cannot run, fails, or times out.
    async fn variant(
        cargo: &str,
        directory: &Path,
        environment: &BTreeMap<String, String>,
    ) -> io::Result<String> {
        let version = Self::run_cargo(cargo, "-V", directory, environment).await?;

        Ok(hex::encode(Sha256::digest(&version))[..VARIANT_LENGTH].to_string())
    }

    /// Returns the archive names of `packages` that `home` holds in no
    /// crates.io registry directory, except those listed as `missing`.
    async fn uncached(home: &Path, packages: &[LockedPackage], missing: &[String]) -> Vec<String> {
        let home = home.to_path_buf();
        let wanted = packages
            .iter()
            .map(LockedPackage::archive_name)
            .filter(|archive| !missing.contains(archive))
            .collect::<Vec<_>>();

        tokio::task::spawn_blocking(move || {
            let registries = Generation::crates_io_directories(&home);
            wanted
                .into_iter()
                .filter(|archive| {
                    !registries.iter().any(|registry| {
                        std::fs::symlink_metadata(
                            home.join(REGISTRY_CACHE).join(registry).join(archive),
                        )
                        .is_ok_and(|metadata| metadata.is_file())
                    })
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Writes the synthetic workspace for `lockfile` into `work` and runs
    /// `cargo fetch` there.
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
        cargo: &str,
        lockfile: &Lockfile,
        work: &Path,
        environment: &BTreeMap<String, String>,
    ) -> io::Result<()> {
        tokio::fs::write(work.join("Cargo.toml"), lockfile.synthetic_manifest()).await?;
        tokio::fs::write(work.join("Cargo.lock"), lockfile.synthetic_lockfile()).await?;
        tokio::fs::write(work.join("lib.rs"), "").await?;

        Self::run_cargo(cargo, "fetch", work, environment)
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
        for registry in Self::crates_io_directories(home) {
            let archives = home.join(REGISTRY_CACHE).join(&registry);
            let index = home.join(REGISTRY_INDEX).join(&registry);
            files.push(index.join(INDEX_CONFIG));
            for package in requested {
                files.push(archives.join(package.archive_name()));
                files.push(index.join(package.index_entry()));
            }
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

    /// Returns the names of the registry directories of `home` that hold
    /// canonical crates.io: named as Cargo names crates.io's, with an index
    /// configuration that downloads from crates.io and requires no
    /// authentication. Every other registry is ignored.
    fn crates_io_directories(home: &Path) -> Vec<String> {
        std::fs::read_dir(home.join(REGISTRY_INDEX))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                name.starts_with(CRATES_IO_DIRECTORY_PREFIX) && Self::is_relative_file(name)
            })
            .filter(|name| {
                let config = home.join(REGISTRY_INDEX).join(name).join(INDEX_CONFIG);
                let mut contents = Vec::new();
                let read = std::fs::File::open(config)
                    .and_then(|file| file.take(MAX_INDEX_CONFIG_BYTES).read_to_end(&mut contents));

                read.is_ok()
                    && serde_json::from_slice::<serde_json::Value>(&contents).is_ok_and(|config| {
                        config.get("dl").and_then(serde_json::Value::as_str)
                            == Some(CRATES_IO_DOWNLOADS)
                            && config
                                .get("auth-required")
                                .is_none_or(|required| required == &serde_json::Value::Bool(false))
                    })
            })
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

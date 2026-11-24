//! The crates.io packages that a session worktree's `Cargo.lock` pins, read
//! as untrusted data.
//!
//! Commands write the worktree, so its `Cargo.lock` is read once, without
//! following a link or blocking on a planted FIFO, within a size limit, and
//! only crates.io packages with a well-formed name, version, and checksum
//! are kept. Path, Git, and other registries' packages are dropped, so the
//! trusted fetch never reaches a source a session named.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;

use rustix::fs::{self, FileType, Mode, OFlags};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// Lockfile whose crates.io packages the cache holds.
const LOCKFILE: &str = "Cargo.lock";
/// Largest `Cargo.lock` read; a larger file counts as absent.
const MAX_LOCKFILE_BYTES: u64 = 16 * 1024 * 1024;
/// Most packages kept from one lockfile; a larger one counts as absent.
const MAX_PACKAGES: usize = 20_000;
/// Longest package name crates.io accepts.
const MAX_NAME_LENGTH: usize = 64;
/// Longest version kept.
const MAX_VERSION_LENGTH: usize = 64;
/// Lockfile sources that name crates.io.
const CRATES_IO_SOURCES: [&str; 2] = [CRATES_IO_SOURCE, "sparse+https://index.crates.io/"];
/// The source Cargo records for crates.io packages.
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// Hexadecimal length of a SHA-256 checksum.
const CHECKSUM_LENGTH: usize = 64;

/// The crates.io packages of one worktree's `Cargo.lock`.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Lockfile {
    /// SHA-256 of the bytes the packages were parsed from.
    pub(super) digest: String,
    /// Distinct crates.io packages, sorted.
    pub(super) packages: Vec<LockedPackage>,
}

impl Lockfile {
    /// Reads `worktree`'s `Cargo.lock`, or returns `None` when it is
    /// missing, a link, not a regular file, too large, or not a lockfile.
    pub(super) fn read(worktree: &Path) -> Option<Self> {
        let descriptor = fs::open(
            worktree.join(LOCKFILE),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        let stat = fs::fstat(&descriptor).ok()?;
        let size = u64::try_from(stat.st_size).ok()?;
        if !FileType::from_raw_mode(stat.st_mode).is_file() || size > MAX_LOCKFILE_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::from(descriptor)
            .take(MAX_LOCKFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;

        Self::parse(&bytes)
    }

    /// Parses lockfile `bytes`, keeping its valid crates.io packages.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let contents: LockfileContents = toml::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
        if contents.package.len() > MAX_PACKAGES {
            return None;
        }
        let packages = contents
            .package
            .into_iter()
            .filter_map(LockedPackage::from_entry)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Some(Self {
            digest: hex::encode(Sha256::digest(bytes)),
            packages,
        })
    }

    /// Returns the manifest of a workspace that depends on exactly these
    /// packages, without default features, each under its own key so that
    /// several versions of one crate fit.
    pub(super) fn synthetic_manifest(&self) -> String {
        let mut manifest = String::from(
            "[package]\nname = \"agentty-cargo-cache\"\nversion = \"0.0.0\"\nedition = \
             \"2021\"\npublish = false\n\n[lib]\npath = \"lib.rs\"\n\n[dependencies]\n",
        );
        for (index, package) in self.packages.iter().enumerate() {
            let _ = writeln!(
                manifest,
                "p{index} = {{ package = \"{}\", version = \"={}\", default-features = false }}",
                package.name, package.version
            );
        }

        manifest
    }

    /// Returns a lockfile holding exactly these packages, so that Cargo
    /// keeps their versions, yanked ones included, and checks their
    /// checksums.
    pub(super) fn synthetic_lockfile(&self) -> String {
        let mut lockfile = String::from("version = 4\n");
        for package in &self.packages {
            let _ = write!(
                lockfile,
                "\n[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \
                 \"{CRATES_IO_SOURCE}\"\nchecksum = \"{}\"\n",
                package.name, package.version, package.checksum
            );
        }

        lockfile
    }
}

/// One crates.io package a lockfile pins.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct LockedPackage {
    /// SHA-256 of the package's crate archive, in lowercase hexadecimal.
    pub(super) checksum: String,
    pub(super) name: String,
    pub(super) version: String,
}

impl LockedPackage {
    /// Returns the file name Cargo stores the package's crate archive under.
    pub(super) fn archive_name(&self) -> String {
        format!("{}-{}.crate", self.name, self.version)
    }

    /// Returns where a registry index directory caches the package's index
    /// entry, as Cargo lays it out: by the lowercase name's length and
    /// first characters.
    pub(super) fn index_entry(&self) -> String {
        let name = self.name.to_ascii_lowercase();
        let prefix = match name.len() {
            1 => "1".to_string(),
            2 => "2".to_string(),
            3 => format!("3/{}", &name[..1]),
            _ => format!("{}/{}", &name[..2], &name[2..4]),
        };

        format!(".cache/{prefix}/{name}")
    }

    /// Keeps `entry` when it is a crates.io package with a valid name,
    /// version, and checksum.
    fn from_entry(entry: LockfileEntry) -> Option<Self> {
        let source = entry.source?;
        let checksum = entry.checksum?;
        let is_valid = CRATES_IO_SOURCES.contains(&source.as_str())
            && Self::is_valid_name(&entry.name)
            && Self::is_valid_version(&entry.version)
            && checksum.len() == CHECKSUM_LENGTH
            && checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));

        is_valid.then_some(Self {
            checksum,
            name: entry.name,
            version: entry.version,
        })
    }

    /// Returns whether `name` is a crate name crates.io could serve.
    fn is_valid_name(name: &str) -> bool {
        name.len() <= MAX_NAME_LENGTH
            && name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }

    /// Returns whether `version` is a semantic version in canonical form.
    fn is_valid_version(version: &str) -> bool {
        version.len() <= MAX_VERSION_LENGTH
            && semver::Version::parse(version).is_ok_and(|parsed| parsed.to_string() == version)
    }
}

/// The parts of a `Cargo.lock` the cache reads.
#[derive(Deserialize)]
struct LockfileContents {
    #[serde(default)]
    package: Vec<LockfileEntry>,
}

/// One `[[package]]` entry of a `Cargo.lock`.
#[derive(Deserialize)]
struct LockfileEntry {
    checksum: Option<String>,
    name: String,
    source: Option<String>,
    version: String,
}

#[cfg(test)]
#[path = "cargo_lockfile_test.rs"]
mod tests;

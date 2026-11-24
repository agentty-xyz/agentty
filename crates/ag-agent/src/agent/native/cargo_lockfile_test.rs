//! Untrusted `Cargo.lock` reading tests.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

use super::{LockedPackage, Lockfile};

/// A checksum of the right form.
const CHECKSUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Returns a locked crates.io package.
fn package(name: &str, version: &str) -> LockedPackage {
    LockedPackage {
        checksum: CHECKSUM.to_string(),
        name: name.to_string(),
        version: version.to_string(),
    }
}

#[test]
/// Keeps only well-formed crates.io packages, once each, and records the
/// digest of the parsed bytes.
fn parse_keeps_only_valid_crates_io_packages() {
    // Arrange
    let contents = format!(
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["leaf"]

[[package]]
name = "leaf"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{CHECKSUM}"

[[package]]
name = "leaf"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{CHECKSUM}"

[[package]]
name = "Sparse_Leaf"
version = "2.0.0-rc.1"
source = "sparse+https://index.crates.io/"
checksum = "{CHECKSUM}"

[[package]]
name = "gitdep"
version = "1.0.0"
source = "git+ssh://git@example.com/private/gitdep#0123"

[[package]]
name = "private"
version = "1.0.0"
source = "registry+https://registry.example.com/index"
checksum = "{CHECKSUM}"

[[package]]
name = "../escape"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{CHECKSUM}"

[[package]]
name = "short"
version = "1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{CHECKSUM}"

[[package]]
name = "upper"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{}"

[[package]]
name = "unsummed"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
        CHECKSUM.to_uppercase()
    );

    // Act
    let lockfile = Lockfile::parse(contents.as_bytes()).expect("lockfile");

    // Assert
    assert_eq!(
        lockfile.packages,
        [
            package("Sparse_Leaf", "2.0.0-rc.1"),
            package("leaf", "1.0.0")
        ]
    );
    assert_eq!(
        lockfile.digest,
        hex::encode(Sha256::digest(contents.as_bytes()))
    );
}

#[test]
/// Rejects what is not a lockfile, or one listing too many packages.
fn parse_rejects_malformed_and_oversized_lockfiles() {
    // Arrange
    let mut oversized = String::new();
    for index in 0..=20_000 {
        let _ = write!(
            oversized,
            "[[package]]\nname = \"p{index}\"\nversion = \"1.0.0\"\n"
        );
    }

    // Act
    let invalid_utf8 = Lockfile::parse(&[0xff, 0xfe]);
    let invalid_toml = Lockfile::parse(b"[[package]\n");
    let too_many = Lockfile::parse(oversized.as_bytes());
    let empty = Lockfile::parse(b"version = 4\n");

    // Assert
    assert!(invalid_utf8.is_none() && invalid_toml.is_none() && too_many.is_none());
    assert_eq!(empty.expect("empty lockfile").packages, []);
}

#[test]
/// Reads only a regular `Cargo.lock` within the size limit, never through
/// a link or from a FIFO, which would block.
fn read_accepts_only_a_regular_file_within_the_limit() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let lockfile = root.path().join("Cargo.lock");
    let mut outcomes = Vec::new();

    // Act
    outcomes.push(Lockfile::read(root.path()));
    std::os::unix::fs::symlink("/etc/hosts", &lockfile).expect("link");
    outcomes.push(Lockfile::read(root.path()));
    std::fs::remove_file(&lockfile).expect("remove link");
    let fifo = std::process::Command::new("mkfifo")
        .arg(&lockfile)
        .status()
        .expect("mkfifo");
    assert!(fifo.success());
    outcomes.push(Lockfile::read(root.path()));
    std::fs::remove_file(&lockfile).expect("remove FIFO");
    std::fs::File::create(&lockfile)
        .and_then(|file| file.set_len(17 * 1024 * 1024))
        .expect("large lockfile");
    outcomes.push(Lockfile::read(root.path()));
    std::fs::write(&lockfile, "version = 4\n").expect("lockfile");
    let regular = Lockfile::read(root.path());

    // Assert
    assert_eq!(outcomes, [None, None, None, None]);
    assert!(regular.is_some());
}

#[test]
/// Names archives and index entries as Cargo lays them out.
fn packages_name_their_archive_and_index_entry() {
    // Arrange
    let names = ["a", "Ab", "abc", "Serde_Json"];

    // Act
    let entries = names.map(|name| package(name, "1.0.0").index_entry());

    // Assert
    assert_eq!(
        entries,
        [
            ".cache/1/a",
            ".cache/2/ab",
            ".cache/3/a/abc",
            ".cache/se/rd/serde_json"
        ]
    );
    assert_eq!(package("leaf", "1.0.0").archive_name(), "leaf-1.0.0.crate");
}

#[test]
/// Pins every package exactly, under its own key, so that two versions of
/// one crate fit, and locks each with its checksum.
fn synthetic_workspace_pins_exactly_the_locked_packages() {
    // Arrange
    let lockfile = Lockfile {
        digest: String::new(),
        packages: vec![package("leaf", "1.0.0"), package("leaf", "2.0.0")],
    };

    // Act
    let manifest = lockfile.synthetic_manifest();
    let synthetic_lockfile = lockfile.synthetic_lockfile();

    // Assert
    assert!(manifest.ends_with(
        "[dependencies]\np0 = { package = \"leaf\", version = \"=1.0.0\", default-features = \
         false }\np1 = { package = \"leaf\", version = \"=2.0.0\", default-features = false }\n"
    ));
    assert!(manifest.starts_with("[package]\nname = \"agentty-cargo-cache\""));
    assert_eq!(
        synthetic_lockfile,
        format!(
            "version = 4\n\n[[package]]\nname = \"leaf\"\nversion = \"1.0.0\"\nsource = \
             \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \
             \"{CHECKSUM}\"\n\n[[package]]\nname = \"leaf\"\nversion = \"2.0.0\"\nsource = \
             \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{CHECKSUM}\"\n"
        )
    );
}

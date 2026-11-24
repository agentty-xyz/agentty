//! Trusted Cargo cache refresh and session copy tests.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::{CommandSandbox, TrustedCargoCache};

/// Registry directory the fake Cargo fills, as Cargo names one.
const REGISTRY: &str = "index.crates.io-0123456789abcdef";
/// Archive of `leaf` relative to a `CARGO_HOME`.
const LEAF_ARCHIVE: &str = "registry/cache/index.crates.io-0123456789abcdef/leaf-1.0.0.crate";
/// Archive of `twig` relative to a `CARGO_HOME`.
const TWIG_ARCHIVE: &str = "registry/cache/index.crates.io-0123456789abcdef/twig-2.0.0.crate";
/// Index configuration relative to a `CARGO_HOME`.
const INDEX_CONFIG: &str = "registry/index/index.crates.io-0123456789abcdef/config.json";
/// Index entry of `leaf` relative to a `CARGO_HOME`.
const LEAF_ENTRY: &str = "registry/index/index.crates.io-0123456789abcdef/.cache/le/af/leaf";
/// Index entry of `twig` relative to a `CARGO_HOME`.
const TWIG_ENTRY: &str = "registry/index/index.crates.io-0123456789abcdef/.cache/tw/ig/twig";
/// Lockfile source of crates.io packages.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// Index configuration of crates.io.
const CRATES_IO_CONFIG: &str =
    r#"{"dl":"https://static.crates.io/crates","api":"https://crates.io"}"#;
/// Registry directory of a mirror the fake Cargo also fills.
const MIRROR: &str = "mirror.example.com-0123456789abcdef";
/// Registry directory named like crates.io's whose index requires
/// authentication, which the fake Cargo also fills.
const AUTHENTICATED: &str = "index.crates.io-fedcba9876543210";

/// A worktree whose `Cargo.lock` pins `leaf` and `twig` from crates.io
/// beside a Git and a path package, and a fake `cargo` on the `PATH` that
/// records how it ran and stores the pinned archives like a fetch, in the
/// crates.io registry and in two registries that must be ignored.
struct Fixture {
    bin: PathBuf,
    cache: TrustedCargoCache,
    root: TempDir,
    worktree: PathBuf,
}

impl Fixture {
    /// Creates the fixture. The fetch passes the fake `cargo` no variables
    /// of the test's own, so it reads its controls from files that
    /// [`Self::refresh_in`] writes for `FAKE_*` names: it answers `-V` with
    /// `FAKE_VERSION`, sleeps for `FAKE_SLEEP` seconds or `-V` for
    /// `FAKE_VERSION_SLEEP`, fails with `FAKE_EXIT` or `-V` with
    /// `FAKE_VERSION_EXIT`, and leaves out `twig` under `FAKE_SKIP_TWIG`.
    fn new() -> Self {
        let root = private_directory();
        std::fs::create_dir_all(root.path().join("tmp")).expect("temporary directory");
        let worktree = root.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::write(
            worktree.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nleaf = \
             \"1\"\nsecret = { git = \"ssh://git@example.com/private/secret\" }\nother = { \
             version = \"1\", registry = \"private\" }\n",
        )
        .expect("manifest");
        std::fs::write(
            worktree.join("Cargo.lock"),
            Self::lockfile(&["leaf", "twig"]),
        )
        .expect("lockfile");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(bin.join("control")).expect("bin");
        let cargo = bin.join("cargo");
        let record = root.path().join("record");
        std::fs::write(
            &cargo,
            format!(
                "#!/bin/sh\nC='{control}'\nR='{record}'\nv() {{ cat \"$C/$1\" 2>/dev/null; \
                 }}\nif [ \"$1\" = -V ]; then\n  [ -n \"$(v FAKE_VERSION_SLEEP)\" ] && sleep \
                 \"$(v FAKE_VERSION_SLEEP)\"\n  [ -n \"$(v FAKE_VERSION_EXIT)\" ] && exit \
                 \"$(v FAKE_VERSION_EXIT)\"\n  version=$(v FAKE_VERSION)\n  echo \"cargo \
                 ${{version:-1.99.0}}\"\n  exit 0\nfi\n{{ pwd -P; printf '%s\\n' \"$@\"; }} > \
                 \"$R\"\nenv > \"$R.env\"\nls -A \"$CARGO_HOME\" > \"$R.home\"\ncp Cargo.toml \
                 \"$R.manifest\"\ncp Cargo.lock \"$R.lock\"\necho run >> \"$R.runs\"\n[ -n \
                 \"$(v FAKE_SLEEP)\" ] && sleep \"$(v FAKE_SLEEP)\"\n[ -n \"$(v FAKE_EXIT)\" ] && \
                 exit \"$(v FAKE_EXIT)\"\nfor registry in {REGISTRY} {MIRROR} {AUTHENTICATED}; \
                 do\n  mkdir -p \"$CARGO_HOME/registry/cache/$registry\" \
                 \"$CARGO_HOME/registry/index/$registry/.cache/le/af\" \
                 \"$CARGO_HOME/registry/index/$registry/.cache/tw/ig\"\n  printf leaf > \
                 \"$CARGO_HOME/registry/cache/$registry/leaf-1.0.0.crate\"\n  printf leaf-entry > \
                 \"$CARGO_HOME/registry/index/$registry/.cache/le/af/leaf\"\ndone\nprintf '%s' \
                 '{CRATES_IO_CONFIG}' > \"$CARGO_HOME/{INDEX_CONFIG}\"\nprintf '%s' \
                 '{{\"dl\":\"https://mirror.example.com/crates\"}}' > \
                 \"$CARGO_HOME/registry/index/{MIRROR}/config.json\"\nprintf '%s' \
                 '{{\"dl\":\"https://static.crates.io/crates\",\"auth-required\":true}}' > \
                 \"$CARGO_HOME/registry/index/{AUTHENTICATED}/config.json\"\nif [ -z \"$(v \
                 FAKE_SKIP_TWIG)\" ]; then\n  printf twig > \"$CARGO_HOME/{TWIG_ARCHIVE}\"\n  \
                 printf twig-entry > \"$CARGO_HOME/{TWIG_ENTRY}\"\nfi\nln -sf /etc/hosts \
                 \"$CARGO_HOME/registry/cache/{REGISTRY}/linked.crate\"\nprintf other > \
                 \"$CARGO_HOME/registry/cache/{REGISTRY}/unrequested-1.0.0.crate\"\n",
                control = bin.join("control").display(),
                record = record.display(),
            ),
        )
        .expect("fake cargo");
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let cache = TrustedCargoCache::for_project(&root.path().join("cache"), &worktree);

        Self {
            bin,
            cache,
            root,
            worktree,
        }
    }

    /// Returns a lockfile pinning the crates.io packages `names` at their
    /// fake versions and checksums, beside a Git and a path package.
    fn lockfile(names: &[&str]) -> String {
        let mut lockfile = String::from(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname \
             = \"secret\"\nversion = \"1.0.0\"\nsource = \
             \"git+ssh://git@example.com/private/secret#0123\"\n",
        );
        for name in names {
            let version = if *name == "leaf" { "1.0.0" } else { "2.0.0" };
            let _ = write!(
                lockfile,
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \
                 \"{CRATES_IO}\"\nchecksum = \"{}\"\n",
                digest(name)
            );
        }

        lockfile
    }

    /// Returns the host environment the fake `cargo` runs with, plus the
    /// `extra` variables whose names do not start with `FAKE_`.
    fn environment(&self, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([
            (
                "PATH".to_string(),
                format!("{}:/usr/bin:/bin", self.bin.display()),
            ),
            (
                "HOME".to_string(),
                self.root.path().join("home").display().to_string(),
            ),
            (
                "TMPDIR".to_string(),
                self.root.path().join("tmp").display().to_string(),
            ),
        ]);
        for (name, value) in extra.iter().filter(|(name, _)| !name.starts_with("FAKE_")) {
            environment.insert((*name).to_string(), (*value).to_string());
        }

        environment
    }

    /// Refreshes the cache for `worktree` with `extra` variables, writing
    /// `FAKE_*` ones as the fake `cargo`'s controls, and returns the outcome
    /// and whether a fetch started.
    async fn refresh_in(
        &self,
        worktree: &Path,
        extra: &[(&str, &str)],
    ) -> (std::io::Result<()>, bool) {
        let control = self.bin.join("control");
        std::fs::remove_dir_all(&control).expect("old controls");
        std::fs::create_dir_all(&control).expect("controls");
        for (name, value) in extra.iter().filter(|(name, _)| name.starts_with("FAKE_")) {
            std::fs::write(control.join(name), value).expect("control");
        }
        let fetched = AtomicBool::new(false);
        let outcome = self
            .cache
            .refresh("cargo", worktree, &self.environment(extra), || {
                fetched.store(true, Ordering::SeqCst);
            })
            .await;

        (outcome, fetched.load(Ordering::SeqCst))
    }

    /// Refreshes the cache for the fixture worktree with `extra` variables.
    async fn refresh(&self, extra: &[(&str, &str)]) -> (std::io::Result<()>, bool) {
        self.refresh_in(&self.worktree, extra).await
    }

    /// Returns the file the fake `cargo` records its last fetch in.
    fn record(&self) -> PathBuf {
        self.root.path().join("record")
    }

    /// Returns a file the fake `cargo` wrote beside its record.
    fn recorded(&self, suffix: &str) -> String {
        std::fs::read_to_string(format!("{}{suffix}", self.record().display())).unwrap_or_default()
    }

    /// Returns the variables the fake `cargo`'s last fetch received,
    /// without those its shell sets.
    fn recorded_environment(&self) -> BTreeMap<String, String> {
        self.recorded(".env")
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(name, _)| !["OLDPWD", "PWD", "SHLVL", "_"].contains(name))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    /// Returns how many fetches the fake `cargo` ran.
    fn runs(&self) -> usize {
        self.recorded(".runs").lines().count()
    }

    /// Returns the cache `CARGO_HOME`.
    fn home(&self) -> PathBuf {
        self.cache.root.join("home")
    }

    /// Returns the generation file's contents.
    fn generation(&self) -> Option<String> {
        std::fs::read_to_string(self.cache.root.join("generation")).ok()
    }
}

/// Returns a fresh directory where the fetch may run: under the first of
/// the test process's `TMPDIR`, `XDG_RUNTIME_DIR`, CI runner temporary
/// directory, and home directory that only its owner can change and that
/// has no Cargo configuration above it, since a shared `/tmp` does not
/// qualify.
fn private_directory() -> TempDir {
    let parent = ["TMPDIR", "XDG_RUNTIME_DIR", "RUNNER_TEMP", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|parent| {
            TrustedCargoCache::is_private(parent) && !TrustedCargoCache::reads_configuration(parent)
        })
        .expect("a private temporary directory");

    tempfile::Builder::new()
        .prefix("cargo-cache-test-")
        .tempdir_in(parent)
        .expect("private directory")
}

/// Opens `path` as a session `CARGO_HOME` handle, creating it.
fn session_home(path: &Path) -> OwnedFd {
    std::fs::create_dir_all(path).expect("session home");

    rustix::fs::open(
        path,
        CommandSandbox::directory_flags(),
        rustix::fs::Mode::empty(),
    )
    .expect("session home handle")
}

/// Returns the SHA-256 Agentty records for `contents`.
fn digest(contents: &str) -> String {
    hex::encode(Sha256::digest(contents))
}

#[test]
/// Keys each project's cache by its main checkout under the cache root and
/// hides its private parts.
fn for_project_keys_the_cache_by_main_checkout() {
    // Arrange
    let root = Path::new("/cache-root");

    // Act
    let first = TrustedCargoCache::for_project(root, Path::new("/projects/one"));
    let again = TrustedCargoCache::for_project(root, Path::new("/projects/one"));
    let other = TrustedCargoCache::for_project(root, Path::new("/projects/two"));

    // Assert
    assert_eq!(first, again);
    assert_ne!(first, other);
    let project = root.join(&digest("/projects/one")[..32]);
    assert_eq!(first.root, project);
    assert_eq!(first.hidden_path(), root);
}

#[tokio::test]
/// Fetches only the lockfile's crates.io packages, through a synthetic
/// workspace in a fresh directory under the host's temporary directory,
/// with a `CARGO_HOME` cleared of configuration and credentials and only
/// the allowed host variables: the worktree manifest, its Git dependency,
/// and its other registry never reach Cargo, nor do agent sockets,
/// credentials, Git or Cargo configuration variables. It then records the
/// requested archives it stored in the canonical crates.io registry and
/// their index metadata, but no mirror, authenticated registry, other
/// archive, link, or file, and fetches nothing while every pinned archive
/// is cached.
async fn refresh_fetches_only_crates_io_packages_through_a_synthetic_workspace() {
    // Arrange
    let fixture = Fixture::new();
    let temporary = fixture.root.path().join("tmp");
    std::fs::create_dir_all(&temporary).expect("temporary directory");
    std::fs::create_dir_all(fixture.home()).expect("cache home");
    for name in ["config.toml", "credentials.toml"] {
        std::fs::write(fixture.home().join(name), "[registry]\n").expect("planted file");
    }
    let temporary_value = temporary.display().to_string();
    let host = [
        ("SSH_AUTH_SOCK", "/agent.sock"),
        ("GIT_ASKPASS", "/askpass"),
        ("GIT_CONFIG_GLOBAL", "/gitconfig"),
        ("CARGO_REGISTRIES_PRIVATE_TOKEN", "token"),
        (
            "CARGO_REGISTRIES_PRIVATE_CREDENTIAL_PROVIDER",
            "cargo:token",
        ),
        ("CARGO_NET_GIT_FETCH_WITH_CLI", "true"),
        ("DEPLOY_TOKEN", "token"),
        ("HTTPS_PROXY", "http://proxy.example.com:3128"),
        ("TMPDIR", &temporary_value),
    ];

    // Act
    let first = fixture.refresh(&host).await;
    let again = fixture.refresh(&[]).await;

    // Assert
    assert!(first.0.is_ok() && first.1, "{first:?}");
    assert!(again.0.is_ok() && !again.1, "{again:?}");
    assert_eq!(fixture.runs(), 1);
    let record = fixture.recorded("");
    let mut record = record.lines();
    let work = PathBuf::from(record.next().expect("working directory"));
    assert_eq!(
        work.parent(),
        Some(
            std::fs::canonicalize(&temporary)
                .expect("temporary")
                .as_path()
        )
    );
    assert!(
        work.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("agentty-cargo-fetch-"))
    );
    assert_eq!(record.collect::<Vec<_>>(), ["fetch"]);
    assert!(!work.exists(), "the work directory is removed");
    let manifest = fixture.recorded(".manifest");
    assert!(manifest.contains("package = \"leaf\", version = \"=1.0.0\""));
    assert!(manifest.contains("package = \"twig\", version = \"=2.0.0\""));
    assert!(!manifest.contains("secret") && !manifest.contains("private"));
    assert!(!fixture.recorded(".lock").contains("git+"));
    let home_entries = fixture.recorded(".home");
    assert!(!home_entries.contains("config") && !home_entries.contains("credentials"));
    let environment = fixture.recorded_environment();
    let names = environment.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "HOME",
            "HTTPS_PROXY",
            "PATH"
        ]
    );
    assert_eq!(
        environment.get("CARGO_HOME").map(String::as_str),
        fixture.home().to_str()
    );
    let work_name = work
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    assert!(
        environment
            .get("CARGO_TARGET_DIR")
            .is_some_and(|target| target.ends_with(&format!("{work_name}/target")))
    );
    let generation = fixture.generation().expect("generation");
    let mut lines = generation.lines();
    assert_eq!(lines.next(), Some("agentty-cargo-cache 3"));
    assert_eq!(lines.next(), Some("1"));
    assert_eq!(lines.next().map(str::len), Some(16));
    assert_eq!(
        lines.collect::<Vec<_>>(),
        [
            format!("file {LEAF_ARCHIVE}"),
            format!("file {TWIG_ARCHIVE}"),
            format!("file {LEAF_ENTRY}"),
            format!("file {TWIG_ENTRY}"),
            format!("file {INDEX_CONFIG}"),
        ]
    );
}

#[tokio::test]
/// Covers one project's worktrees cumulatively: a worktree locking a subset
/// fetches nothing, one adding a package fetches again, and a new Cargo
/// version fetches again.
async fn refresh_covers_worktrees_cumulatively_per_toolchain() {
    // Arrange
    let fixture = Fixture::new();
    std::fs::write(
        fixture.worktree.join("Cargo.lock"),
        Fixture::lockfile(&["leaf"]),
    )
    .expect("first lockfile");
    let other = fixture.root.path().join("other-worktree");
    std::fs::create_dir_all(&other).expect("other worktree");
    std::fs::write(
        other.join("Cargo.lock"),
        Fixture::lockfile(&["leaf", "twig"]),
    )
    .expect("other lockfile");
    let subset = fixture.root.path().join("subset-worktree");
    std::fs::create_dir_all(&subset).expect("subset worktree");
    std::fs::write(subset.join("Cargo.lock"), Fixture::lockfile(&["twig"]))
        .expect("subset lockfile");

    // Act
    let first = fixture.refresh(&[("FAKE_SKIP_TWIG", "1")]).await;
    let added = fixture.refresh_in(&other, &[]).await;
    let covered = fixture.refresh_in(&subset, &[]).await;
    let upgraded = fixture
        .refresh_in(&subset, &[("FAKE_VERSION", "2.0.0")])
        .await;

    // Assert
    let fetched = [first, added, covered, upgraded].map(|(outcome, fetched)| {
        assert!(outcome.is_ok());
        fetched
    });
    assert_eq!(fetched, [true, true, false, true]);
}

#[test]
/// Accepts only a directory whose every ancestor its owner alone can
/// change: a group- or world-writable ancestor, as a shared `/tmp` is, or a
/// missing directory fails.
fn private_directories_admit_no_other_writer() {
    // Arrange
    let root = private_directory();
    let world = root.path().join("world");
    let group = root.path().join("group");
    for (directory, mode) in [(&world, 0o1777), (&group, 0o770)] {
        std::fs::create_dir_all(directory.join("child")).expect("directory");
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode))
            .expect("permissions");
    }

    // Act
    let private = TrustedCargoCache::is_private(root.path());
    let below_world = TrustedCargoCache::is_private(&world.join("child"));
    let below_group = TrustedCargoCache::is_private(&group.join("child"));
    let missing = TrustedCargoCache::is_private(&root.path().join("missing"));

    // Assert
    assert!(private);
    assert!(!below_world && !below_group && !missing);
}

#[tokio::test]
/// Fetches nothing, and does not even read the Cargo version, when neither
/// `XDG_RUNTIME_DIR` nor `TMPDIR` names a private directory, and fetches in
/// a private `XDG_RUNTIME_DIR` before `TMPDIR`.
async fn refresh_runs_only_in_a_private_directory() {
    // Arrange
    let fixture = Fixture::new();
    let world = fixture.root.path().join("world");
    std::fs::create_dir_all(&world).expect("shared directory");
    std::fs::set_permissions(&world, std::fs::Permissions::from_mode(0o1777)).expect("permissions");
    let runtime = fixture.root.path().join("runtime");
    std::fs::create_dir_all(&runtime).expect("runtime directory");
    let world_value = world.display().to_string();
    let runtime_value = runtime.display().to_string();

    // Act
    let shared = fixture
        .refresh(&[
            ("XDG_RUNTIME_DIR", &world_value),
            ("TMPDIR", &world_value),
            ("PATH", "/nonexistent"),
        ])
        .await;
    let private = fixture
        .refresh(&[
            ("XDG_RUNTIME_DIR", &runtime_value),
            ("TMPDIR", &world_value),
        ])
        .await;

    // Assert
    assert!(shared.0.is_ok() && !shared.1);
    assert!(private.0.is_ok() && private.1, "{private:?}");
    let work = PathBuf::from(fixture.recorded("").lines().next().unwrap_or_default());
    assert_eq!(
        work.parent(),
        Some(std::fs::canonicalize(&runtime).expect("runtime").as_path())
    );
}

#[tokio::test]
/// Fetches nothing, and does not even read the Cargo version, when an
/// ancestor of the fetch directory holds Cargo configuration or a toolchain
/// file, which Cargo or `rustup` would read.
async fn refresh_skips_configuration_above_the_fetch_directory() {
    // Arrange
    let fixture = Fixture::new();
    let parent = fixture.root.path().join("configured");
    let temporary = parent.join("tmp");
    std::fs::create_dir_all(&temporary).expect("temporary directory");
    let temporary_value = temporary.display().to_string();
    let mut outcomes = Vec::new();

    // Act
    for planted in [".cargo/config.toml", "rust-toolchain.toml"] {
        let file = parent.join(planted);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("planted parent");
        std::fs::write(&file, "").expect("planted file");
        outcomes.push(
            fixture
                .refresh(&[("TMPDIR", &temporary_value), ("PATH", "/nonexistent")])
                .await,
        );
        std::fs::remove_file(&file).expect("remove planted file");
    }
    let clean = fixture.refresh(&[("TMPDIR", &temporary_value)]).await;

    // Assert
    for (outcome, fetched) in outcomes {
        assert!(outcome.is_ok() && !fetched);
    }
    assert!(clean.0.is_ok() && clean.1, "a clean directory fetches");
}

#[tokio::test]
/// Lets a session turn copy from the cache while another process holds
/// Cargo's locks on it: copying takes none of them.
async fn copy_into_ignores_held_cargo_locks() {
    // Arrange
    let fixture = Fixture::new();
    let _ = fixture.refresh(&[]).await;
    let locks = [".package-cache", ".package-cache-mutate", ".global-cache"].map(|name| {
        let lock = std::fs::File::create(fixture.home().join(name)).expect("lock file");
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("held lock");

        lock
    });
    let session = fixture.root.path().join("session-locked");

    // Act
    fixture
        .cache
        .copy_into(&session_home(&session), &fixture.worktree);

    // Assert
    assert!(session.join(LEAF_ARCHIVE).is_file());
    drop(locks);
}

#[tokio::test]
/// Fetches nothing, and does not even read the Cargo version, for a
/// worktree without a usable `Cargo.lock` or without crates.io packages.
async fn refresh_skips_a_worktree_without_crates_io_packages() {
    // Arrange
    let fixture = Fixture::new();
    let without_lockfile = fixture.root.path().join("without-lockfile");
    std::fs::create_dir_all(&without_lockfile).expect("worktree");
    std::fs::write(fixture.worktree.join("Cargo.lock"), Fixture::lockfile(&[]))
        .expect("lockfile without crates.io packages");
    let no_cargo = [("PATH", "/nonexistent")];

    // Act
    let missing = fixture.refresh_in(&without_lockfile, &no_cargo).await;
    let local_only = fixture.refresh(&no_cargo).await;

    // Assert
    assert!(missing.0.is_ok() && !missing.1);
    assert!(local_only.0.is_ok() && !local_only.1);
    assert_eq!(fixture.generation(), None);
}

#[tokio::test]
/// Reports a fetch that left a package uncached, records it, and does not
/// fetch it again for the same toolchain.
async fn refresh_reports_and_remembers_uncached_packages() {
    // Arrange
    let fixture = Fixture::new();

    // Act
    let incomplete = fixture.refresh(&[("FAKE_SKIP_TWIG", "1")]).await;
    let again = fixture.refresh(&[]).await;

    // Assert
    assert!(
        incomplete
            .0
            .expect_err("an incomplete fetch fails")
            .to_string()
            .contains("1 packages uncached")
    );
    assert!(again.0.is_ok() && !again.1);
    assert!(
        fixture
            .generation()
            .expect("generation")
            .contains("missing twig-2.0.0.crate")
    );
}

#[tokio::test]
/// Keeps the previous generation when Cargo fails or cannot start, its
/// version cannot be read, configuration in the cache `CARGO_HOME` cannot
/// be removed, or the cache cannot be written.
async fn refresh_keeps_the_previous_generation_on_failure() {
    // Arrange
    let fixture = Fixture::new();
    std::fs::write(
        fixture.worktree.join("Cargo.lock"),
        Fixture::lockfile(&["leaf"]),
    )
    .expect("first lockfile");
    let _ = fixture.refresh(&[("FAKE_SKIP_TWIG", "1")]).await;
    let before = fixture.generation();
    std::fs::write(
        fixture.worktree.join("Cargo.lock"),
        Fixture::lockfile(&["leaf", "twig"]),
    )
    .expect("changed lockfile");
    let unwritable_root = tempfile::NamedTempFile::new().expect("file in place of a directory");
    let unwritable = TrustedCargoCache::for_project(unwritable_root.path(), &fixture.worktree);

    // Act
    let failed = fixture.refresh(&[("FAKE_EXIT", "3")]).await;
    let missing = fixture.refresh(&[("PATH", "/nonexistent")]).await;
    let versionless = fixture.refresh(&[("FAKE_VERSION_EXIT", "1")]).await;
    std::fs::create_dir_all(fixture.home().join("config.toml/kept")).expect("planted directory");
    let unremovable = fixture.refresh(&[]).await;
    std::fs::remove_dir_all(fixture.home().join("config.toml")).expect("remove directory");
    let unwritable = unwritable
        .refresh("cargo", &fixture.worktree, &fixture.environment(&[]), || {})
        .await;

    // Assert
    assert!(
        failed
            .0
            .expect_err("a failing cargo fails")
            .to_string()
            .contains("cargo fetch failed")
    );
    for outcome in [missing.0, versionless.0, unremovable.0, unwritable] {
        assert!(outcome.is_err());
    }
    assert_eq!(fixture.generation(), before);
}

#[tokio::test(start_paused = true)]
/// Stops a fetch or version check that outlives its deadline.
async fn refresh_times_out_hung_cargo() {
    // Arrange
    let fixture = Fixture::new();

    // Act
    let hung_fetch = fixture.refresh(&[("FAKE_SLEEP", "30")]).await;
    let hung_version = fixture.refresh(&[("FAKE_VERSION_SLEEP", "30")]).await;

    // Assert
    for (outcome, _) in [hung_fetch, hung_version] {
        assert_eq!(
            outcome.expect_err("hung cargo times out").kind(),
            std::io::ErrorKind::TimedOut
        );
    }
    assert_eq!(fixture.generation(), None);
}

#[tokio::test]
/// Copies the recorded archives the session's lockfile names, verified, and
/// their index metadata into its `CARGO_HOME`, keeping the session's own
/// entries, then skips the work while the generation and lockfile stay the
/// same and copies again when the lockfile changes.
async fn copy_into_copies_verified_archives_without_clobbering() {
    // Arrange
    let fixture = Fixture::new();
    let _ = fixture.refresh(&[]).await;
    std::fs::write(
        fixture.worktree.join("Cargo.lock"),
        Fixture::lockfile(&["leaf"]),
    )
    .expect("session lockfile");
    let session = fixture.root.path().join("session-cargo");
    let handle = session_home(&session);
    std::fs::create_dir_all(session.join(INDEX_CONFIG).parent().expect("parent"))
        .expect("session index");
    std::fs::write(session.join(INDEX_CONFIG), "session").expect("session config");

    // Act
    fixture.cache.copy_into(&handle, &fixture.worktree);
    let twig_after_leaf_lock = session.join(TWIG_ARCHIVE).exists();
    std::fs::remove_file(session.join(LEAF_ARCHIVE)).expect("session removes its copy");
    fixture.cache.copy_into(&handle, &fixture.worktree);
    let leaf_after_unchanged = session.join(LEAF_ARCHIVE).exists();
    std::fs::write(
        fixture.worktree.join("Cargo.lock"),
        Fixture::lockfile(&["leaf", "twig"]),
    )
    .expect("changed lockfile");
    std::fs::write(session.join(TWIG_ARCHIVE), "session twig").expect("session archive");
    fixture.cache.copy_into(&handle, &fixture.worktree);

    // Assert
    assert!(!twig_after_leaf_lock && !leaf_after_unchanged);
    let metadata = std::fs::symlink_metadata(session.join(LEAF_ARCHIVE)).expect("copy");
    assert!(metadata.is_file(), "a copy, never a link into the cache");
    assert_eq!(
        std::fs::read_to_string(session.join(LEAF_ARCHIVE)).expect("leaf"),
        "leaf"
    );
    assert_eq!(
        std::fs::read_to_string(session.join(TWIG_ARCHIVE)).expect("twig"),
        "session twig",
        "the session's own archive is kept"
    );
    assert_eq!(
        std::fs::read_to_string(session.join(INDEX_CONFIG)).expect("config"),
        "session"
    );
    assert_eq!(
        std::fs::read_to_string(session.join(LEAF_ENTRY)).expect("entry"),
        "leaf-entry"
    );
    for registry in [MIRROR, AUTHENTICATED] {
        assert!(!session.join("registry/cache").join(registry).exists());
        assert!(!session.join("registry/index").join(registry).exists());
    }
}

#[tokio::test]
/// Keeps what one session does out of every other session and out of the
/// cache: a session that replaces its copy with a poisoned archive changes
/// neither, and a cached archive that no longer matches the lockfile's
/// checksum is never copied.
async fn copies_stay_private_and_verified() {
    // Arrange
    let fixture = Fixture::new();
    let _ = fixture.refresh(&[]).await;
    let poisoned = fixture.root.path().join("session-a");
    let poisoned_handle = session_home(&poisoned);
    fixture.cache.copy_into(&poisoned_handle, &fixture.worktree);
    std::fs::write(poisoned.join(LEAF_ARCHIVE), "poisoned").expect("poisoned archive");
    let clean = fixture.root.path().join("session-b");
    std::fs::write(fixture.home().join(TWIG_ARCHIVE), "tampered").expect("tampered cache");
    let tampered = fixture.root.path().join("session-c");

    // Act
    fixture
        .cache
        .copy_into(&session_home(&clean), &fixture.worktree);
    fixture
        .cache
        .copy_into(&session_home(&tampered), &fixture.worktree);

    // Assert
    assert_eq!(
        std::fs::read_to_string(clean.join(LEAF_ARCHIVE)).expect("genuine archive"),
        "leaf"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home().join(LEAF_ARCHIVE)).expect("cached archive"),
        "leaf"
    );
    assert!(!tampered.join(TWIG_ARCHIVE).exists());
    let leftovers = std::fs::read_dir(tampered.join("registry/cache").join(REGISTRY))
        .expect("session registry")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(leftovers, ["leaf-1.0.0.crate"], "no partial copy is left");
}

#[tokio::test]
/// Never follows a link a session planted in place of a directory or of its
/// generation record, and copies nothing before a fetch finished, without a
/// lockfile, or from a damaged generation file.
async fn copy_into_never_follows_planted_links() {
    // Arrange
    let fixture = Fixture::new();
    let unfetched = fixture.root.path().join("session-unfetched");
    fixture
        .cache
        .copy_into(&session_home(&unfetched), &fixture.worktree);
    let _ = fixture.refresh(&[]).await;
    let outside = fixture.root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    let planted = fixture.root.path().join("session-planted");
    std::fs::create_dir_all(&planted).expect("planted session");
    std::os::unix::fs::symlink(&outside, planted.join("registry")).expect("planted directory");
    std::os::unix::fs::symlink(
        outside.join("record"),
        planted.join(".agentty-cache-generation"),
    )
    .expect("planted record");
    let without_lockfile = fixture.root.path().join("without-lockfile");
    std::fs::create_dir_all(&without_lockfile).expect("worktree");
    let unlocked = fixture.root.path().join("session-unlocked");

    // Act
    fixture
        .cache
        .copy_into(&session_home(&planted), &fixture.worktree);
    fixture
        .cache
        .copy_into(&session_home(&unlocked), &without_lockfile);
    let generation = fixture.cache.root.join("generation");
    let contents = std::fs::read_to_string(&generation).expect("generation");
    std::fs::write(
        &generation,
        contents.replace(
            &format!("file {LEAF_ARCHIVE}"),
            "file ../leaf-1.0.0.crate\nnoise",
        ),
    )
    .expect("damaged generation");
    let damaged = fixture.root.path().join("session-damaged");
    fixture
        .cache
        .copy_into(&session_home(&damaged), &fixture.worktree);
    std::fs::write(&generation, "agentty-cargo-cache 1\n1\n").expect("old generation");
    let outdated = fixture.root.path().join("session-outdated");
    fixture
        .cache
        .copy_into(&session_home(&outdated), &fixture.worktree);

    // Assert
    for session in [&unfetched, &unlocked, &outdated] {
        assert_eq!(std::fs::read_dir(session).expect("session").count(), 0);
    }
    assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
    assert!(!damaged.join(LEAF_ARCHIVE).exists());
    assert!(damaged.join(TWIG_ARCHIVE).is_file());
}

#[tokio::test]
/// Never blocks on, or writes into, a FIFO a session planted in place of
/// its generation record, whether or not a command holds it open.
async fn copy_into_never_blocks_on_a_planted_fifo() {
    // Arrange
    let fixture = Fixture::new();
    let _ = fixture.refresh(&[]).await;
    let session = fixture.root.path().join("session-fifo");
    std::fs::create_dir_all(&session).expect("session");
    let record = session.join(".agentty-cache-generation");
    let fifo = std::process::Command::new("mkfifo")
        .arg(&record)
        .status()
        .expect("mkfifo");
    assert!(fifo.success());
    let handle = session_home(&session);

    // Act
    fixture.cache.copy_into(&handle, &fixture.worktree);
    let reader = rustix::fs::open(
        &record,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .expect("reader");
    fixture.cache.copy_into(&handle, &fixture.worktree);

    // Assert
    let mut unread = [0_u8; 8];
    assert_eq!(rustix::io::read(&reader, &mut unread).unwrap_or(0), 0);
    assert!(session.join(LEAF_ARCHIVE).is_file());
    let file_type = std::fs::symlink_metadata(&record)
        .expect("record")
        .file_type();
    assert!(std::os::unix::fs::FileTypeExt::is_fifo(&file_type));
}

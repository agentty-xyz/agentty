use std::cell::RefCell;
use std::collections::VecDeque;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use crate::validation::{
    Catalog, Changes, Hook, Host, Ledger, Metadata, Options, ProcessHost, Runner, ValidationGuard,
};

struct Guard;
impl ValidationGuard for Guard {}

struct ScriptedHost {
    calls: RefCell<Vec<String>>,
    changed_files: Vec<PathBuf>,
    error: &'static str,
    fingerprints: RefCell<VecDeque<String>>,
    ledger: RefCell<Ledger>,
    seeds: Vec<String>,
    target: PathBuf,
}

impl ScriptedHost {
    fn new() -> Self {
        Self {
            calls: RefCell::default(),
            changed_files: Vec::new(),
            error: "",
            fingerprints: RefCell::default(),
            ledger: RefCell::default(),
            seeds: vec!["core".into()],
            target: "target".into(),
        }
    }

    fn check(&self, operation: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(operation.into());
        if self.error == operation {
            return Err(operation.into());
        }

        Ok(())
    }
}

impl Host for ScriptedHost {
    fn packages(&self) -> Vec<String> {
        self.seeds.clone()
    }

    fn changes(&self, base: &str) -> Result<Changes, String> {
        self.check("changes")?;
        assert_eq!(base, "origin/main");

        Ok(Changes {
            files: self.changed_files.clone(),
            root: "/workspace".into(),
        })
    }

    fn metadata(&self) -> Result<Metadata, String> {
        self.check("metadata")?;
        let mut metadata = metadata();
        metadata.target_directory.clone_from(&self.target);

        Ok(metadata)
    }

    fn catalog(&self) -> Result<Catalog, String> {
        self.check("catalog")?;
        Ok(catalog())
    }

    fn lock(&self, _: &Path) -> Result<Box<dyn ValidationGuard>, String> {
        self.check("lock")?;
        Ok(Box::new(Guard))
    }

    fn load(&self, _: &Path) -> Result<Ledger, String> {
        self.check("load")?;
        Ok(self.ledger.borrow().clone())
    }

    fn fingerprint(&self, packages: &[String], _: &str, _: &Path) -> Result<String, String> {
        self.check("fingerprint")?;
        assert_eq!(packages, ["app", "core"]);
        Ok(self
            .fingerprints
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| "same".into()))
    }

    fn execute(&self, hook: &Hook, _: &[String], target: &Path) -> Result<(), String> {
        assert_eq!(target, self.target);
        self.check(&format!("execute-{}", hook.id))
    }

    fn save(&self, _: &Path, ledger: &Ledger) -> Result<(), String> {
        self.check("save")?;
        self.ledger.replace(ledger.clone());
        Ok(())
    }
}

fn metadata() -> Metadata {
    serde_json::from_value(serde_json::json!({
        "target_directory": "target",
        "workspace_members": ["core", "support", "app", "other", "macros"],
        "packages": [
            {"id":"core", "name":"core", "manifest_path":"/workspace/crates/core/Cargo.toml", "dependencies":[{"name":"support", "path":"support"}]},
            {"id":"support", "name":"support", "manifest_path":"/workspace/crates/support/Cargo.toml", "dependencies":[]},
            {"id":"app", "name":"app", "manifest_path":"/workspace/crates/app/Cargo.toml", "dependencies":[{"name":"core", "path":"core"}, {"name":"external", "path":null}]},
            {"id":"other", "name":"other", "manifest_path":"/workspace/crates/other/Cargo.toml", "dependencies":[]},
            {"id":"macros", "name":"macros", "manifest_path":"/workspace/crates/core/macros/Cargo.toml", "dependencies":[]},
            {"id":"registry", "name":"registry", "manifest_path":"/registry/registry/Cargo.toml", "dependencies":[]}
        ]
    })).expect("metadata fixture")
}

fn catalog() -> Catalog {
    serde_yaml_ng::from_str("default_stages: [pre-commit]\nrepos:\n- hooks:\n  - id: clippy\n  - id: test-focused\n    stages: [manual]\n  - id: coverage\n    stages: [manual]\n  - id: unsupported\n    stages: [commit-msg]\n").expect("catalog fixture")
}

fn options(reuse: bool) -> Options {
    Options {
        base: "origin/main".into(),
        packages: Vec::new(),
        plan: false,
        hooks: vec!["clippy".into(), "test-focused".into()],
        reuse,
    }
}

#[test]
fn automatic_plan_write_failure_stops_before_checks_and_stamps() {
    // Arrange
    let host = ScriptedHost::new();
    let mut bytes = [];

    // Act
    let result = Runner { host: &host }.run_with_output(options(false), bytes.as_mut_slice());

    // Assert
    assert!(result.is_err());
    assert_eq!(*host.calls.borrow(), ["metadata", "catalog"]);
    assert!(host.ledger.borrow().is_empty());
}

#[test]
fn automatic_selection_executes_dependents_and_plan_does_not_run_checks() {
    // Arrange
    let mut host = ScriptedHost::new();
    host.seeds.clear();
    host.changed_files = vec!["crates/core/src/lib.rs".into()];
    let runner = Runner { host: &host };

    // Act
    runner.run(options(false)).expect("automatic checks");
    host.calls.borrow_mut().clear();
    runner
        .run(Options {
            plan: true,
            ..options(false)
        })
        .expect("plan");

    // Assert
    assert_eq!(*host.calls.borrow(), ["metadata", "changes", "catalog"]);
    assert_eq!(host.ledger.borrow().len(), 2);
}

#[test]
fn shared_paths_expand_selection_and_explicit_packages_bypass_git_discovery() {
    // Arrange
    let mut host = ScriptedHost::new();
    host.seeds.clear();
    host.changed_files = vec!["Cargo.lock".into()];

    // Act
    Runner { host: &host }
        .run(Options {
            plan: true,
            ..options(false)
        })
        .expect("workspace plan");
    host.calls.borrow_mut().clear();
    host.error = "changes";
    let explicit = Runner { host: &host }.run(Options {
        packages: vec!["core".into()],
        plan: true,
        ..options(false)
    });
    let discovery_failure = Runner { host: &host }.run(options(false));

    // Assert
    assert!(explicit.is_ok());
    assert_eq!(discovery_failure.expect_err("Git failure"), "changes");
}

#[test]
fn git_path_decoding_preserves_unusual_names_and_rejects_escaping_paths() {
    // Arrange
    let paths = "crates/core/a file.rs\0crates/core/a\nfile.rs\0";

    // Act
    let decoded = ProcessHost::git_paths(paths).expect("relative paths");

    // Assert
    assert_eq!(
        decoded,
        [
            PathBuf::from("crates/core/a file.rs"),
            PathBuf::from("crates/core/a\nfile.rs")
        ]
    );
    for invalid in ["../outside\0", "/outside\0"] {
        assert!(ProcessHost::git_paths(invalid).is_err());
    }
}

#[test]
fn files_map_to_their_innermost_workspace_package_or_to_the_whole_workspace() {
    // Arrange
    let metadata = metadata();
    let root = Path::new("/workspace");
    // Act
    let owned = metadata.owners(
        &[
            "crates/app/src/main.rs".into(),
            "crates/core/src/lib.rs".into(),
            "crates/core/macros/src/lib.rs".into(),
            "crates/app/Cargo.toml".into(),
        ],
        root,
    );
    let lockfile = metadata.owners(
        &["crates/core/src/lib.rs".into(), "Cargo.lock".into()],
        root,
    );
    let outside_members = metadata.owners(&["../registry/registry/src/lib.rs".into()], root);
    // Assert
    assert_eq!(
        owned,
        Some(vec![
            "app".to_string(),
            "core".to_string(),
            "macros".to_string()
        ])
    );
    assert_eq!(lockfile, None);
    assert_eq!(outside_members, None);
}

#[test]
fn graph_includes_transitive_dependents_only_and_rejects_unknown_packages() {
    // Arrange
    let metadata = metadata();
    // Act
    let from_core = metadata.affected(&["core".into()]).expect("core graph");
    let from_support = metadata
        .affected(&["support".into()])
        .expect("support graph");
    let missing = metadata.affected(&["missing".into()]);
    // Assert
    assert_eq!(from_core, ["app", "core"]);
    assert_eq!(from_support, ["app", "core", "support"]);
    assert!(missing.is_err());
}

#[test]
fn successful_checks_are_reused_only_when_requested_and_fingerprints_match() {
    // Arrange
    let host = ScriptedHost::new();
    let runner = Runner { host: &host };
    // Act
    runner.run(options(false)).expect("fresh checks");
    runner.run(options(true)).expect("reused checks");
    host.fingerprints
        .borrow_mut()
        .extend(["changed".into(), "changed".into()]);
    runner.run(options(true)).expect("invalidated clippy");
    // Assert
    let calls = host.calls.borrow();
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == "execute-clippy")
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| *call == "execute-test-focused")
            .count(),
        1
    );
}

#[test]
fn fresh_checks_ignore_cached_results_and_failures_invalidate_old_success() {
    // Arrange
    let mut host = ScriptedHost::new();
    host.ledger
        .borrow_mut()
        .insert("clippy".into(), "same".into());
    host.error = "execute-clippy";
    // Act
    let result = Runner { host: &host }.run(options(false));
    // Assert
    assert!(result.is_err());
    assert!(!host.ledger.borrow().contains_key("clippy"));
}

#[test]
fn coverage_is_always_fresh_and_changed_inputs_cannot_create_a_stamp() {
    // Arrange
    let host = ScriptedHost::new();
    host.ledger
        .borrow_mut()
        .insert("coverage".into(), "same".into());
    let coverage = Options {
        packages: vec!["core".into()],
        hooks: vec!["coverage".into()],
        ..options(true)
    };
    // Act
    Runner { host: &host }
        .run(coverage)
        .expect("fresh coverage");
    host.fingerprints
        .borrow_mut()
        .extend(["before".into(), "after".into()]);
    let changed = Runner { host: &host }.run(options(false));
    // Assert
    assert!(host.calls.borrow().contains(&"execute-coverage".into()));
    assert!(
        changed
            .expect_err("changed input")
            .contains("Inputs changed")
    );
    assert!(!host.ledger.borrow().contains_key("clippy"));
}

#[test]
fn setup_errors_and_unknown_hooks_fail_closed() {
    // Arrange / Act / Assert
    for operation in [
        "metadata",
        "catalog",
        "lock",
        "load",
        "fingerprint",
        "save",
        "execute-test-focused",
    ] {
        let mut host = ScriptedHost::new();
        host.error = operation;
        assert!(
            Runner { host: &host }.run(options(false)).is_err(),
            "{operation}"
        );
    }
    for id in ["missing", "validate", "unsupported"] {
        assert!(catalog().hook(id).is_err());
    }
    let mut host = ScriptedHost::new();
    host.seeds.clear();
    assert!(Runner { host: &host }.run(options(false)).is_err());
    for target in ["", "../outside", "/outside"] {
        host.seeds = vec!["core".into()];
        host.target = target.into();
        assert!(Runner { host: &host }.run(options(false)).is_err());
    }
}

#[test]
fn process_lock_is_exclusive_and_ledger_round_trips() {
    // Arrange
    let directory = tempfile::tempdir_in(".").expect("fixture");
    let path = directory.path();
    let ledger = Ledger::from([("clippy".into(), "fingerprint".into())]);
    // Act
    let guard = ProcessHost.lock(path).expect("lock");
    let second = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("agentty-validation.lock"))
        .expect("second handle");
    let locked = second.try_lock();
    ProcessHost.save(path, &ledger).expect("save");
    let loaded = ProcessHost.load(path).expect("load");
    drop(guard);
    // Assert
    assert!(matches!(locked, Err(std::fs::TryLockError::WouldBlock)));
    assert_eq!(loaded, ledger);
    second.try_lock().expect("released lock");
}

#[cfg(unix)]
#[test]
fn target_resolution_covers_missing_outputs_and_rejects_cyclic_links() {
    // Arrange
    let directory = tempfile::tempdir().expect("fixture");
    let root = directory.path();
    let target = root.join("build");
    std::fs::create_dir(&target).expect("target");
    let target = target.canonicalize().expect("resolved target");
    std::os::unix::fs::symlink(target.join("missing"), root.join("absolute-output"))
        .expect("absolute output");
    std::os::unix::fs::symlink("build", root.join("linked-build")).expect("linked target");
    std::os::unix::fs::symlink("cycle", root.join("cycle")).expect("cyclic link");
    // Act
    let absolute = ProcessHost::inside_target(&root.join("absolute-output"), &target);
    let nested = ProcessHost::inside_target(&root.join("linked-build/missing/output"), &target);
    let cycle = ProcessHost::inside_target(&root.join("cycle"), &target);
    let source = ProcessHost::inside_target(root, &target);
    // Assert
    assert!(absolute);
    assert!(nested);
    assert!(!cycle);
    assert!(!source);
}

#[test]
fn command_collection_reports_spawn_errors_and_unsuccessful_exits() {
    // Arrange
    let directory = tempfile::tempdir().expect("fixture");
    let missing = directory.path().join("missing-executable");
    // Act
    let absent = ProcessHost::output(missing.to_str().expect("fixture path"), &[]);
    let failed = ProcessHost::output("sh", &["-c", "exit 17"]);
    // Assert
    assert!(
        absent
            .expect_err("missing tool")
            .contains("Could not execute")
    );
    assert_eq!(
        failed.expect_err("unsuccessful tool"),
        "sh failed while collecting validation inputs"
    );
}

#[test]
fn git_inventory_cannot_escape_the_workspace() {
    // Arrange
    let target = tempfile::tempdir().expect("target");
    // Act / Assert
    for path in [b"../source\0".as_slice(), b"/source\0".as_slice()] {
        let mut included = Vec::new();
        let result = ProcessHost::include_sources(path, target.path(), &mut |bytes| {
            included.push(bytes.to_vec());
        });
        assert_eq!(
            result.expect_err("escaped source"),
            "Git returned a path outside the workspace"
        );
        assert_eq!(included, Vec::<Vec<u8>>::new());
    }
}

#[cfg(unix)]
#[test]
fn sources_replaced_during_hashing_fail_closed() {
    // Arrange
    for stage in ["metadata", "link", "content"] {
        let directory = tempfile::tempdir_in(".").expect("fixture");
        let relative = directory
            .path()
            .canonicalize()
            .expect("fixture path")
            .strip_prefix(env::current_dir().expect("cwd"))
            .expect("workspace fixture")
            .to_path_buf();
        let parent = relative.join("inputs");
        fs::create_dir(&parent).expect("source parent");
        let source = parent.join("source");
        let target = directory
            .path()
            .canonicalize()
            .expect("fixture path")
            .join("target");
        if stage == "link" {
            symlink("missing", &source).expect("source link");
        } else {
            fs::write(&source, "source").expect("source file");
        }
        let mut inventory = source.as_os_str().as_encoded_bytes().to_vec();
        inventory.push(0);
        let mut included = 0;
        // Act
        let result = ProcessHost::include_sources(&inventory, &target, &mut |_| {
            included += 1;
            match (stage, included) {
                ("metadata", 1) => {
                    fs::remove_file(&source).expect("remove source");
                    fs::remove_dir(&parent).expect("remove parent");
                    fs::write(&parent, "replacement file").expect("replace parent");
                }
                ("link", 3) => fs::remove_file(&source).expect("remove link"),
                ("content", 2) => {
                    fs::remove_file(&source).expect("remove source");
                    symlink("source", &source).expect("cyclic replacement");
                }
                _ => {}
            }
        });
        // Assert
        assert!(
            result.is_err(),
            "{stage}: source changes must not be cached"
        );
    }
}

#[cfg(unix)]
#[test]
fn unsafe_or_unreadable_targets_and_ledgers_are_rejected() {
    // Arrange
    let directory = tempfile::tempdir_in(".").expect("fixture");
    let root = directory.path();
    let outside = root.join("outside");
    symlink("/", &outside).expect("outside target");
    let cycle = root.join("cycle");
    symlink("cycle", &cycle).expect("cyclic target");
    fs::create_dir(root.join("agentty-validation.lock")).expect("blocked lock");
    fs::create_dir(root.join("agentty-validation.json")).expect("unreadable ledger");
    // Act
    let escaped = ProcessHost.lock(&outside);
    let cyclic = ProcessHost.lock(&cycle);
    let blocked = ProcessHost.lock(root);
    let load = ProcessHost.load(root);
    let save = ProcessHost.save(root, &Ledger::new());
    let fingerprint = ProcessHost.fingerprint(&[], "clippy", Path::new("/"));
    // Assert
    assert!(matches!(escaped, Err(error) if error.contains("outside the workspace")));
    assert!(cyclic.is_err());
    assert!(blocked.is_err());
    assert!(load.is_err());
    assert!(save.is_err());
    assert!(fingerprint.is_err());
}

#[cfg(unix)]
#[test]
fn disappearing_working_directory_rejects_locking_and_fingerprinting() {
    // Arrange
    const FIXTURE_ENV: &str = "AGENTTY_VALIDATION_DELETED_CWD_TARGET";
    if let Some(target) = env::var_os(FIXTURE_ENV) {
        let cwd = env::current_dir().expect("existing cwd");
        fs::remove_dir(cwd).expect("remove child cwd");
        // Act
        let lock = ProcessHost.lock(Path::new("target"));
        let fingerprint = ProcessHost.fingerprint(&[], "clippy", Path::new(&target));
        // Assert
        assert!(lock.is_err());
        assert!(fingerprint.is_err());

        return;
    }
    let directory = tempfile::tempdir().expect("fixture");
    let cwd = directory.path().join("cwd");
    fs::create_dir(&cwd).expect("child cwd");
    // Act
    let output = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "validation::tests::disappearing_working_directory_rejects_locking_and_fingerprinting",
        ])
        .current_dir(&cwd)
        .env(FIXTURE_ENV, directory.path())
        .output()
        .expect("isolated working-directory test");
    // Assert
    assert!(output.status.success(), "{output:?}");
}

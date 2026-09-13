use std::collections::BTreeMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::Configuration;
use crate::execution::contract::{Command, Grants, Policy};

struct Fixture {
    root: TempDir,
    workspace: PathBuf,
    runtime: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture");
        let base = root.path().canonicalize().expect("canonical fixture");
        let workspace = base.join("workspace");
        let runtime = base.join("runtime");
        let scratch = base.join("scratch");
        for path in [
            &workspace,
            &runtime,
            &scratch,
            &workspace.join(".git"),
            &workspace.join("src"),
        ] {
            fs::create_dir(path).expect("fixture directory");
        }
        fs::write(workspace.join("src/file"), "original").expect("workspace file");
        fs::write(runtime.join("tool"), "runtime").expect("runtime file");

        Self {
            root,
            workspace,
            runtime,
            scratch,
        }
    }

    fn grants(&self) -> Grants {
        Grants {
            host_information: true,
            external_reads: vec![self.runtime.clone()],
            workspace_writes: vec!["src/file".into()],
            ..Grants::default()
        }
    }

    fn configuration(&self, grants: Grants) -> std::io::Result<Configuration> {
        let policy = Policy::new(
            self.workspace.clone(),
            vec![self.workspace.join(".git")],
            grants,
        )
        .expect("policy");
        let command = Command::new(self.runtime.join("tool"), vec![], ".".into()).expect("command");

        Configuration::new(command, policy, self.scratch.clone())
    }
}

#[test]
fn validates_read_only_and_existing_file_write_policies_without_side_effects() {
    // Arrange
    let fixture = Fixture::new();
    let grants = fixture.grants();

    // Act
    let configuration = fixture.configuration(grants).expect("valid configuration");

    // Assert
    assert_eq!(
        configuration.command().executable(),
        fixture.runtime.join("tool")
    );
    assert_eq!(configuration.policy().workspace(), fixture.workspace);
    assert_eq!(configuration.scratch(), fixture.scratch);
    assert_eq!(
        configuration.writable_paths().collect::<Vec<_>>(),
        [fixture.workspace.join("src/file")]
    );
    assert_eq!(fs::read_dir(&fixture.scratch).expect("scratch").count(), 0);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("src/file")).expect("content"),
        "original"
    );
    let mut grants = fixture.grants();
    grants.workspace_writes.clear();
    assert!(fixture.configuration(grants).is_ok());
}

#[test]
fn rejects_directory_missing_host_information_and_overlapping_grants() {
    // Arrange
    let fixture = Fixture::new();

    // Act / Assert
    for write in [".", "src", "missing"] {
        let mut grants = fixture.grants();
        grants.workspace_writes = vec![write.into()];
        assert!(fixture.configuration(grants).is_err(), "{write}");
    }
    let mut grants = fixture.grants();
    grants.host_information = false;
    assert!(fixture.configuration(grants).is_err());
    for read in [
        &fixture.workspace,
        &fixture.scratch,
        &fixture.runtime,
        fixture.root.path(),
    ] {
        let mut grants = fixture.grants();
        grants.external_reads.push(read.to_path_buf());
        assert!(fixture.configuration(grants).is_err());
    }
}

#[test]
fn rejects_scratch_overlap_in_both_directions_and_non_directories() {
    // Arrange
    let mut fixture = Fixture::new();

    // Act / Assert
    for scratch in [
        fixture.workspace.clone(),
        fixture.workspace.join("src"),
        fixture.workspace.parent().expect("parent").to_path_buf(),
        fixture.runtime.join("tool"),
    ] {
        fixture.scratch = scratch;
        assert!(fixture.configuration(fixture.grants()).is_err());
    }
}

#[test]
fn rejects_hardlink_symlink_socket_and_special_read_grants() {
    // Arrange
    let fixture = Fixture::new();
    let alias = fixture.workspace.join("alias");

    // Act / Assert
    fs::hard_link(fixture.workspace.join("src/file"), &alias).expect("hard link");
    assert!(fixture.configuration(fixture.grants()).is_err());
    fs::remove_file(&alias).expect("remove alias");
    symlink(fixture.runtime.join("tool"), &alias).expect("symlink");
    assert!(fixture.configuration(fixture.grants()).is_err());
    fs::remove_file(&alias).expect("remove alias");
    let socket = UnixListener::bind(&alias).expect("socket");
    assert!(fixture.configuration(fixture.grants()).is_err());
    drop(socket);
    fs::remove_file(&alias).expect("remove socket");
    let mut grants = fixture.grants();
    grants.external_reads.push("/dev/null".into());
    assert!(fixture.configuration(grants).is_err());
    assert!(fixture.configuration(fixture.grants()).is_ok());
}

#[test]
fn discovers_gitfile_and_common_metadata_before_accepting_writes() {
    // Arrange
    let fixture = Fixture::new();
    let nested = fixture.workspace.join("nested");
    fs::create_dir(&nested).expect("nested repository");
    fs::write(nested.join(".git"), "gitdir: ../src\n").expect("gitfile");

    // Act / Assert
    assert!(fixture.configuration(fixture.grants()).is_err());
    fs::write(nested.join(".git"), "gitdir: ../.git\n").expect("gitfile");
    fs::write(fixture.workspace.join(".git/commondir"), "../src\n").expect("commondir");
    assert!(fixture.configuration(fixture.grants()).is_err());
    fs::remove_file(fixture.workspace.join(".git/commondir")).expect("remove common");
    assert!(fixture.configuration(fixture.grants()).is_ok());
}

#[test]
fn rejects_malformed_git_indirections_and_non_directory_common_roots() {
    // Arrange
    let fixture = Fixture::new();
    let nested = fixture.workspace.join("nested");
    fs::create_dir(&nested).expect("nested repository");

    // Act / Assert
    for contents in ["bad", "gitdir: ../missing", "gitdir: ../src/file"] {
        fs::write(nested.join(".git"), contents).expect("gitfile");
        assert!(fixture.configuration(fixture.grants()).is_err());
    }
    fs::remove_file(nested.join(".git")).expect("remove gitfile");
    fs::write(fixture.workspace.join(".git/commondir"), "../src/file").expect("common file");
    assert!(fixture.configuration(fixture.grants()).is_err());
    fs::remove_file(fixture.workspace.join(".git/commondir")).expect("remove common");
    fs::create_dir(fixture.workspace.join(".git/commondir")).expect("unreadable common");
    assert!(fixture.configuration(fixture.grants()).is_err());
}

#[test]
fn rejects_ungranted_or_writable_executable_and_invalid_working_directory() {
    // Arrange
    let fixture = Fixture::new();

    // Act / Assert
    let mut grants = fixture.grants();
    grants.external_reads.clear();
    assert!(fixture.configuration(grants).is_err());
    for (executable, directory) in [
        (fixture.workspace.join("src/file"), "."),
        (fixture.runtime.clone(), "."),
        (fixture.runtime.join("tool"), "src/file"),
    ] {
        let policy = Policy::new(
            fixture.workspace.clone(),
            vec![fixture.workspace.join(".git")],
            fixture.grants(),
        )
        .expect("policy");
        let command = Command::new(executable, vec![], directory.into()).expect("command");
        assert!(Configuration::new(command, policy, fixture.scratch.clone()).is_err());
    }
}

#[test]
fn revalidation_rejects_replaced_source_and_noncanonical_roots() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture
        .configuration(fixture.grants())
        .expect("configuration");
    let alias = fixture.workspace.join("src/file");

    // Act
    fs::remove_file(&alias).expect("remove file");
    symlink(fixture.runtime.join("tool"), &alias).expect("replace with symlink");

    // Assert
    assert!(configuration.validate().is_err());
    assert!(super::canonical(&alias).is_err());
    assert!(super::canonical(Path::new("relative")).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn rejects_pseudo_filesystems_even_when_the_entry_looks_like_a_regular_file() {
    // Arrange
    let mut metadata = Vec::new();

    // Act / Assert
    assert!(
        super::inspect_tree(
            Path::new("/proc/self/status"),
            &mut metadata,
            &mut BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        super::inspect_tree(Path::new("/dev/null"), &mut metadata, &mut BTreeMap::new()).is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn rejects_a_host_fifo_inside_an_otherwise_permitted_tree() {
    // Arrange
    let fixture = Fixture::new();
    // Act / Assert
    for path in ["host-pipe", ".git/commondir"] {
        let fifo = fixture.workspace.join(path);
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RWXU)
            .expect("host FIFO control");
        assert!(fixture.configuration(fixture.grants()).is_err());
        fs::remove_file(fifo).expect("remove FIFO");
    }
}

#[test]
fn preserves_trailing_spaces_in_gitfile_and_common_directory_targets() {
    // Arrange
    let fixture = Fixture::new();
    for name in ["metadata", "metadata ", "nested"] {
        fs::create_dir(fixture.workspace.join(name)).expect("metadata directory");
    }
    fs::write(fixture.workspace.join("metadata /index"), "protected").expect("index");
    let gitfile = fixture.workspace.join("nested/.git");
    let common = fixture.workspace.join(".git/commondir");

    // Act / Assert
    for (path, content) in [
        (&gitfile, "gitdir: ../metadata \r\n"),
        (&common, "../metadata \r\n"),
    ] {
        fs::write(path, content).expect("indirection");
        assert!(fixture.configuration(fixture.grants()).is_ok());
        let mut grants = fixture.grants();
        grants.workspace_writes = vec!["metadata /index".into()];
        assert!(fixture.configuration(grants).is_err());
        fs::remove_file(path).expect("remove indirection");
    }
}

#[test]
fn protects_nested_bare_repositories_and_bounds_reference_reads() {
    // Arrange
    let fixture = Fixture::new();
    let bare = fixture.workspace.join("bare");
    fs::create_dir(&bare).expect("bare root");
    for directory in ["objects", "refs"] {
        fs::create_dir(bare.join(directory)).expect("bare directory");
    }
    fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").expect("bare HEAD");

    // Act / Assert
    assert!(fixture.configuration(fixture.grants()).is_ok());
    let mut grants = fixture.grants();
    grants.workspace_writes = vec!["bare/HEAD".into()];
    assert!(fixture.configuration(grants).is_err());
    fs::write(
        fixture.workspace.join(".git/commondir"),
        format!("../bare{}", "\n".repeat(1_048_577)),
    )
    .expect("oversized indirection");
    assert!(fixture.configuration(fixture.grants()).is_err());
}

#[test]
fn accepts_a_trusted_gitfile_root_without_treating_it_as_a_directory() {
    // Arrange
    let fixture = Fixture::new();
    let gitfile = fixture.workspace.join(".git");
    fs::remove_dir(&gitfile).expect("replace Git directory");
    fs::write(&gitfile, "gitdir: ../runtime\n").expect("trusted gitfile");

    // Act / Assert
    assert!(fixture.configuration(fixture.grants()).is_ok());
}

#[test]
fn rejects_alternates_in_supplied_discovered_and_common_git_directories() {
    // Arrange
    let fixture = Fixture::new();
    let external = fixture.root.path().join("administration");
    fs::create_dir(&external).expect("external Git directory");
    let nested = fixture.workspace.join("nested");
    fs::create_dir(&nested).expect("nested repository");
    fs::write(
        nested.join(".git"),
        format!("gitdir: {}\n", external.display()),
    )
    .expect("gitfile");
    let common = fixture.root.path().join("common");
    fs::create_dir(&common).expect("common directory");
    fs::write(external.join("commondir"), "../common\n").expect("common reference");

    // Act / Assert
    for directory in [fixture.workspace.join(".git"), external, common] {
        let info = directory.join("objects/info");
        fs::create_dir_all(&info).expect("object info");
        let alternates = info.join("alternates");
        fs::write(&alternates, "../../../../workspace/src\n").expect("alternate reference");
        let error = fixture
            .configuration(fixture.grants())
            .err()
            .expect("reject alternates");
        assert!(error.to_string().contains("alternate"), "{error}");
        fs::remove_file(alternates).expect("remove reference");
        assert!(fixture.configuration(fixture.grants()).is_ok());
    }
}

#[test]
fn rejects_scratch_inside_supplied_and_resolved_git_administration() {
    // Arrange
    let fixture = Fixture::new();
    let external = fixture.root.path().join("administration");
    fs::create_dir(&external).expect("external Git directory");
    let scratch = external.join("scratch");
    fs::create_dir(&scratch).expect("scratch directory");

    // Act / Assert
    for discovered in [false, true] {
        let git = if discovered {
            fs::write(
                fixture.workspace.join(".git/commondir"),
                format!("{}\n", external.display()),
            )
            .expect("common directory reference");
            fixture.workspace.join(".git")
        } else {
            external.clone()
        };
        let policy =
            Policy::new(fixture.workspace.clone(), vec![git], fixture.grants()).expect("policy");
        let command =
            Command::new(fixture.runtime.join("tool"), vec![], ".".into()).expect("command");
        let error = Configuration::new(command, policy, scratch.clone())
            .err()
            .expect("reject overlap");
        assert!(
            error.to_string().contains("scratch overlaps protected Git"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&scratch).expect("scratch").count(), 0);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn rejects_native_bind_mount_aliases() {
    // Arrange / Act / Assert
    if let Some(base) = std::env::var_os("AG_HARNESS_ALIAS_ROOT") {
        let base = PathBuf::from(base);
        let source = PathBuf::from(std::env::var_os("AG_HARNESS_ALIAS_SOURCE").expect("source"));
        let target = PathBuf::from(std::env::var_os("AG_HARNESS_ALIAS_TARGET").expect("target"));
        let original = fs::metadata(&source).expect("source metadata");
        let alias = fs::metadata(&target).expect("target metadata");
        assert_eq!((original.dev(), original.ino()), (alias.dev(), alias.ino()));
        assert_ne!(
            fs::canonicalize(&source).expect("source"),
            fs::canonicalize(&target).expect("target")
        );
        let policy = Policy::new(
            base.join("workspace"),
            vec![base.join("workspace/.git"), base.join("administration")],
            Grants {
                host_information: true,
                external_reads: vec![base.join("runtime")],
                workspace_writes: vec!["src/file".into()],
                ..Grants::default()
            },
        )
        .expect("policy");
        let command = Command::new(base.join("runtime/tool"), vec![], ".".into()).expect("command");
        let error = Configuration::new(command, policy, base.join("scratch"))
            .err()
            .expect("reject alias");
        assert!(error.to_string().contains("alias"), "{error}");

        return;
    }
    let fixture = Fixture::new();
    let base = fixture.root.path();
    fs::create_dir(base.join("administration")).expect("Git administration");
    fs::write(base.join("administration/index"), "protected").expect("Git index");
    for (source, target) in [
        (fixture.workspace.join("src"), fixture.scratch.clone()),
        (
            base.join("administration/index"),
            fixture.workspace.join("src/file"),
        ),
        (
            fixture.workspace.join("src/file"),
            fixture.runtime.join("tool"),
        ),
        (base.join("administration"), fixture.scratch.clone()),
    ] {
        let bubblewrap =
            std::env::var_os("AG_HARNESS_BWRAP").unwrap_or_else(|| "/usr/bin/bwrap".into());
        let mut command = tokio::process::Command::new(bubblewrap);
        command
            .kill_on_drop(true)
            .env("AG_HARNESS_ALIAS_ROOT", base)
            .env("AG_HARNESS_ALIAS_SOURCE", &source)
            .env("AG_HARNESS_ALIAS_TARGET", &target)
            .args([
                "--unshare-user",
                "--unshare-pid",
                "--unshare-net",
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--bind",
            ])
            .arg(base)
            .arg(base)
            .arg("--bind")
            .arg(source)
            .arg(target)
            .arg("--")
            .arg(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "execution::linux::configuration::tests::rejects_native_bind_mount_aliases",
                "--nocapture",
            ]);
        let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
            .await
            .expect("alias test deadline")
            .expect("owned alias subprocess");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn rejects_repeated_inode_identity_after_a_path_is_renamed() {
    // Arrange
    let fixture = Fixture::new();
    let mut metadata = Vec::new();
    let mut identities = BTreeMap::new();
    let original = fixture.runtime.join("tool");
    let renamed = fixture.runtime.join("renamed");
    super::inspect_tree(&original, &mut metadata, &mut identities).expect("first identity");

    // Act
    fs::rename(&original, &renamed).expect("same inode through a new path");
    let error = super::inspect_tree(&renamed, &mut metadata, &mut identities)
        .expect_err("duplicate identity");

    // Assert
    assert!(error.to_string().contains("alias"));
}

#[test]
fn rejects_uninspectable_git_object_store_layouts() {
    // Arrange
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join(".git/objects"), "not a directory")
        .expect("invalid object store");

    // Act
    let result = fixture.configuration(fixture.grants());

    // Assert
    assert!(result.is_err());
}

#[test]
fn validates_only_fresh_launch_directories_below_the_shared_scratch_root() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture
        .configuration(fixture.grants())
        .expect("configuration");
    let active = fixture.scratch.join("active");
    fs::create_dir(&active).expect("active launch");
    symlink(fixture.runtime.join("tool"), active.join("link")).expect("active symlink");
    let fresh = tempfile::tempdir_in(&fixture.scratch).expect("new launch");
    let invalid = fixture.scratch.join("file");
    fs::write(&invalid, "file").expect("not a directory");

    // Act / Assert
    configuration
        .validate()
        .expect("ignore another launch's contents");
    configuration
        .validate_launch_directory(fresh.path())
        .expect("fresh empty directory");
    for path in [
        &fixture.scratch,
        &fixture.workspace,
        &active,
        &invalid,
        &active.join("link"),
    ] {
        assert!(
            configuration.validate_launch_directory(path).is_err(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn rejects_a_git_repository_as_the_shared_scratch_root() {
    // Arrange
    let mut fixture = Fixture::new();
    for name in ["objects", "refs"] {
        fs::create_dir(fixture.scratch.join(name)).expect("bare repository directory");
    }
    fs::write(fixture.scratch.join("HEAD"), "ref: refs/heads/main\n").expect("bare HEAD");

    // Act / Assert
    assert!(fixture.configuration(fixture.grants()).is_err());
    fixture.scratch = fixture.scratch.join(".git");
    fs::create_dir(&fixture.scratch).expect("Git administration root");
    assert!(fixture.configuration(fixture.grants()).is_err());
}

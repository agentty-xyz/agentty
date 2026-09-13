use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::{fs, io};

use tempfile::TempDir;

use crate::execution::contract::{Command, Grants, Policy};
use crate::execution::macos::configuration::{
    Configuration, GIT_METADATA_BYTES, enqueue_entries, git_path, read_git_metadata,
    remove_directory_contents, scan_trees_with_budget, trees_overlap, validate_device,
    validate_entry, validate_filesystem,
};

struct Fixture {
    root: TempDir,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("create owned fixture root");
        let workspace = root
            .path()
            .canonicalize()
            .expect("canonical fixture path")
            .join("workspace");
        fs::create_dir(&workspace).expect("native isolation fixture operation succeeds");
        fs::create_dir(workspace.join(".git"))
            .expect("native isolation fixture operation succeeds");

        Self { root, workspace }
    }

    fn grants() -> Grants {
        Grants {
            host_information: true,
            external_entries: vec!["/".into(), "/usr/bin/env".into(), "/usr/bin/true".into()],
            ..Grants::default()
        }
    }

    fn scratch(&self) -> TempDir {
        TempDir::new_in(
            self.root
                .path()
                .canonicalize()
                .expect("canonical fixture path"),
        )
        .expect("native isolation fixture operation succeeds")
    }

    fn configuration(&self, grants: Grants, scratch: TempDir) -> std::io::Result<Configuration> {
        let policy = Policy::new(
            self.workspace.clone(),
            vec![self.workspace.join(".git")],
            grants,
        )
        .expect("native isolation fixture operation succeeds");

        Configuration::new(policy, scratch)
    }
}

#[test]
fn unsupported_host_policy_cleans_owned_scratch() {
    // Arrange
    let fixture = Fixture::new();
    let scratch = fixture.scratch();
    let path = scratch.path().to_owned();

    // Act
    let result = fixture.configuration(Grants::default(), scratch);

    // Assert
    assert!(result.is_err());
    assert!(!path.exists());
}

#[test]
fn aliases_special_files_and_noncanonical_paths_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let file = fixture.workspace.join("file");
    fs::write(&file, "data").expect("native isolation fixture operation succeeds");
    let link = fixture.workspace.join("link");
    symlink(&file, &link).expect("native isolation fixture operation succeeds");
    let socket_path = fixture.workspace.join("socket");
    let socket =
        UnixListener::bind(&socket_path).expect("native isolation fixture operation succeeds");

    // Act / Assert
    for path in [
        Path::new("relative"),
        Path::new("/dev/null"),
        &link,
        &socket_path,
        &fixture.workspace.join("é"),
    ] {
        assert!(validate_entry(path).is_err(), "{path:?}");
    }
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
    fs::remove_file(link).expect("native isolation fixture operation succeeds");
    drop(socket);
    fs::remove_file(socket_path).expect("native isolation fixture operation succeeds");
    fs::hard_link(&file, fixture.workspace.join("hardlink"))
        .expect("native isolation fixture operation succeeds");
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
}

#[test]
fn overlapping_trees_and_file_write_grants_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let nested =
        TempDir::new_in(&fixture.workspace).expect("native isolation fixture operation succeeds");
    let scratch = fixture.scratch();
    let scratch_path = scratch.path().to_owned();
    let file = fixture.workspace.join("file");
    fs::write(&file, "data").expect("native isolation fixture operation succeeds");

    // Act / Assert
    assert!(fixture.configuration(Fixture::grants(), nested).is_err());
    let grants = Grants {
        external_reads: vec![scratch_path],
        ..Fixture::grants()
    };
    assert!(fixture.configuration(grants, scratch).is_err());
    let grants = Grants {
        workspace_writes: vec!["file".into()],
        ..Fixture::grants()
    };
    assert!(fixture.configuration(grants, fixture.scratch()).is_err());
    let policy = Policy::new(
        file,
        vec![fixture.workspace.join(".git")],
        Fixture::grants(),
    )
    .expect("native isolation fixture operation succeeds");
    assert!(Configuration::new(policy, fixture.scratch()).is_err());
}

#[test]
fn discovered_git_directories_inside_scratch_are_rejected() {
    // Arrange
    for common_directory in [false, true] {
        let fixture = Fixture::new();
        let scratch = fixture.scratch();
        let scratch_path = scratch.path().to_owned();
        let metadata = scratch.path().join("admin");
        fs::create_dir(&metadata).expect("create nested metadata directory");
        let git = fixture.workspace.join(".git");
        if common_directory {
            fs::write(
                git.join("commondir"),
                metadata.to_str().expect("ASCII metadata path"),
            )
            .expect("point common directory into scratch");
        } else {
            fs::remove_dir(&git).expect("remove Git directory");
            fs::write(&git, format!("gitdir: {}\n", metadata.display()))
                .expect("point linked worktree into scratch");
        }

        // Act
        let error = fixture
            .configuration(Fixture::grants(), scratch)
            .err()
            .expect("reject discovered metadata inside scratch");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("overlaps scratch"));
        assert!(
            !scratch_path.exists(),
            "validation failure releases owned scratch"
        );
        assert!(git.exists(), "the workspace Git entry survives cleanup");
    }
}

#[test]
fn prelaunch_validation_rejects_git_retargeted_into_scratch() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .expect("initial disjoint configuration");
    let command = Command::new("/usr/bin/true".into(), vec![], ".".into()).expect("valid command");
    configuration
        .profile(&command)
        .expect("initial profile is valid");
    let metadata = configuration.scratch().join("admin");
    fs::create_dir(&metadata).expect("create nested metadata directory");
    fs::write(
        fixture.workspace.join(".git/commondir"),
        metadata.to_str().expect("ASCII metadata path"),
    )
    .expect("retarget common directory after construction");

    // Act
    let error = configuration
        .profile(&command)
        .expect_err("revalidation rejects the overlap before launch");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("overlaps scratch"));
    assert!(metadata.exists(), "revalidation retains scratch ownership");
}

#[test]
fn git_indirections_are_protected_and_invalid_forms_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let admin = fixture.workspace.join("admin");
    let common = fixture.workspace.join("common");
    fs::create_dir(&admin).expect("native isolation fixture operation succeeds");
    fs::create_dir(&common).expect("native isolation fixture operation succeeds");
    fs::write(admin.join("commondir"), "../common\n")
        .expect("native isolation fixture operation succeeds");
    fs::remove_dir(fixture.workspace.join(".git"))
        .expect("native isolation fixture operation succeeds");
    fs::write(fixture.workspace.join(".git"), "gitdir: admin\n")
        .expect("native isolation fixture operation succeeds");
    let configuration = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .expect("native isolation fixture operation succeeds");
    let command = Command::new("/usr/bin/true".into(), vec![], ".".into())
        .expect("native isolation fixture operation succeeds");

    // Act
    let profile = configuration
        .profile(&command)
        .expect("native isolation fixture operation succeeds");

    // Assert
    for path in [admin, common] {
        assert!(profile.contains(&format!(
            "(deny file-write* (subpath \"{}\"))",
            path.display()
        )));
    }
    fs::write(fixture.workspace.join(".git"), "not a gitdir")
        .expect("native isolation fixture operation succeeds");
    assert!(configuration.profile(&command).is_err());
    fs::write(fixture.workspace.join(".git"), "gitdir: .\n")
        .expect("native isolation fixture operation succeeds");
    assert!(configuration.profile(&command).is_err());
}

#[test]
fn object_alternates_and_validation_exhaustion_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let info = fixture.workspace.join(".git/objects/info");
    fs::create_dir_all(&info).expect("native isolation fixture operation succeeds");
    fs::write(info.join("alternates"), "elsewhere\n")
        .expect("native isolation fixture operation succeeds");

    // Act / Assert
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
    assert!(scan_trees_with_budget([fixture.workspace.as_path()], &mut vec![], 0).is_err());
}

#[test]
fn wide_directories_stop_enumerating_when_the_queue_budget_is_exhausted() {
    // Arrange
    let fixture = Fixture::new();
    let wide = fixture.workspace.join("wide");
    fs::create_dir(&wide).expect("create wide directory");
    for index in 0..2048 {
        fs::write(wide.join(format!("entry-{index}")), []).expect("create directory entry");
    }
    let mut enumerated = 0;
    let entries = fs::read_dir(&wide)
        .expect("open wide directory")
        .map(|entry| {
            enumerated += 1;

            entry.map(|entry| entry.path())
        });
    let mut remaining = 16;
    let mut pending = Vec::new();

    // Act
    let result = enqueue_entries(entries, &mut pending, &mut remaining);

    // Assert
    assert_eq!(
        result.expect_err("reject excess entries").kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(enumerated, 17, "stop at the first entry over the limit");
    assert_eq!(pending.len(), 16, "never enqueue beyond the budget");
    assert_eq!(remaining, 0);
    assert!(scan_trees_with_budget([wide.as_path()], &mut vec![], 2048).is_err());
    scan_trees_with_budget([wide.as_path()], &mut vec![], 2049)
        .expect("budget includes root and every child");
}

#[test]
fn directory_entry_errors_are_propagated_without_queueing_a_path() {
    // Arrange
    let entries = [Err(io::Error::from(io::ErrorKind::PermissionDenied))];
    let mut pending = Vec::new();
    let mut remaining = 1;

    // Act
    let error = enqueue_entries(entries, &mut pending, &mut remaining)
        .expect_err("propagate directory enumeration failure");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(pending, Vec::<PathBuf>::new());
}

#[test]
fn non_git_alternates_suffixes_are_accepted_in_granted_trees() {
    // Arrange
    let fixture = Fixture::new();
    let external = fixture.scratch();
    let scratch = fixture.scratch();
    for root in [fixture.workspace.as_path(), external.path(), scratch.path()] {
        let info = root.join("fixtures/objects/info");
        fs::create_dir_all(&info).expect("create ordinary fixture directories");
        fs::write(info.join("alternates"), "fixture contents").expect("write ordinary fixture");
    }
    let grants = Grants {
        external_reads: vec![external.path().to_owned()],
        ..Fixture::grants()
    };
    let command = Command::new("/usr/bin/true".into(), vec![], ".".into()).expect("valid command");

    // Act
    let configuration = fixture.configuration(grants, scratch);

    // Assert
    configuration
        .expect("ordinary files are not Git alternates")
        .profile(&command)
        .expect("prelaunch revalidation accepts ordinary fixtures");
}

#[test]
fn alternates_in_explicit_git_metadata_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let admin = fixture.scratch();
    fs::create_dir_all(admin.path().join("objects/info")).expect("create supplied Git objects");
    fs::write(admin.path().join("objects/info/alternates"), "elsewhere\n")
        .expect("write Git alternates");
    let policy = Policy::new(
        fixture.workspace.clone(),
        vec![admin.path().to_owned()],
        Fixture::grants(),
    )
    .expect("valid supplied Git metadata policy");

    // Act
    let error = Configuration::new(policy, fixture.scratch())
        .err()
        .expect("reject Git alternates");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), "Git object alternates are unsupported");
}

#[test]
fn alternates_in_common_git_metadata_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let admin = fixture.workspace.join("admin");
    let common = fixture.workspace.join("common");
    fs::create_dir(&admin).expect("create worktree administration");
    fs::create_dir_all(common.join("objects/info")).expect("create common Git objects");
    fs::write(common.join("objects/info/alternates"), "elsewhere\n").expect("write Git alternates");
    fs::write(admin.join("commondir"), "../common\n").expect("write common directory pointer");
    fs::remove_dir(fixture.workspace.join(".git")).expect("remove initial Git directory");
    fs::write(fixture.workspace.join(".git"), "gitdir: admin\n").expect("write Git pointer");

    // Act
    let error = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .err()
        .expect("reject common Git alternates");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(error.to_string(), "Git object alternates are unsupported");
}

#[test]
fn explicit_external_trees_profile_quoting_and_working_directories() {
    // Arrange
    let fixture = Fixture::new();
    let external = fixture
        .root
        .path()
        .canonicalize()
        .expect("native isolation fixture operation succeeds")
        .join("quoted-\"\\");
    fs::create_dir(&external).expect("native isolation fixture operation succeeds");
    let grants = Grants {
        external_reads: vec![external],
        ..Fixture::grants()
    };
    let configuration = fixture
        .configuration(grants, fixture.scratch())
        .expect("native isolation fixture operation succeeds");
    fs::write(fixture.workspace.join("file"), "data")
        .expect("native isolation fixture operation succeeds");
    let command = Command::new("/usr/bin/true".into(), vec![], "file".into())
        .expect("native isolation fixture operation succeeds");

    // Act / Assert
    assert!(configuration.profile(&command).is_err());
    let command = Command::new("/usr/bin/true".into(), vec![], ".".into())
        .expect("native isolation fixture operation succeeds");
    assert!(
        configuration
            .profile(&command)
            .expect("native isolation fixture operation succeeds")
            .contains("quoted-\\\"\\\\")
    );
    let command = Command::new(fixture.workspace, vec![], ".".into())
        .expect("native isolation fixture operation succeeds");
    assert!(configuration.profile(&command).is_err());
}

#[test]
fn nested_device_identity_is_rejected() {
    // Arrange / Act / Assert
    assert!(validate_device(1, 1).is_ok());
    assert!(validate_device(2, 1).is_err());
}

#[test]
fn case_aliases_and_volume_alias_overlap_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let alias = fixture.workspace.with_file_name("WORKSPACE");
    let data_alias = PathBuf::from("/System/Volumes/Data").join(
        fixture
            .workspace
            .strip_prefix("/")
            .expect("absolute fixture"),
    );

    // Act / Assert
    if alias.exists() {
        assert_ne!(
            alias.canonicalize().expect("native canonical spelling"),
            alias
        );
        assert!(validate_entry(&alias).is_err());
        assert!(trees_overlap(&fixture.workspace, &alias).expect("compare case alias identity"));
    }
    assert!(trees_overlap(&fixture.workspace, &fixture.workspace).expect("same tree"));
    assert!(!trees_overlap(&fixture.workspace, fixture.scratch().path()).expect("disjoint trees"));
    if data_alias.exists() {
        assert!(
            trees_overlap(&fixture.workspace, &data_alias).expect("compare firmlink identities")
        );
    }
}

#[test]
fn ambiguous_git_indirections_are_rejected_without_trimming() {
    // Arrange / Act / Assert
    for text in [
        "",
        "\n",
        " admin\n",
        "admin \n",
        "admin\nother",
        "admin\r\n",
    ] {
        assert!(git_path(text).is_err(), "{text:?}");
    }
    assert_eq!(
        git_path("../admin\n").expect("relative indirection"),
        "../admin"
    );
}

#[test]
fn alternates_in_an_external_git_directory_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let admin = fixture
        .root
        .path()
        .canonicalize()
        .expect("canonical root")
        .join("admin");
    fs::create_dir_all(admin.join("objects/info")).expect("create external git objects");
    fs::write(admin.join("objects/info/alternates"), "elsewhere\n").expect("write alternates");
    fs::remove_dir(fixture.workspace.join(".git")).expect("remove git directory");
    fs::write(fixture.workspace.join(".git"), "gitdir: ../admin\n").expect("write git pointer");

    // Act / Assert
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
}

#[test]
fn non_apfs_entries_are_unsupported() {
    // Arrange / Act / Assert
    assert!(validate_filesystem(&b"apfs\0".map(u8::cast_signed)).is_ok());
    assert!(validate_filesystem(&b"hfs\0".map(u8::cast_signed)).is_err());
}

#[test]
fn git_metadata_cannot_use_an_alternate_workspace_mount_path() {
    // Arrange
    let fixture = Fixture::new();
    let data_alias = PathBuf::from("/System/Volumes/Data").join(
        fixture
            .workspace
            .strip_prefix("/")
            .expect("absolute fixture"),
    );
    fs::create_dir(fixture.workspace.join("admin")).expect("create admin directory");
    fs::remove_dir(fixture.workspace.join(".git")).expect("remove git directory");

    // Act / Assert
    if data_alias.exists() {
        for path in [data_alias.clone(), data_alias.join("admin")] {
            fs::write(
                fixture.workspace.join(".git"),
                format!("gitdir: {}\n", path.display()),
            )
            .expect("write alias pointer");
            assert!(
                fixture
                    .configuration(Fixture::grants(), fixture.scratch())
                    .is_err()
            );
        }
    }
}

#[test]
fn malformed_git_targets_fail_during_tree_validation() {
    // Arrange
    let fixture = Fixture::new();
    fs::remove_dir(fixture.workspace.join(".git")).expect("remove git directory");
    fs::create_dir(fixture.workspace.join("admin")).expect("create admin directory");

    // Act / Assert
    fs::write(fixture.workspace.join(".git"), "gitdir:  admin\n").expect("write ambiguous pointer");
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
    fs::write(fixture.workspace.join(".git"), "gitdir: admin\n").expect("write valid pointer");
    fs::write(fixture.workspace.join("admin/commondir"), "missing\n")
        .expect("write dangling common directory");
    assert!(
        fixture
            .configuration(Fixture::grants(), fixture.scratch())
            .is_err()
    );
}

#[test]
fn oversized_git_and_common_directory_records_are_rejected() {
    // Arrange
    let fixture = Fixture::new();
    let git = fixture.workspace.join(".git");
    let common = fixture.workspace.join("admin/commondir");
    fs::remove_dir(&git).expect("remove git directory");
    fs::create_dir(fixture.workspace.join("admin")).expect("create admin directory");

    // Act / Assert
    for path in [&git, &common] {
        fs::write(&git, "gitdir: admin\n").expect("write git pointer");
        fs::File::create(path)
            .expect("create oversized record")
            .set_len(1 << 30)
            .expect("extend sparse record without allocating its contents");
        let scratch = fixture.scratch();
        let scratch_path = scratch.path().to_owned();
        let error = fixture
            .configuration(Fixture::grants(), scratch)
            .err()
            .expect("oversized metadata is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("4096 bytes"));
        assert!(!scratch_path.exists());
    }
}

#[test]
fn git_metadata_reader_bounds_content_and_propagates_read_errors() {
    // Arrange
    let fixture = Fixture::new();
    let path = fixture.workspace.join("record");
    let content = "a".repeat(usize::try_from(GIT_METADATA_BYTES).expect("small byte limit"));
    fs::write(&path, &content).expect("write limit-sized record");

    // Act / Assert
    assert_eq!(
        read_git_metadata(&path).expect("accept exact limit"),
        content
    );
    fs::write(&path, [0xff]).expect("write invalid UTF-8");
    assert_eq!(
        read_git_metadata(&path)
            .expect_err("reject invalid UTF-8")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    fs::remove_file(&path).expect("remove record");
    assert!(read_git_metadata(&path).is_err());
}

#[test]
fn executable_paths_with_equals_are_rejected_before_profile_construction() {
    // Arrange
    let fixture = Fixture::new();
    let executable = fixture.workspace.join("tool=x");
    fs::write(&executable, "placeholder").expect("create executable entry");
    let configuration = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .expect("valid filesystem policy");
    let command = Command::new(executable, vec![], ".".into()).expect("valid portable command");

    // Act
    let error = configuration
        .profile(&command)
        .expect_err("reject ambiguous executable");

    // Assert
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("containing '='"));
}

#[test]
fn cleanup_repairs_directory_access_without_following_symlinks() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .expect("valid configuration");
    let nested = configuration.scratch().join("locked/nested");
    fs::create_dir_all(&nested).expect("create scratch directories");
    fs::write(nested.join("file"), "contents").expect("create scratch file");
    symlink(&fixture.workspace, nested.join("outside")).expect("simulate a host-added symlink");
    fs::set_permissions(&fixture.workspace, fs::Permissions::from_mode(0o750))
        .expect("set external permissions");
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o0))
        .expect("remove nested directory access");
    fs::set_permissions(
        configuration.scratch().join("locked"),
        fs::Permissions::from_mode(0o0),
    )
    .expect("remove parent directory access");
    let scratch = configuration.scratch().to_owned();

    // Act
    configuration
        .cleanup()
        .expect("restore access and remove scratch");
    configuration.cleanup().expect("cleanup is idempotent");

    // Assert
    assert!(!scratch.exists());
    assert_eq!(
        fs::metadata(&fixture.workspace)
            .expect("external tree survives")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(remove_directory_contents(&scratch).is_err());
}

#[test]
fn cleanup_does_not_follow_a_replaced_scratch_root() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture
        .configuration(Fixture::grants(), fixture.scratch())
        .expect("valid configuration");
    let scratch = configuration.scratch().to_owned();
    let retained = fixture.workspace.join("retained");
    fs::write(&retained, "workspace contents").expect("create workspace file");
    fs::remove_dir(&scratch).expect("simulate a host removing the scratch root");
    symlink(&fixture.workspace, &scratch).expect("replace root with a host-created symlink");

    // Act
    let result = configuration.cleanup();

    // Assert
    assert!(
        result.is_err(),
        "a replaced root cannot be removed as a directory"
    );
    assert_eq!(
        fs::read_to_string(retained).expect("target survives"),
        "workspace contents"
    );
    assert!(
        fs::symlink_metadata(&scratch)
            .expect("link is not followed")
            .is_symlink()
    );
    drop(configuration);
    assert!(
        !scratch.exists(),
        "TempDir releases the root link without following it"
    );
    assert!(fixture.workspace.is_dir());
}

#[test]
fn external_git_administration_rejects_aliases_into_writable_workspace() {
    // Arrange
    for source in [
        "supplied",
        "linked",
        "common",
        "chained-common",
        "nested-git",
    ] {
        let fixture = Fixture::new();
        let admin = fixture.scratch();
        let common = fixture.scratch();
        let terminal = fixture.scratch();
        let writable = fixture.workspace.join("writable");
        fs::create_dir(&writable).expect("create writable directory");
        let target = writable.join("config");
        fs::write(&target, "metadata").expect("create writable alias target");
        let mut git_metadata = vec![fixture.workspace.join(".git")];
        if source == "supplied" {
            git_metadata.push(admin.path().to_owned());
        } else {
            fs::remove_dir(fixture.workspace.join(".git")).expect("remove Git directory");
            fs::write(
                fixture.workspace.join(".git"),
                format!("gitdir: {}\n", admin.path().display()),
            )
            .expect("write linked worktree pointer");
        }
        let alias_root = match source {
            "common" | "chained-common" => {
                fs::write(
                    admin.path().join("commondir"),
                    common.path().to_str().expect("ASCII path"),
                )
                .expect("write common directory pointer");
                if source == "chained-common" {
                    fs::write(
                        common.path().join("commondir"),
                        terminal.path().to_str().expect("ASCII path"),
                    )
                    .expect("write another common directory pointer");
                    terminal.path()
                } else {
                    common.path()
                }
            }
            "nested-git" => {
                fs::write(
                    admin.path().join(".git"),
                    format!("gitdir: {}\n", terminal.path().display()),
                )
                .expect("write nested Git pointer");
                terminal.path()
            }
            _ => admin.path(),
        };
        fs::create_dir(alias_root.join("nested")).expect("create nested metadata");
        symlink(&target, alias_root.join("nested/config")).expect("create metadata alias");
        let policy = Policy::new(
            fixture.workspace.clone(),
            git_metadata,
            Grants {
                workspace_writes: vec!["writable".into()],
                ..Fixture::grants()
            },
        )
        .expect("valid lexical grants");
        let scratch = fixture.scratch();
        let scratch_path = scratch.path().to_owned();

        // Act
        let error = Configuration::new(policy, scratch)
            .err()
            .expect("reject metadata alias");

        // Assert
        assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{source}");
        assert!(
            error.to_string().contains("noncanonical"),
            "{source}: {error}"
        );
        assert!(!scratch_path.exists(), "{source}");
        assert_eq!(
            fs::read_to_string(&target).expect("read alias target"),
            "metadata"
        );
    }
}

#[test]
fn git_graph_shares_a_budget_and_deduplicates_overlaps_and_cycles() {
    // Arrange
    let fixture = Fixture::new();
    let first = fixture.scratch();
    let second = fixture.scratch();
    let third = fixture.scratch();
    for (root, next) in [
        (first.path(), second.path()),
        (second.path(), third.path()),
        (third.path(), first.path()),
    ] {
        fs::write(root.join("commondir"), next.to_str().expect("ASCII path"))
            .expect("write cyclic common directory pointer");
    }
    let entry = first.path().join("commondir");
    let mut protected = vec![first.path().to_owned(), first.path().to_owned()];

    // Act
    let result = scan_trees_with_budget([first.path(), entry.as_path()], &mut protected, 6);

    // Assert
    result.expect("each of the six unique entries consumes one budget unit");
    let mut expected = vec![
        first.path().to_owned(),
        second.path().to_owned(),
        third.path().to_owned(),
    ];
    expected.sort_unstable();
    assert_eq!(protected, expected);
    scan_trees_with_budget(
        [entry.as_path(), first.path()],
        &mut vec![first.path().to_owned()],
        7,
    )
    .expect("overlapping parent checks the repeated entry without traversing it again");
    let error = scan_trees_with_budget([], &mut vec![first.path().to_owned()], 5)
        .expect_err("new common directories share the original budget");
    assert_eq!(error.to_string(), "tree validation limit exceeded");
}

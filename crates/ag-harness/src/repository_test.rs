use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::{
    Repository, RepositoryError, canonical_executable_parent, containing_worktree_root,
    is_executable,
};

#[cfg(unix)]
fn executable(path: &Path) {
    fs::write(path, "#!/bin/sh\nexit 0\n").expect("executable fixture should be written");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata should exist")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("fixture should be executable");
}

#[cfg(unix)]
#[test]
fn validates_and_canonicalizes_host_repository_configuration() {
    // Arrange
    let parent = tempdir().expect("temporary parent should exist");
    let root = parent.path().join("repository");
    let executable_path = test_git_executable();
    fs::create_dir(&root).expect("repository fixture should exist");
    // A temporary sibling executable may still be inside the enclosing
    // checkout. Use the trusted host executable for this successful
    // public-constructor case.

    // Act
    let repository = Repository::new(root.join("."), &executable_path)
        .expect("valid host configuration should be accepted");

    // Assert
    assert_eq!(
        repository.root(),
        root.canonicalize()
            .expect("repository fixture should canonicalize")
    );
    assert_eq!(
        repository.git_executable(),
        executable_path
            .canonicalize()
            .expect("executable fixture should canonicalize")
    );
}

#[cfg(unix)]
#[test]
fn rejects_untrusted_git_executable_configurations() {
    // Arrange
    let parent = tempdir().expect("temporary parent should exist");
    let root = parent.path().join("repository");
    fs::create_dir(&root).expect("repository fixture should exist");
    let directory = parent.path().join("git-directory");
    fs::create_dir(&directory).expect("directory fixture should exist");
    let inert = parent.path().join("inert-git");
    fs::write(&inert, "not executable").expect("inert fixture should be written");
    let inside = root.join("git");
    executable(&inside);
    let administrative_root = parent.path().join(".GIT");
    fs::create_dir(&administrative_root).expect("administrative root fixture should exist");

    // Act
    let relative = Repository::new(&root, "git");
    let missing = Repository::new(&root, parent.path().join("missing"));
    let non_file = Repository::new(&root, &directory);
    let non_executable = Repository::new(&root, &inert);
    let inside_repository = Repository::new(&root, &inside);
    let missing_root = Repository::new(parent.path().join("missing-root"), &inside);
    let file_root = Repository::new(&inert, &inside);
    let git_root = Repository::new(&administrative_root, &inside);

    // Assert
    assert!(matches!(
        relative,
        Err(RepositoryError::GitExecutableNotAbsolute { .. })
    ));
    assert!(
        matches!(missing, Err(RepositoryError::GitExecutable { .. })),
        "unexpected missing executable result: {missing:?}"
    );
    assert!(matches!(
        non_file,
        Err(RepositoryError::GitExecutableNotFile { .. })
    ));
    assert!(matches!(
        non_executable,
        Err(RepositoryError::GitExecutableNotExecutable { .. })
    ));
    assert!(matches!(
        inside_repository,
        Err(RepositoryError::GitExecutableInsideRepository { .. })
    ));
    assert!(matches!(missing_root, Err(RepositoryError::Root { .. })));
    assert!(matches!(file_root, Err(RepositoryError::Root { .. })));
    assert!(matches!(
        git_root,
        Err(RepositoryError::RootIsGitAdministrative { .. })
    ));
}

#[cfg(unix)]
#[test]
fn rejects_git_executable_inside_containing_worktree() {
    // Arrange
    let parent = tempdir().expect("temporary parent should exist");
    let worktree = parent.path().join("checkout");
    let root = worktree.join("crate");
    fs::create_dir_all(worktree.join(".git"))
        .expect("worktree and administrative directory should exist");
    fs::create_dir(&root).expect("nested repository root should exist");
    let executable_path = worktree.join("fake-git");
    executable(&executable_path);
    let canonical_worktree = worktree
        .canonicalize()
        .expect("worktree fixture should canonicalize");

    // Act
    let result = Repository::new(&root, &executable_path);

    // Assert
    assert!(matches!(
        result,
        Err(RepositoryError::GitExecutableInsideRepository {
            path: rejected_path,
            root: rejected_root,
        }) if rejected_path == executable_path.canonicalize().expect("executable path")
            && canonical_worktree.starts_with(&rejected_root)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_git_symlink_located_inside_containing_worktree() {
    // Arrange
    let parent = tempdir().expect("temporary parent should exist");
    let worktree = parent.path().join("checkout");
    let root = worktree.join("crate");
    fs::create_dir_all(worktree.join(".git"))
        .expect("worktree and administrative directory should exist");
    fs::create_dir(&root).expect("nested repository root should exist");
    let executable_path = test_git_executable();
    let linked_executable = worktree.join("git-link");
    symlink(&executable_path, &linked_executable).expect("executable symlink should exist");

    // Act
    let result = Repository::new(&root, &linked_executable);

    // Assert
    assert!(matches!(
        result,
        Err(RepositoryError::GitExecutableInsideRepository { path, .. })
            if path == linked_executable
    ));
}

#[test]
fn containing_worktree_keeps_outer_boundary_with_nested_markers() {
    // Arrange
    let fixture = tempdir().expect("fixture");
    let outer = fixture.path().join("outer");
    let inner = outer.join("inner");
    let root = inner.join("crate");
    let metadata = fs::metadata(fixture.path()).expect("directory metadata");

    // Act
    let boundary = containing_worktree_root(&root, |path| {
        if path == outer.join(".git") || path == inner.join(".git") {
            Ok(metadata.clone())
        } else {
            Err(std::io::ErrorKind::NotFound.into())
        }
    })
    .expect("worktree boundary");

    // Assert
    assert_eq!(boundary, outer);
}

#[test]
fn rejects_uninspectable_containing_worktree_boundary() {
    // Arrange
    let repository = tempdir().expect("temporary repository root should exist");
    let root = repository
        .path()
        .canonicalize()
        .expect("repository root should canonicalize");
    let expected_root = root.clone();

    // Act
    let result = containing_worktree_root(&root, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
    });

    // Assert
    assert!(matches!(
        result,
        Err(RepositoryError::Root { path, source })
            if path == expected_root && source.kind() == std::io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn rejects_uninspectable_git_executable_parent() {
    // Arrange
    let executable = Path::new("/host/git");

    // Act
    let result = canonical_executable_parent(executable, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
    });

    // Assert
    assert!(matches!(
        result,
        Err(RepositoryError::GitExecutable { path, source })
            if path == executable && source.kind() == std::io::ErrorKind::PermissionDenied
    ));
}

#[cfg(unix)]
#[test]
fn validates_execute_permission_for_effective_identity() {
    // Arrange
    let parent = tempdir().expect("temporary parent should exist");
    let root = parent.path().join("repository");
    fs::create_dir(&root).expect("repository fixture should exist");
    let executable_path = parent.path().join("git");
    executable(&executable_path);
    let mut permissions = fs::metadata(&executable_path)
        .expect("executable metadata should exist")
        .permissions();
    permissions.set_mode(u32::from(!rustix::process::geteuid().is_root()));
    fs::set_permissions(&executable_path, permissions)
        .expect("fixture should not be executable by the effective identity");

    // Act
    let result = Repository::new(&root, &executable_path);

    // Assert
    assert!(matches!(
        result,
        Err(RepositoryError::GitExecutableNotExecutable { .. })
    ));
}

impl Repository {
    pub(crate) fn fixture(root: impl Into<PathBuf>) -> Self {
        Self {
            git_executable: test_git_executable(),
            root: root.into(),
        }
    }
}

pub(crate) fn test_git_executable() -> PathBuf {
    let executable_name = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH");
    assert!(path.is_some(), "test PATH should be configured");
    let executables = path
        .iter()
        .flat_map(|path| std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name))
        .filter_map(|candidate| candidate.canonicalize().ok())
        .filter(|path| path.is_file() && is_executable(path))
        .collect::<Vec<_>>();
    assert!(
        !executables.is_empty(),
        "trusted Git executable should be available on PATH"
    );

    executables[0].clone()
}

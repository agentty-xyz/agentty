use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustix::fs as rustix_fs;
use thiserror::Error;

/// Validated host configuration for repository-scoped tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    git_executable: PathBuf,
    root: PathBuf,
}

impl Repository {
    /// Validates a repository root and the host-selected Git executable.
    ///
    /// Both paths are canonicalized immediately. The executable and its
    /// configured location must be outside the containing worktree. On
    /// Unix, the executable must also be an executable regular file. Other
    /// platforms enforce the regular file check but defer executable-access
    /// validation to process creation.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryError`] when either path cannot be resolved or the
    /// executable does not satisfy the host-trust requirements.
    pub fn new(
        root: impl AsRef<Path>,
        git_executable: impl AsRef<Path>,
    ) -> Result<Self, RepositoryError> {
        let root = canonical_directory(root.as_ref()).map_err(|source| RepositoryError::Root {
            path: root.as_ref().to_path_buf(),
            source,
        })?;
        if root.components().any(|component| {
            component
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        }) {
            return Err(RepositoryError::RootIsGitAdministrative { path: root });
        }

        let worktree_root = containing_worktree_root(&root, |path| fs::symlink_metadata(path))?;
        let requested_executable = git_executable.as_ref();
        if !requested_executable.is_absolute() {
            return Err(RepositoryError::GitExecutableNotAbsolute {
                path: requested_executable.to_path_buf(),
            });
        }
        let git_executable = fs::canonicalize(requested_executable).map_err(|source| {
            RepositoryError::GitExecutable {
                path: requested_executable.to_path_buf(),
                source,
            }
        })?;
        if !git_executable.is_file() {
            return Err(RepositoryError::GitExecutableNotFile {
                path: git_executable,
            });
        }
        if !is_executable(&git_executable) {
            return Err(RepositoryError::GitExecutableNotExecutable {
                path: git_executable,
            });
        }
        let requested_parent =
            canonical_executable_parent(requested_executable, |path| fs::canonicalize(path))?;
        let target_inside_worktree = git_executable.starts_with(&worktree_root);
        if requested_executable.starts_with(&worktree_root)
            || requested_parent.is_some_and(|parent| parent.starts_with(&worktree_root))
            || target_inside_worktree
        {
            return Err(RepositoryError::GitExecutableInsideRepository {
                path: if target_inside_worktree {
                    git_executable
                } else {
                    requested_executable.to_path_buf()
                },
                root: worktree_root,
            });
        }

        Ok(Self {
            git_executable,
            root,
        })
    }

    pub(crate) fn git_executable(&self) -> &Path {
        &self.git_executable
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// Invalid host configuration for repository-scoped tools.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// The configured repository root could not be resolved to a directory.
    #[error("failed to resolve repository root `{path}`: {source}")]
    Root {
        /// Repository root supplied by the host.
        path: PathBuf,
        /// Filesystem failure returned while resolving the root.
        #[source]
        source: std::io::Error,
    },
    /// The configured root was itself inside Git administrative state.
    #[error("repository root must not access Git administrative state: `{path}`")]
    RootIsGitAdministrative {
        /// Canonical repository root.
        path: PathBuf,
    },
    /// The Git executable could not be resolved or inspected.
    #[error("failed to resolve Git executable `{path}`: {source}")]
    GitExecutable {
        /// Git executable supplied by the host.
        path: PathBuf,
        /// Filesystem failure returned while resolving the executable.
        #[source]
        source: std::io::Error,
    },
    /// The Git executable path was not absolute.
    #[error("Git executable must be absolute: `{path}`")]
    GitExecutableNotAbsolute {
        /// Relative executable path supplied by the host.
        path: PathBuf,
    },
    /// The configured Git executable location or canonical target was inside
    /// the containing worktree.
    #[error("Git executable `{path}` must be outside repository-controlled worktree `{root}`")]
    GitExecutableInsideRepository {
        /// Rejected configured location or canonical executable path.
        path: PathBuf,
        /// Canonical containing worktree root.
        root: PathBuf,
    },
    /// The canonical Git executable was not a regular file.
    #[error("Git executable is not a regular file: `{path}`")]
    GitExecutableNotFile {
        /// Canonical executable path.
        path: PathBuf,
    },
    /// The canonical Git executable lacked executable permissions.
    #[error("Git executable is not executable: `{path}`")]
    GitExecutableNotExecutable {
        /// Canonical executable path.
        path: PathBuf,
    },
}

fn canonical_directory(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !fs::metadata(&canonical)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a directory",
        ));
    }

    Ok(canonical)
}

fn canonical_executable_parent(
    path: &Path,
    canonicalize: impl Fn(&Path) -> std::io::Result<PathBuf>,
) -> Result<Option<PathBuf>, RepositoryError> {
    path.parent()
        .map(canonicalize)
        .transpose()
        .map_err(|source| RepositoryError::GitExecutable {
            path: path.to_path_buf(),
            source,
        })
}

fn containing_worktree_root(
    root: &Path,
    inspect_entry: impl Fn(&Path) -> std::io::Result<fs::Metadata>,
) -> Result<PathBuf, RepositoryError> {
    let mut worktree_root = root;
    for ancestor in root.ancestors() {
        match inspect_entry(&ancestor.join(".git")) {
            Ok(_) => worktree_root = ancestor,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RepositoryError::Root {
                    path: root.to_path_buf(),
                    source,
                });
            }
        }
    }

    Ok(worktree_root.to_path_buf())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    rustix_fs::accessat(
        rustix_fs::CWD,
        path,
        rustix_fs::Access::EXEC_OK,
        rustix_fs::AtFlags::EACCESS,
    )
    .is_ok()
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

#[cfg(test)]
pub(crate) use tests::test_git_executable;

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;

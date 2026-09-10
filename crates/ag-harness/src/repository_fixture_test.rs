use std::path::PathBuf;

use super::{Repository, is_executable};

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

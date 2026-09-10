use std::path::Path;

use ag_harness::Repository;

/// Builds a fixture with a trusted host Git, even when temporary directories
/// and integration test binaries live inside the checkout being tested.
pub(crate) fn repository_with_host_git(root: &Path) -> Repository {
    let executable_name = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").expect("test PATH should be configured");

    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name))
        .find_map(|executable| Repository::new(root, executable).ok())
        .expect("repository fixture should accept a trusted host Git from PATH")
}

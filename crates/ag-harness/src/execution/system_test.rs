//! System read set unit tests for host filtering and the selected Xcode.

use super::reads;

#[test]
fn system_reads_name_only_existing_absolute_system_directories() {
    // Arrange / Act
    let reads = reads();

    // Assert
    assert!(reads.iter().all(|path| path.is_absolute() && path.exists()));
    assert!(reads.iter().any(|path| path.ends_with("usr")), "{reads:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn selected_xcode_is_the_bundle_holding_the_selected_developer_directory() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let developer = root.join("Xcode.app/Contents/Developer");
    std::fs::create_dir_all(&developer).expect("developer directory");
    std::fs::create_dir_all(root.join("CommandLineTools")).expect("command line tools");
    std::os::unix::fs::symlink(&developer, root.join("xcode")).expect("Xcode link");
    std::os::unix::fs::symlink(root.join("CommandLineTools"), root.join("tools"))
        .expect("tools link");
    std::os::unix::fs::symlink("/", root.join("root")).expect("root link");

    // Act / Assert
    assert_eq!(
        super::selected_xcode(&root.join("xcode")),
        Some(root.join("Xcode.app"))
    );
    assert_eq!(super::selected_xcode(&root.join("tools")), None);
    assert_eq!(super::selected_xcode(&root.join("root")), None);
    assert_eq!(super::selected_xcode(&root.join("missing")), None);
}

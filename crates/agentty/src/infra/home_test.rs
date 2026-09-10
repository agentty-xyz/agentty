use std::path::PathBuf;

use super::resolve_agentty_home;

#[test]
fn resolve_agentty_home_returns_env_override_when_set() {
    // Arrange
    let agentty_root = Some(PathBuf::from("/tmp/custom-agentty"));
    let home_dir = Some(PathBuf::from("/home/test-user"));

    // Act
    let home = resolve_agentty_home(agentty_root, home_dir);

    // Assert
    assert_eq!(home, PathBuf::from("/tmp/custom-agentty"));
}

#[test]
fn resolve_agentty_home_falls_back_to_home_directory_when_override_is_empty() {
    // Arrange
    let agentty_root = Some(PathBuf::new());
    let home_dir = Some(PathBuf::from("/home/test-user"));

    // Act
    let home = resolve_agentty_home(agentty_root, home_dir);

    // Assert
    assert_eq!(home, PathBuf::from("/home/test-user/.agentty"));
}

#[test]
fn resolve_agentty_home_falls_back_to_relative_directory_without_home_dir() {
    // Arrange
    let agentty_root = None;
    let home_dir = None;

    // Act
    let home = resolve_agentty_home(agentty_root, home_dir);

    // Assert
    assert_eq!(home, PathBuf::from(".agentty"));
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{env, fs};

use tempfile::tempdir;

use crate::agent::availability::{available_agent_kinds_from_path, executable_name};
use crate::model::agent::AgentKind;

#[test]
/// Ensures executable names stay aligned with provider command names.
fn test_executable_name_matches_agent_cli_names() {
    // Arrange / Act / Assert
    assert_eq!(executable_name(AgentKind::Antigravity), "agy");
    assert_eq!(executable_name(AgentKind::Claude), "claude");
    assert_eq!(executable_name(AgentKind::Codex), "codex");
    assert_eq!(executable_name(AgentKind::Gemini), "gemini");
}

#[test]
/// Ensures the production probe reports only agent kinds whose
/// executables are present on the current `PATH`.
fn test_real_agent_availability_probe_filters_missing_executables() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(&codex_path, "").expect("failed to create codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(available_agent_kinds, vec![AgentKind::Codex]);
}

#[test]
/// Ensures probe discovery ignores non-executable files even when their
/// names match supported agent CLIs.
fn test_real_agent_availability_probe_ignores_non_executable_files() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(&codex_path, "").expect("failed to create codex file");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o640))
        .expect("failed to mark codex non-executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(
        available_agent_kinds,
        [] as [crate::model::agent::AgentKind; 0]
    );
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::{env, fs};

use ag_session::{AgentAvailabilityProbe, AgentCliInfo, AgentKind};
use tempfile::tempdir;

use crate::agent::availability::{
    RealAgentAvailabilityProbe, available_agent_kinds_from_path, executable_name,
};

#[tokio::test]
/// Ensures the production probe reads the process environment: an empty
/// `PATH` reports no CLIs and an unusable sign-in environment fails before
/// opening a browser. A child test process isolates the environment so
/// discovery never updates CLIs installed on the host.
async fn test_real_agent_availability_probe_reads_an_isolated_process_environment() {
    // Arrange
    const MARKER: &str = "AG_AGENT_EMPTY_PROVIDER_PATH";
    if env::var_os(MARKER).is_some() {
        // Act
        let available_agent_kinds = RealAgentAvailabilityProbe.available_agent_kinds();
        let available_agent_clis = RealAgentAvailabilityProbe.available_agent_clis();
        let harness_models = RealAgentAvailabilityProbe.native_harness_models().await;
        let opened_urls = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = Arc::clone(&opened_urls);
        let sign_in = RealAgentAvailabilityProbe
            .sign_in_chatgpt(Box::new(move |url| {
                recorder.lock().expect("opened lock").push(url);
            }))
            .await;

        // Assert
        assert_eq!(available_agent_kinds, [] as [AgentKind; 0]);
        assert_eq!(available_agent_clis, [] as [AgentCliInfo; 0]);
        assert!(harness_models.iter().all(|option| option.cost.is_paid()));
        assert!(sign_in.is_err_and(|error| !error.is_empty()));
        assert!(opened_urls.lock().expect("opened lock").is_empty());

        return;
    }
    let auth_directory = tempdir().expect("auth directory");
    let unreadable_auth_file = auth_directory.path().join("chatgpt.json");
    fs::write(&unreadable_auth_file, "not json").expect("unreadable sign-in");

    // Act
    let output = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "agent::availability::tests::discovery::\
             test_real_agent_availability_probe_reads_an_isolated_process_environment",
        ])
        .env("CHATGPT_AUTH_FILE", &unreadable_auth_file)
        .env(MARKER, "1")
        .env("PATH", "")
        .env("MODEL_API_KEY", "test-key")
        .env("OPENAI_COMPATIBLE_BASE_URL", "http://127.0.0.1:9/v1")
        .env_remove("OPENAI_COMPATIBLE_MODEL")
        .output()
        .expect("isolated discovery");

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
/// Ensures executable names stay aligned with provider command names and the
/// in-process harness has none.
fn test_executable_name_matches_agent_cli_names() {
    // Arrange / Act / Assert
    assert_eq!(executable_name(AgentKind::Antigravity), Some("agy"));
    assert_eq!(executable_name(AgentKind::Claude), Some("claude"));
    assert_eq!(executable_name(AgentKind::Codex), Some("codex"));
    assert_eq!(executable_name(AgentKind::Harness), None);
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
/// Ensures probe discovery ignores non-executable files and directories even
/// when their names match supported agent CLIs.
fn test_real_agent_availability_probe_ignores_non_executable_files() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(&codex_path, "").expect("failed to create codex file");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o640))
        .expect("failed to mark codex non-executable");
    fs::create_dir(temp_directory.path().join("claude")).expect("failed to create claude dir");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(available_agent_kinds, [] as [ag_session::AgentKind; 0]);
}

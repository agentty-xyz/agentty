use std::path::{Path, PathBuf};

use ag_git::{GitClient, RealGitClient};
use testty::feature::{GifMode, GifStatus};
use testty::frame::TerminalFrame;

use super::{
    BuilderEnv, FeatureTest, NO_COLOR_ENV_VALUE, NO_COLOR_ENV_VAR, PINNED_CLOCK_ENV_VAR,
    PINNED_CLOCK_UNIX_SECONDS, PINNED_CLOCK_UTC_OFFSET_ENV_VAR, PINNED_CLOCK_UTC_OFFSET_SECONDS,
    PINNED_DISPLAY_VERSION_ENV_VAR, SESSION_VIEW_FOOTER_MARKER, STUB_AGENT_EXECUTABLES,
    feature_gif_mode_for_artifacts, gif_exists_on_disk, match_session_view_texts, parse_gif_mode,
    tab_page_marker,
};

#[test]
fn tab_page_marker_maps_supported_tabs() {
    // Arrange
    let tab_names = ["Projects", "Sessions", "Settings"];

    // Act
    let markers = tab_names.map(tab_page_marker);

    // Assert
    assert_eq!(markers, ["Activity", "new session", "Default Smart Model"]);
}

#[test]
fn tab_page_marker_maps_unsupported_tab_to_missing_content() {
    // Arrange
    let tab_name = "Unknown";

    // Act
    let marker = tab_page_marker(tab_name);

    // Assert
    assert_eq!(marker, "<unsupported E2E tab>");
}

#[test]
fn match_session_view_texts_accepts_footer_and_all_markers() {
    // Arrange
    let frame = TerminalFrame::new(
        80,
        4,
        b"Campaign: Managed feature delivery\r\nremediation 1/3\r\n\r\nq: back",
    );
    let expected_texts = vec![
        "Campaign: Managed feature delivery".to_string(),
        "remediation 1/3".to_string(),
    ];

    // Act
    let result = match_session_view_texts(&frame, &expected_texts);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_session_view_texts_reports_missing_content_with_frame() {
    // Arrange
    let frame = TerminalFrame::new(
        80,
        4,
        b"Campaign: Managed feature delivery\r\nwaiting\r\n\r\nq: back",
    );
    let expected_texts = vec!["remediation 1/3".to_string()];

    // Act
    let failure = match_session_view_texts(&frame, &expected_texts)
        .expect_err("missing campaign content should fail");

    // Assert
    assert!(failure.message.contains("remediation 1/3"));
    assert!(
        failure
            .frame_excerpt
            .contains("Campaign: Managed feature delivery")
    );
}

#[test]
fn match_session_view_texts_requires_session_footer() {
    // Arrange
    let frame = TerminalFrame::new(
        80,
        4,
        b"Campaign: Managed feature delivery\r\nremediation 1/3",
    );
    let expected_texts = vec!["remediation 1/3".to_string()];

    // Act
    let failure = match_session_view_texts(&frame, &expected_texts)
        .expect_err("campaign text outside a session view should fail");

    // Assert
    assert!(failure.message.contains(SESSION_VIEW_FOOTER_MARKER));
}

#[test]
fn parse_gif_mode_recognizes_always_generate_aliases() {
    // Arrange / Act / Assert
    assert_eq!(parse_gif_mode("force"), Some(GifMode::AlwaysGenerate));
    assert_eq!(parse_gif_mode("always"), Some(GifMode::AlwaysGenerate));
    assert_eq!(
        parse_gif_mode("always-generate"),
        Some(GifMode::AlwaysGenerate),
    );
    assert_eq!(parse_gif_mode("Force"), Some(GifMode::AlwaysGenerate));
}

#[test]
fn builder_env_keeps_painted_paths_under_home() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");

    // Act
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

    // Assert
    assert_eq!(env.agentty_root, env.home_dir.join(".agentty"));
    assert!(env.workdir.starts_with(&env.home_dir));
}

#[test]
fn builder_env_preserves_existing_git_file() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let git_path = temp.path().join(".git");
    let git_contents = "gitdir: linked-worktree-metadata\n";
    std::fs::write(&git_path, git_contents).expect("failed to write worktree pointer");

    // Act
    let error = BuilderEnv::new(temp.path())
        .err()
        .expect("existing worktree root should be rejected");

    // Assert
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&git_path).expect("failed to read worktree pointer"),
        git_contents,
    );
    assert_eq!(
        temp.path()
            .read_dir()
            .expect("failed to read fixture root")
            .count(),
        1
    );
}

#[test]
fn builder_env_preserves_nonempty_directories() {
    for directory_name in [".git", "home"] {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
        let existing_dir = temp.path().join(directory_name);
        std::fs::create_dir(&existing_dir).expect("failed to create existing directory");
        let existing_file = existing_dir.join("keep.txt");
        std::fs::write(&existing_file, "preserve this data")
            .expect("failed to write existing data");

        // Act
        let error = BuilderEnv::new(temp.path())
            .err()
            .expect("nonempty fixture root should be rejected");

        // Assert
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&existing_file).expect("failed to read existing data"),
            "preserve this data",
        );
        assert_eq!(
            temp.path()
                .read_dir()
                .expect("failed to read fixture root")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn builder_env_isolates_parent_git_repositories() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let parent_git_dir = temp.path().join(".git");
    std::fs::create_dir(&parent_git_dir).expect("failed to create parent Git marker");
    std::fs::write(parent_git_dir.join("HEAD"), "ref: refs/heads/host-only\n")
        .expect("failed to seed parent branch");
    let fixture_root = temp.path().join("fixture");
    std::fs::create_dir(&fixture_root).expect("failed to create fixture root");
    let env = BuilderEnv::new(&fixture_root).expect("failed to create builder environment");
    env.init_git()
        .expect("failed to initialize fixture project");

    // Act
    let placeholder_root = RealGitClient
        .find_git_repo_root(env.agentty_root.clone())
        .await;
    let project_root = RealGitClient.find_git_repo_root(env.workdir.clone()).await;
    let placeholder_branch = RealGitClient.detect_git_info(env.agentty_root).await;
    let project_branch = RealGitClient.detect_git_info(env.workdir.clone()).await;

    // Assert
    assert_eq!(placeholder_root, None);
    assert_eq!(project_root, Some(env.workdir));
    assert_eq!(placeholder_branch, None);
    assert_eq!(project_branch.as_deref(), Some("main"));
}

#[tokio::test]
async fn builder_env_without_git_has_no_repository_root() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

    // Act
    let repository_root = RealGitClient.find_git_repo_root(env.workdir).await;

    // Assert
    assert_eq!(repository_root, None);
}

#[test]
fn builder_env_pins_vhs_terminal_environment() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

    // Act
    let environment = env.as_vhs_env_pairs();

    // Assert
    assert!(
        environment
            .iter()
            .any(|(key, value)| { key == NO_COLOR_ENV_VAR && value == NO_COLOR_ENV_VALUE }),
        "feature recording must disable color"
    );
    assert!(
        environment
            .iter()
            .any(|(key, value)| key == "TMUX" && value.is_empty()),
        "feature recording must not inherit host tmux shortcuts"
    );
}

#[test]
fn builder_env_vhs_launcher_uses_semantic_proof_workdir() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");
    let launcher = env
        .create_vhs_launcher(Path::new("/bin/pwd"))
        .expect("failed to create VHS launcher");

    // Act
    let output = std::process::Command::new(launcher)
        .output()
        .expect("failed to execute VHS launcher");

    // Assert
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        env.workdir.to_string_lossy(),
    );
}

#[test]
fn builder_env_stubs_every_supported_agent_cli() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");

    // Act
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

    // Assert
    for executable_name in STUB_AGENT_EXECUTABLES {
        assert!(
            env.stub_bin.join(executable_name).is_file(),
            "missing {executable_name} test stub",
        );
    }
}

#[test]
fn builder_env_keeps_antigravity_stub_supported_after_update() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
    let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");
    let antigravity_path = env.stub_bin.join("agy");

    // Act
    let initial_output = std::process::Command::new(&antigravity_path)
        .arg("--version")
        .output()
        .expect("failed to execute Antigravity test stub");
    let update_status = std::process::Command::new(&antigravity_path)
        .arg("update")
        .status()
        .expect("failed to update Antigravity test stub");
    let updated_output = std::process::Command::new(&antigravity_path)
        .arg("--version")
        .output()
        .expect("failed to execute updated Antigravity test stub");

    // Assert
    assert!(initial_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&initial_output.stdout),
        "agy 1.2.0\n"
    );
    assert!(update_status.success());
    assert!(updated_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&updated_output.stdout),
        "agy 1.2.1\n"
    );
}

#[test]
fn feature_test_pins_render_environment() {
    // Arrange
    let expected_environment = [
        (
            PINNED_CLOCK_ENV_VAR.to_string(),
            PINNED_CLOCK_UNIX_SECONDS.to_string(),
        ),
        (
            PINNED_CLOCK_UTC_OFFSET_ENV_VAR.to_string(),
            PINNED_CLOCK_UTC_OFFSET_SECONDS.to_string(),
        ),
        (PINNED_DISPLAY_VERSION_ENV_VAR.to_string(), "1".to_string()),
    ];

    // Act
    let feature_test = FeatureTest::new("deterministic_clock");

    // Assert
    for environment_entry in expected_environment {
        assert!(feature_test.child_env.contains(&environment_entry));
    }
}

#[test]
fn parse_gif_mode_recognizes_generate_if_stale_aliases() {
    // Arrange / Act / Assert
    assert_eq!(parse_gif_mode("generate"), Some(GifMode::GenerateIfStale));
    assert_eq!(
        parse_gif_mode("  generate-if-stale  "),
        Some(GifMode::GenerateIfStale),
    );
}

#[test]
fn parse_gif_mode_recognizes_check_only_aliases() {
    // Arrange / Act / Assert
    assert_eq!(parse_gif_mode("check"), Some(GifMode::CheckOnly));
    assert_eq!(parse_gif_mode("  check-only  "), Some(GifMode::CheckOnly));
    assert_eq!(parse_gif_mode("Check"), Some(GifMode::CheckOnly));
}

#[test]
fn parse_gif_mode_leaves_recording_off_for_unrecognized_values() {
    // Arrange / Act / Assert
    assert_eq!(parse_gif_mode(""), None);
    assert_eq!(parse_gif_mode("nonsense"), None);
}

#[test]
fn feature_gif_mode_for_run_keeps_zola_feature_mode() {
    // Arrange / Act / Assert
    assert_eq!(
        feature_gif_mode_for_artifacts(Some(GifMode::CheckOnly), true, true),
        Some(GifMode::CheckOnly),
    );
}

#[test]
fn feature_gif_mode_for_run_skips_regression_only_tests() {
    // Arrange / Act / Assert
    assert_eq!(
        feature_gif_mode_for_artifacts(Some(GifMode::CheckOnly), false, true),
        None,
    );
}

#[test]
fn feature_gif_mode_for_run_skips_unpublished_check_only_features() {
    // Arrange / Act / Assert
    assert_eq!(
        feature_gif_mode_for_artifacts(Some(GifMode::CheckOnly), true, false),
        None,
    );
}

#[test]
fn feature_gif_mode_for_run_keeps_generate_for_unpublished_zola_features() {
    // Arrange / Act / Assert
    assert_eq!(
        feature_gif_mode_for_artifacts(Some(GifMode::GenerateIfStale), true, false),
        Some(GifMode::GenerateIfStale),
    );
}

#[test]
fn gif_exists_on_disk_reports_recorded_gif() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("temp dir");
    let gif_path = temp.path().join("feature.gif");
    std::fs::write(&gif_path, b"gif").expect("write gif");

    // Act / Assert
    assert!(gif_exists_on_disk(&GifStatus::Generated(gif_path.clone())));
    assert!(gif_exists_on_disk(&GifStatus::CacheHit(gif_path)));
}

#[test]
fn gif_exists_on_disk_reports_skipped_and_missing_gifs() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("temp dir");
    let missing_gif_path = temp.path().join("absent.gif");

    // Act / Assert
    assert!(!gif_exists_on_disk(&GifStatus::Generated(missing_gif_path)));
    assert!(!gif_exists_on_disk(&GifStatus::VhsNotInstalled));
    assert!(!gif_exists_on_disk(&GifStatus::NoOutputDir));
}

#[test]
fn validate_gif_status_accepts_fresh_check_result() {
    // Arrange
    let feature_test = FeatureTest::new("fresh_feature");
    let gif_status = GifStatus::Fresh {
        gif_path: PathBuf::from("docs/site/static/features/fresh_feature.gif"),
        hash: 42,
    };

    // Act
    let result = feature_test.validate_gif_status(&gif_status);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn validate_gif_status_rejects_stale_check_result() {
    // Arrange
    let feature_test = FeatureTest::new("stale_feature");
    let gif_status = GifStatus::Stale {
        gif_path: PathBuf::from("docs/site/static/features/stale_feature.gif"),
        current: 42,
        committed: Some(7),
        committed_error: None,
    };

    // Act
    let result = feature_test.validate_gif_status(&gif_status);

    // Assert
    let error = result.expect_err("stale GIF status should fail validation");
    let message = error.to_string();

    assert!(message.contains("Feature GIF is stale for stale_feature"));
    assert!(message.contains("current hash 42"));
    assert!(message.contains("committed hash Some(7)"));
}

#[test]
fn validate_gif_status_accepts_existing_gif_without_sidecar() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("temp dir");
    let gif_path = temp.path().join("legacy_feature.gif");
    std::fs::write(&gif_path, b"gif").expect("write gif");

    let feature_test = FeatureTest::new("legacy_feature");
    let gif_status = GifStatus::Stale {
        gif_path,
        current: 42,
        committed: None,
        committed_error: None,
    };

    // Act
    let result = feature_test.validate_gif_status(&gif_status);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn validate_gif_status_rejects_missing_gif_without_sidecar() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("temp dir");
    let feature_test = FeatureTest::new("missing_feature");
    let gif_status = GifStatus::Stale {
        gif_path: temp.path().join("missing_feature.gif"),
        current: 42,
        committed: None,
        committed_error: None,
    };

    // Act
    let result = feature_test.validate_gif_status(&gif_status);

    // Assert
    let error = result.expect_err("missing GIF should fail validation");
    let message = error.to_string();

    assert!(message.contains("Feature GIF is stale for missing_feature"));
    assert!(message.contains("committed hash None"));
}

#[test]
fn validate_gif_status_rejects_invalid_sidecar_for_existing_gif() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("temp dir");
    let gif_path = temp.path().join("invalid_sidecar.gif");
    std::fs::write(&gif_path, b"gif").expect("write gif");

    let feature_test = FeatureTest::new("invalid_sidecar");
    let gif_status = GifStatus::Stale {
        gif_path,
        current: 42,
        committed: None,
        committed_error: Some("failed to parse hash sidecar as u64".to_string()),
    };

    // Act
    let result = feature_test.validate_gif_status(&gif_status);

    // Assert
    let error = result.expect_err("invalid sidecar should fail validation");
    let message = error.to_string();

    assert!(message.contains("Feature GIF is stale for invalid_sidecar"));
    assert!(message.contains("committed hash None"));
    assert!(message.contains("committed sidecar error"));
}

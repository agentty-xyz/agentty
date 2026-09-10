use super::*;

#[test]
/// Ensures unsupported Antigravity installations are not selectable even
/// when the executable is present.
fn test_available_agent_kinds_from_path_filters_old_antigravity() {
    // Arrange
    let _cache_guard = antigravity_cache_test_guard();
    let temp_directory = tempdir().expect("failed to create temp dir");
    let antigravity_path = temp_directory.path().join("agy");
    let codex_path = temp_directory.path().join("codex");
    fs::write(
        &antigravity_path,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'agy 1.1.17\\n'; fi\n",
    )
    .expect("failed to create agy executable");
    fs::write(&codex_path, "").expect("failed to create codex executable");
    fs::set_permissions(&antigravity_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark agy executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_kinds = available_agent_kinds_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(available_agent_kinds, vec![AgentKind::Codex]);
}

#[test]
/// Ensures refreshed Antigravity compatibility is reused without another
/// version process and invalidated when the executable changes.
fn test_cached_antigravity_support_tracks_refreshed_executable() {
    // Arrange
    let _cache_guard = antigravity_cache_test_guard();
    let temp_directory = tempdir().expect("failed to create temp dir");
    let antigravity_path = temp_directory.path().join("agy");
    fs::write(
        &antigravity_path,
        "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 0; fi\nif [ \"$1\" = \"--version\" ]; \
         then printf 'agy 1.2.0\\n'; exit 0; fi\nexit 1\n",
    )
    .expect("failed to create agy executable");
    fs::set_permissions(&antigravity_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark agy executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let detected_version = refresh_agent_cli_version(
        AgentKind::Antigravity,
        &antigravity_path,
        Some(path_value.as_os_str()),
    );
    let cached_result =
        ensure_cached_antigravity_cli_supported_on_path(Some(path_value.as_os_str()));
    fs::write(
        &antigravity_path,
        "#!/bin/sh\nprintf 'changed Antigravity executable\\n'\n",
    )
    .expect("failed to replace agy executable");
    let changed_result =
        ensure_cached_antigravity_cli_supported_on_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(detected_version, Some("1.2.0".to_string()));
    assert_eq!(cached_result, Ok(()));
    let changed_error =
        changed_result.expect_err("changed Antigravity executable should fail closed");
    assert!(changed_error.contains("installation changed"));
    assert!(changed_error.contains("restart Agentty"));
}

#[test]
/// Ensures supported stable and prefixed Antigravity versions pass the
/// compatibility check.
fn test_validate_antigravity_cli_version_accepts_supported_versions() {
    // Arrange / Act / Assert
    assert_eq!(validate_antigravity_cli_version(Some("1.1.18")), Ok(()));
    assert_eq!(validate_antigravity_cli_version(Some("v1.2.0")), Ok(()));
}

#[test]
/// Ensures old Antigravity versions return an actionable upgrade error.
fn test_validate_antigravity_cli_version_rejects_old_version() {
    // Arrange / Act
    let error = validate_antigravity_cli_version(Some("1.1.17"))
        .expect_err("old Antigravity should be rejected");

    // Assert
    assert_eq!(
        error,
        "Antigravity CLI 1.1.18 or newer is required, but `1.1.17` is installed. Run `agy \
         update`, then retry."
    );
}

#[test]
/// Ensures missing and malformed version output both explain how to
/// recover.
fn test_validate_antigravity_cli_version_rejects_unknown_versions() {
    // Arrange / Act
    let missing_error = validate_antigravity_cli_version(None)
        .expect_err("missing Antigravity version should be rejected");
    let malformed_error = validate_antigravity_cli_version(Some("development"))
        .expect_err("malformed Antigravity version should be rejected");

    // Assert
    assert!(missing_error.contains("did not report a version"));
    assert!(missing_error.contains("Run `agy update`"));
    assert!(malformed_error.contains("reported `development`"));
    assert!(malformed_error.contains("Run `agy update`"));
}

#[test]
/// Ensures turn-time validation reuses only a result for the exact
/// executable fingerprint that was previously probed.
fn test_validate_cached_antigravity_cli_support_requires_matching_fingerprint() {
    // Arrange
    let fingerprint = AntigravityExecutableFingerprint {
        device: 1,
        inode: 2,
        length: 3,
        modified_nanoseconds: 4,
        modified_seconds: 5,
        mode: 0o100_755,
        path: PathBuf::from("/test/agy"),
    };
    let changed_fingerprint = AntigravityExecutableFingerprint {
        length: 30,
        ..fingerprint.clone()
    };
    let supported_snapshot = AntigravityCompatibilitySnapshot {
        fingerprint: Some(fingerprint.clone()),
        result: Ok(()),
    };
    let unsupported_snapshot = AntigravityCompatibilitySnapshot {
        fingerprint: Some(fingerprint.clone()),
        result: Err("Run `agy update`, then retry.".to_string()),
    };

    // Act
    let supported_result =
        validate_cached_antigravity_cli_support(Some(&supported_snapshot), Some(&fingerprint));
    let unsupported_result =
        validate_cached_antigravity_cli_support(Some(&unsupported_snapshot), Some(&fingerprint));
    let missing_snapshot_error = validate_cached_antigravity_cli_support(None, Some(&fingerprint))
        .expect_err("a missing snapshot should fail closed");
    let changed_executable_error = validate_cached_antigravity_cli_support(
        Some(&supported_snapshot),
        Some(&changed_fingerprint),
    )
    .expect_err("a changed executable should invalidate the snapshot");

    // Assert
    assert_eq!(supported_result, Ok(()));
    assert_eq!(
        unsupported_result,
        Err("Run `agy update`, then retry.".to_string())
    );
    assert!(missing_snapshot_error.contains("has not been validated yet"));
    assert!(changed_executable_error.contains("installation changed"));
    assert!(changed_executable_error.contains("restart Agentty"));
}

#[test]
/// Ensures a missing Antigravity executable returns an actionable
/// installation error.
fn test_ensure_antigravity_cli_supported_on_path_rejects_missing_executable() {
    // Arrange
    let _cache_guard = antigravity_cache_test_guard();
    let temp_directory = tempdir().expect("failed to create temp dir");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let error = ensure_antigravity_cli_supported_on_path(Some(path_value.as_os_str()))
        .expect_err("missing Antigravity should be rejected");
    let cached_error =
        ensure_cached_antigravity_cli_supported_on_path(Some(path_value.as_os_str()))
            .expect_err("cached missing Antigravity should remain rejected");

    // Assert
    assert!(error.contains("`agy` was not found on `PATH`"));
    assert!(error.contains("Install it or run `agy update`"));
    assert_eq!(cached_error, error);
}

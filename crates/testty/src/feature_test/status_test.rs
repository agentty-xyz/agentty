use std::path::{Path, PathBuf};

use super::super::vhs_missing_status;
use crate::feature::{GifMode, GifStatus};
use crate::vhs::VhsError;

#[test]
fn vhs_missing_status_always_generate_is_hard_failure() {
    // Arrange
    let err = VhsError::NotInstalled("missing".to_string());

    // Act
    let status = vhs_missing_status(GifMode::AlwaysGenerate, err);

    // Assert
    assert!(
        matches!(status, GifStatus::TapeExecutionFailed(_)),
        "AlwaysGenerate must surface missing VHS as a hard failure, got {status:?}",
    );
    assert!(status.is_failure());
}

#[test]
fn vhs_missing_status_generate_if_stale_is_benign_skip() {
    // Arrange
    let err = VhsError::NotInstalled("missing".to_string());

    // Act
    let status = vhs_missing_status(GifMode::GenerateIfStale, err);

    // Assert
    assert!(matches!(status, GifStatus::VhsNotInstalled));
    assert!(!status.is_failure());
}

#[test]
fn vhs_missing_status_check_only_is_benign_skip() {
    // Arrange — `CheckOnly` short-circuits before the VHS probe in
    // `generate_gif`, but the helper must still treat it as benign so
    // future refactors that route through it do not regress to a hard
    // failure.
    let err = VhsError::NotInstalled("missing".to_string());

    // Act
    let status = vhs_missing_status(GifMode::CheckOnly, err);

    // Assert
    assert!(matches!(status, GifStatus::VhsNotInstalled));
    assert!(!status.is_failure());
}

#[test]
fn gif_status_generated_returns_path() {
    // Arrange
    let status = GifStatus::Generated(PathBuf::from("/tmp/test.gif"));

    // Act / Assert
    assert_eq!(status.gif_path(), Some(Path::new("/tmp/test.gif")));
    assert!(!status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_cache_hit_returns_path() {
    // Arrange
    let status = GifStatus::CacheHit(PathBuf::from("/tmp/cached.gif"));

    // Act / Assert
    assert_eq!(status.gif_path(), Some(Path::new("/tmp/cached.gif")));
    assert!(!status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_vhs_not_installed_is_not_failure() {
    // Arrange
    let status = GifStatus::VhsNotInstalled;

    // Act / Assert
    assert!(status.gif_path().is_none());
    assert!(!status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_no_output_dir_is_not_failure() {
    // Arrange
    let status = GifStatus::NoOutputDir;

    // Act / Assert
    assert!(status.gif_path().is_none());
    assert!(!status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_dir_create_failed_is_failure() {
    // Arrange
    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let status = GifStatus::DirCreateFailed(err);

    // Act / Assert
    assert!(status.gif_path().is_none());
    assert!(status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_tape_execution_failed_is_failure() {
    // Arrange
    let err = VhsError::ExecutionFailed("vhs crashed".to_string());
    let status = GifStatus::TapeExecutionFailed(err);

    // Act / Assert
    assert!(status.gif_path().is_none());
    assert!(status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_fresh_exposes_path_and_is_not_stale() {
    // Arrange
    let status = GifStatus::Fresh {
        gif_path: PathBuf::from("/tmp/feature.gif"),
        hash: 42,
    };

    // Act / Assert
    assert_eq!(status.gif_path(), Some(Path::new("/tmp/feature.gif")));
    assert!(!status.is_failure());
    assert!(!status.is_stale());
}

#[test]
fn gif_status_stale_exposes_path_and_is_stale() {
    // Arrange
    let status = GifStatus::Stale {
        gif_path: PathBuf::from("/tmp/feature.gif"),
        current: 42,
        committed: Some(7),
        committed_error: None,
    };

    // Act / Assert
    assert_eq!(status.gif_path(), Some(Path::new("/tmp/feature.gif")));
    assert!(!status.is_failure());
    assert!(status.is_stale());
}

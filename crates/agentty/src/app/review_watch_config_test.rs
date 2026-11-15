use std::io;
use std::path::Path;

use tempfile::tempdir;

use crate::app::review_watch_config::ReviewWatchConfig;
use crate::infra::fs::{FsError, MockFsClient, RealFsClient};

/// Loads the configuration from a mock file containing `bytes`.
async fn load(bytes: &[u8]) -> Result<ReviewWatchConfig, String> {
    let mut fs = MockFsClient::new();
    let bytes = bytes.to_vec();
    fs.expect_read_file_prefix()
        .once()
        .returning(move |path, max_bytes| {
            assert_eq!(path, Path::new("project/.agentty/review-watch.json"));
            assert_eq!(max_bytes, 16_385);
            let bytes = bytes.clone();
            Box::pin(async move { Ok(bytes) })
        });

    ReviewWatchConfig::load(&fs, Path::new("project")).await
}

#[tokio::test]
async fn real_loader_reads_trusted_bots_and_defaults_when_the_file_is_missing() {
    // Arrange
    let project = tempdir().expect("project directory");

    // Act
    let missing = ReviewWatchConfig::load(&RealFsClient, project.path()).await;
    std::fs::create_dir(project.path().join(".agentty")).expect("configuration directory");
    std::fs::write(
        project.path().join(".agentty/review-watch.json"),
        br#"{"trusted_bots":["greptile-apps","coderabbitai"]}"#,
    )
    .expect("configuration file");
    let present = ReviewWatchConfig::load(&RealFsClient, project.path()).await;

    // Assert
    assert_eq!(missing, Ok(ReviewWatchConfig::default()));
    assert_eq!(
        present,
        Ok(ReviewWatchConfig {
            trusted_bots: vec!["greptile-apps".to_string(), "coderabbitai".to_string()],
        })
    );
}

#[tokio::test]
async fn loader_accepts_an_empty_object_and_rejects_invalid_configuration() {
    // Arrange
    let oversized = vec![b' '; 16_385];
    let cases: [(&[u8], Result<ReviewWatchConfig, &str>); 5] = [
        (b"{}", Ok(ReviewWatchConfig::default())),
        (
            br#"{"trusted_bots":[""]}"#,
            Err(
                "`.agentty/review-watch.json` needs nonempty `trusted_bots` logins without \
                 whitespace",
            ),
        ),
        (
            br#"{"trusted_bots":["review bot"]}"#,
            Err(
                "`.agentty/review-watch.json` needs nonempty `trusted_bots` logins without \
                 whitespace",
            ),
        ),
        (
            &oversized,
            Err("`.agentty/review-watch.json` exceeds 16384 bytes"),
        ),
        (
            br#"{"trusted_bot":["greptile-apps"]}"#,
            Err("Invalid `.agentty/review-watch.json`: unknown field `trusted_bot`"),
        ),
    ];

    for (bytes, expected) in cases {
        // Act
        let result = load(bytes).await;

        // Assert
        match expected {
            Ok(config) => assert_eq!(result, Ok(config)),
            Err(message) => {
                let error = result.expect_err("invalid configuration should fail");
                assert!(error.starts_with(message), "{error}");
            }
        }
    }
}

#[tokio::test]
async fn loader_reports_read_failures() {
    // Arrange
    let mut fs = MockFsClient::new();
    fs.expect_read_file_prefix().once().returning(|_, _| {
        Box::pin(async {
            Err(FsError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "denied",
            )))
        })
    });

    // Act
    let error = ReviewWatchConfig::load(&fs, Path::new("project")).await;

    // Assert
    assert_eq!(
        error,
        Err("Cannot load `.agentty/review-watch.json`: denied".to_string())
    );
}

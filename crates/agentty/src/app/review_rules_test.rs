use std::fmt::Write as _;
use std::io;
use std::path::Path;

use tempfile::tempdir;

use crate::app::review_rules::{MAX_CRITERIA_BYTES, ReviewRules};
use crate::infra::fs::{FsError, MockFsClient, RealFsClient};

async fn load(bytes: &[u8]) -> Result<ReviewRules, ag_contracts::OneShotError> {
    let mut fs = MockFsClient::new();
    let bytes = bytes.to_vec();
    fs.expect_read_file().never();
    fs.expect_read_file_prefix()
        .once()
        .returning(move |path, max_bytes| {
            assert_eq!(path, Path::new("project/.agentty/review-rules.json"));
            assert_eq!(max_bytes, 65_537);
            let bytes = bytes.clone();
            Box::pin(async move { Ok(bytes) })
        });

    ReviewRules::load(&fs, Path::new("project")).await
}

#[tokio::test]
async fn real_loader_accepts_exact_size_limit_and_rejects_larger_files() {
    // Arrange
    let project = tempdir().expect("project directory");
    std::fs::create_dir(project.path().join(".agentty")).expect("configuration directory");
    let path = project.path().join(".agentty/review-rules.json");
    let mut bytes = br#"{"rules":[]}"#.to_vec();
    bytes.resize(65_536, b' ');
    std::fs::write(&path, &bytes).expect("exact limit configuration");

    // Act / Assert
    assert!(
        ReviewRules::load(&RealFsClient, project.path())
            .await
            .is_ok()
    );
    bytes.push(b' ');
    std::fs::write(&path, &bytes).expect("oversized configuration");
    for file_size in [65_537, 1_048_576] {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("configuration file")
            .set_len(file_size)
            .expect("configuration size");
        let error = ReviewRules::load(&RealFsClient, project.path())
            .await
            .err()
            .expect("size error");
        assert_eq!(error.to_string(), "Project review rules exceed 65536 bytes");
    }
}

#[tokio::test]
async fn selects_project_criteria_by_path_component_and_extension() {
    // Arrange
    let rules = load(br#"{"rules":[{"path_prefix":"src/api/","extensions":["rs"],"instructions":"Check request authorization"},{"instructions":"Check public contracts"}]}"#).await.expect("valid rules");

    // Act
    let matching = rules
        .for_diff("diff --git a/src/api/handler.rs b/src/api/handler.rs\n")
        .expect("criteria");
    let outside = rules
        .for_diff("diff --git a/src/api_extra/handler.rs b/src/api_extra/handler.rs\n")
        .expect("criteria");
    let wrong_extension = rules
        .for_diff("diff --git a/src/api/handler.py b/src/api/handler.py\n")
        .expect("criteria");

    // Assert
    assert!(matching.contains("request authorization"));
    assert!(!outside.contains("request authorization"));
    assert!(!wrong_extension.contains("request authorization"));
    assert!(matching.contains("public contracts"));
    let renamed = rules
        .for_diff(
            "diff --git a/src/api/old.rs b/elsewhere/new.rs\nrename from src/api/old.rs\nrename \
             to elsewhere/new.rs\n",
        )
        .expect("criteria");
    assert!(renamed.contains("request authorization"));
    let exact =
        load(br#"{"rules":[{"path_prefix":"src/main.rs","instructions":"Check shutdown"}]}"#)
            .await
            .expect("exact path");
    assert!(
        exact
            .for_diff("diff --git a/src/main.rs b/src/main.rs\n")
            .expect("criteria")
            .contains("shutdown")
    );
}

#[test]
fn selects_language_and_test_rules_without_excluding_changed_files() {
    // Arrange
    let rules = ReviewRules::default();
    let cases = [
        ("rs", "Rust:"),
        ("sql", "SQL:"),
        ("ts", "JavaScript/TypeScript:"),
        ("py", "Python:"),
        ("go", "Go:"),
        ("json", "Configuration:"),
        ("unknown", "changed contracts"),
    ];

    // Act / Assert
    for (extension, expected) in cases {
        let diff = format!("diff --git a/x.{extension} b/x.{extension}\n");
        assert!(rules.for_diff(&diff).expect("criteria").contains(expected));
    }
    let tests = rules
        .for_diff("diff --git a/tests/a.rs b/tests/a.rs\ndiff --git a/tests/b.rs b/tests/b.rs\n")
        .expect("criteria");
    assert_eq!(tests.matches("Rust:").count(), 1);
    assert_eq!(tests.matches("Tests:").count(), 1);
    for path in ["crates/a/tests/check.rs", "src/a_test.rs", "src/a.test.ts"] {
        assert!(
            rules
                .for_diff(&format!("diff --git a/{path} b/{path}\n"))
                .expect("criteria")
                .contains("Tests:")
        );
    }
    assert_eq!(rules.for_diff("no file paths").expect("criteria"), "[]");
    assert!(
        rules
            .for_diff("diff --git a/tests/old.rs b/src/new.rs\n")
            .expect("criteria")
            .contains("Tests:")
    );
}

#[tokio::test]
async fn missing_configuration_uses_defaults_and_other_read_failures_surface() {
    // Arrange / Act / Assert
    for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
        let mut fs = MockFsClient::new();
        fs.expect_read_file_prefix().returning(move |_, _| {
            Box::pin(async move { Err(FsError::Io(io::Error::from(kind))) })
        });
        let result = ReviewRules::load(&fs, Path::new("project")).await;
        if kind == io::ErrorKind::NotFound {
            assert!(result.is_ok());
        } else {
            assert!(
                result
                    .err()
                    .expect("read error")
                    .to_string()
                    .contains("Cannot load review rules")
            );
        }
    }
}

#[tokio::test]
async fn rejects_malformed_oversized_or_unsafe_rule_configuration() {
    // Arrange
    let cases = [
        "invalid json",
        "{\"rules\":[],\"unexpected\":true}",
        r#"{"rules":[{"instructions":" "}]}"#,
        r#"{"rules":[{"instructions":"Check","path_prefix":"/outside"}]}"#,
        r#"{"rules":[{"instructions":"Check","path_prefix":"a\\b"}]}"#,
        r#"{"rules":[{"instructions":"Check","path_prefix":"a/../b"}]}"#,
        r#"{"rules":[{"instructions":"Check","path_prefix":"./a"}]}"#,
        r#"{"rules":[{"instructions":"Check","extensions":[""]}]}"#,
        r#"{"rules":[{"instructions":"Check","extensions":[".rs"]}]}"#,
        r#"{"rules":[{"instructions":"Check","extensions":["a/b"]}]}"#,
    ];

    // Act / Assert
    for input in cases {
        assert!(load(input.as_bytes()).await.is_err(), "{input}");
    }
    assert!(
        load(&vec![b' '; 65_537])
            .await
            .err()
            .expect("size limit")
            .to_string()
            .contains("65536")
    );
}

#[tokio::test]
async fn matching_rules_are_rendered_once_with_their_filters() {
    // Arrange
    let instruction = "Check authorization. ".repeat(100);
    let rule =
        serde_json::json!({"path_prefix":"src/", "extensions":["rs"], "instructions":instruction});
    let config = serde_json::json!({"rules":[rule.clone(), rule]});
    let rules = load(config.to_string().as_bytes()).await.expect("rules");
    let mut diff = String::new();
    for index in 0..200 {
        writeln!(
            diff,
            "diff --git a/src/file {index}.rs b/src/file {index}.rs\nold mode 100644\nnew mode \
             100755"
        )
        .expect("diff text");
    }

    // Act
    let rendered = rules.for_diff(&diff).expect("bounded criteria");
    let criteria: Vec<String> = serde_json::from_str(&rendered).expect("JSON criteria");

    // Assert
    assert_eq!(criteria.len(), 2);
    assert_eq!(
        criteria
            .iter()
            .filter(|entry| entry.contains(&instruction))
            .count(),
        1
    );
    assert!(criteria[1].contains("path_prefix: \"src/\"; extensions: [\"rs\"]"));
    assert!(rendered.len() <= MAX_CRITERIA_BYTES);
}

#[tokio::test]
async fn selected_criteria_budget_uses_rendered_bytes_and_preserves_nonmatching_rules() {
    // Arrange
    let diff = "diff --git a/src/main.rs b/src/main.rs\n";
    let config = |instructions: String| {
        serde_json::json!({"rules":[{"extensions":["rs"], "instructions":instructions}]})
            .to_string()
    };
    let base = load(config("a".into()).as_bytes())
        .await
        .expect("rules")
        .for_diff(diff)
        .expect("criteria");
    let available = MAX_CRITERIA_BYTES - base.len() + 1;
    let exact = load(config("a".repeat(available)).as_bytes())
        .await
        .expect("rules");
    let oversized = load(config("a".repeat(available + 1)).as_bytes())
        .await
        .expect("valid file size");
    let escaped = load(config("\u{0001}".repeat(1500)).as_bytes())
        .await
        .expect("valid escaped instructions");
    let huge = load(config("a".repeat(62_000)).as_bytes())
        .await
        .expect("below file size limit");

    // Act / Assert
    assert_eq!(
        exact.for_diff(diff).expect("exact budget").len(),
        MAX_CRITERIA_BYTES
    );
    for rules in [&oversized, &escaped, &huge] {
        assert!(
            rules
                .for_diff(diff)
                .expect_err("criteria budget")
                .to_string()
                .contains("Selected review criteria exceed 8000 bytes")
        );
        assert!(
            rules
                .for_diff("diff --git a/x.py b/x.py\n")
                .expect("nonmatching criteria")
                .contains("Python:")
        );
    }
}

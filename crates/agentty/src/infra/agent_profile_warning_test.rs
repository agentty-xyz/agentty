use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Mutex;

use super::write_definition;
use crate::domain::agent_profile::AgentDefinitionParseError;
use crate::infra::agent_profile::{
    canonical_catalog_paths, contain_agent_profile_definition, is_agent_profile_definition,
    is_agent_profile_directory, list_catalog_agent_profile_summaries, next_agent_directory,
    parse_agent_profile_value, read_agent_profile_source,
};

/// Captures diagnostics before a tracing subscriber is initialized.
struct CatalogWarningLogger(Mutex<Vec<String>>);

impl tracing::log::Log for CatalogWarningLogger {
    fn enabled(&self, metadata: &tracing::log::Metadata<'_>) -> bool {
        metadata.level() == tracing::log::Level::Warn
            && metadata.target().ends_with("infra::agent_profile")
    }

    fn log(&self, record: &tracing::log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0
                .lock()
                .expect("catalog warnings")
                .push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

#[tokio::test]
async fn catalog_warnings_include_rejected_definition_identity_and_cause() {
    // Arrange: isolate the process-global log fallback from other subscribers.
    static LOGGER: CatalogWarningLogger = CatalogWarningLogger(Mutex::new(Vec::new()));
    if let Some(status) = isolated_log_test().await {
        // Assert
        assert!(status.success());
        return;
    }
    tracing::log::set_logger(&LOGGER).expect("isolated logger");
    tracing::log::set_max_level(tracing::log::LevelFilter::Warn);
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("fixture");

    // Act
    let definition = exercise_catalog_rejections(directory.path()).await;

    // Assert: startup diagnostics explain every rejected catalog boundary.
    let warnings = LOGGER.0.lock().expect("captured warnings");
    for message in [
        "failed to resolve session worktree for agent_profile discovery",
        "ignored agent_profile catalog outside the session worktree",
        "failed to resolve workspace agent_profile catalog",
        "failed to read workspace agent_profile catalog",
        "failed while enumerating workspace agent_profile catalog",
        "failed to inspect workspace agent_profile directory",
        "failed to inspect workspace agent_profile definition",
        "ignored agent_profile definition outside the session worktree",
        "failed to resolve workspace agent_profile definition",
        "failed to read workspace agent_profile definition",
        "ignored malformed workspace agent_profile definition",
        "ignored duplicate workspace agent_profile id",
    ] {
        assert!(
            warnings.iter().any(|warning| warning.contains(message)),
            "missing {message}: {warnings:?}"
        );
    }
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("definition read failed")
                && warning.contains(&definition.display().to_string()))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("agent_profile_id") && warning.contains("reviewer"))
    );
}

/// Exercises rejected catalog boundaries before tracing initialization.
async fn exercise_catalog_rejections(directory: &Path) -> PathBuf {
    let workspace = directory.join("worktree");
    let outside = directory.join("outside");
    let agents_directory = workspace.join(".agents/agents");
    let definition = agents_directory.join("reviewer/agent.md");
    tokio::fs::create_dir_all(workspace.join(".agents"))
        .await
        .expect("workspace");
    tokio::fs::create_dir(&outside)
        .await
        .expect("outside catalog");

    // Act: filesystem failures and containment violations retain their paths.
    let _ = canonical_catalog_paths(&directory.join("missing")).await;
    symlink(&outside, &agents_directory).expect("outside catalog link");
    let _ = canonical_catalog_paths(&workspace).await;
    tokio::fs::remove_file(&agents_directory)
        .await
        .expect("remove link");
    symlink("agents", &agents_directory).expect("catalog loop");
    let _ = canonical_catalog_paths(&workspace).await;
    tokio::fs::remove_file(&agents_directory)
        .await
        .expect("remove loop");
    list_catalog_agent_profile_summaries(
        Err(io::Error::other("catalog open failed")),
        &workspace,
        &agents_directory,
    )
    .await;
    let _ = next_agent_directory(
        Err(io::Error::other("enumeration failed")),
        &agents_directory,
    );
    is_agent_profile_directory(
        Err(io::Error::other("directory metadata failed")),
        &agents_directory,
    );
    is_agent_profile_definition(
        Err(io::Error::other("definition metadata failed")),
        &definition,
    );
    let _ = contain_agent_profile_definition(Ok(outside), &workspace, &definition);
    let _ = contain_agent_profile_definition(
        Err(io::Error::other("canonicalize failed")),
        &workspace,
        &definition,
    );
    let _ = read_agent_profile_source(Err(io::Error::other("definition read failed")), &definition);
    let _ = parse_agent_profile_value::<()>(
        Err(AgentDefinitionParseError::MissingFrontmatter),
        &definition,
    );
    for name in ["first", "second"] {
        write_definition(
            &workspace,
            name,
            "---\nid: reviewer\nname: Reviewer\ndescription: Review code\n---\nReview.",
        )
        .await;
    }
    list_catalog_agent_profile_summaries(
        tokio::fs::read_dir(&agents_directory).await,
        &workspace,
        &agents_directory,
    )
    .await;

    definition
}

/// Runs this diagnostic test without inheriting process-global subscribers.
async fn isolated_log_test() -> Option<ExitStatus> {
    const CHILD_ENV: &str = "AGENTTY_TEST_AGENT_CATALOG_LOG_FALLBACK";
    if std::env::var_os(CHILD_ENV).is_some() {
        return None;
    }
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "infra::agent_profile::tests::warnings::catalog_warnings_include_rejected_definition_identity_and_cause",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1");
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env(
            "LLVM_PROFILE_FILE",
            profile
                .to_string_lossy()
                .replace(".profraw", "-agent-catalog-log-child.profraw"),
        );
    }

    Some(child.status().await.expect("isolated catalog logging test"))
}

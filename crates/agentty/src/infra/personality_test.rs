use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tracing::instrument::WithSubscriber;

use super::{
    RealPersonalityCatalogClient, SUMMARY_BODY_BUFFER_BYTES, body_contains_non_whitespace,
    contain_personality_definition, direct_agent_directory, is_personality_definition,
    is_personality_directory, list_catalog_personality_summaries, next_agent_directory,
    read_personality_summary_source,
};
use crate::domain::personality::PersonalitySummary;
use crate::infra::personality::PersonalityCatalogClient;

/// Writes one agent definition below the test workspace.
async fn write_definition(workspace: &Path, directory: &str, definition: &str) {
    let agent_directory = workspace.join(".agents").join("agents").join(directory);
    tokio::fs::create_dir_all(&agent_directory)
        .await
        .expect("create agent directory");
    tokio::fs::write(agent_directory.join("agent.md"), definition)
        .await
        .expect("write agent definition");
}

#[tokio::test]
async fn test_real_catalog_lists_enabled_workspace_personalities_in_name_order() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    write_definition(
        workspace.path(),
        "reviewer",
        "---\nid: reviewer\nname: Reviewer\ndescription: Reviews code\n---\nReview.",
    )
    .await;
    write_definition(
        workspace.path(),
        "architect",
        "---\nid: architect\nname: Architect\ndescription: Designs systems\n---\nDesign.",
    )
    .await;
    write_definition(
        workspace.path(),
        "disabled",
        "---\nname: Disabled\ndescription: Hidden\nenabled: false\n---\nHide.",
    )
    .await;
    write_definition(
        workspace.path(),
        "duplicate",
        "---\nid: reviewer\nname: Duplicate\ndescription: Duplicate id\n---\nDuplicate.",
    )
    .await;
    write_definition(
        workspace.path(),
        "alpha",
        "---\nid: alpha\nname: Same\ndescription: Same name\n---\nAlpha.",
    )
    .await;
    write_definition(
        workspace.path(),
        "beta",
        "---\nid: beta\nname: same\ndescription: Same lowercase name\n---\nBeta.",
    )
    .await;
    let catalog = RealPersonalityCatalogClient;

    // Act
    let personalities = catalog
        .list_summaries(workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert_eq!(
        personalities
            .iter()
            .map(|personality| personality.id.as_str())
            .collect::<Vec<_>>(),
        ["architect", "reviewer", "alpha", "beta"]
    );
}

#[test]
fn test_direct_agent_directory_accepts_only_one_normal_component() {
    // Arrange
    let agents_directory = Path::new("/workspace/.agents/agents");

    // Act
    let direct = direct_agent_directory(agents_directory, "reviewer");
    let parent = direct_agent_directory(agents_directory, "../reviewer");
    let nested = direct_agent_directory(agents_directory, "team/reviewer");

    // Assert
    assert_eq!(direct, Some(agents_directory.join("reviewer")));
    assert_eq!(parent, None);
    assert_eq!(nested, None);
}

#[tokio::test]
async fn test_summary_source_retains_frontmatter_and_prompt_presence_only() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let large_definition = format!(
        "---\nname: Reviewer\ndescription: Reviews code\n---\n{}",
        "x".repeat(SUMMARY_BODY_BUFFER_BYTES.saturating_mul(16))
    );
    write_definition(workspace.path(), "large", &large_definition).await;
    write_definition(workspace.path(), "empty", "").await;
    write_definition(
        workspace.path(),
        "invalid-start",
        "name: Reviewer\nignored body",
    )
    .await;
    write_definition(
        workspace.path(),
        "missing-end",
        "---\nname: Reviewer\ndescription: Reviews code",
    )
    .await;
    write_definition(
        workspace.path(),
        "missing-prompt",
        "---\nname: Reviewer\ndescription: Reviews code\n---\n \n",
    )
    .await;
    let definitions = workspace.path().join(".agents").join("agents");

    // Act
    let large = read_personality_summary_source(&definitions.join("large").join("agent.md"))
        .await
        .expect("large summary source should load");
    let empty = read_personality_summary_source(&definitions.join("empty").join("agent.md"))
        .await
        .expect("empty summary source should load");
    let invalid_start =
        read_personality_summary_source(&definitions.join("invalid-start").join("agent.md"))
            .await
            .expect("invalid summary source should load");
    let missing_end =
        read_personality_summary_source(&definitions.join("missing-end").join("agent.md"))
            .await
            .expect("unterminated summary source should load");
    let missing_prompt =
        read_personality_summary_source(&definitions.join("missing-prompt").join("agent.md"))
            .await
            .expect("promptless summary source should load");

    // Assert
    assert_eq!(
        large,
        "---\nname: Reviewer\ndescription: Reviews code\n---\nprompt\n"
    );
    assert_eq!(empty, "");
    assert_eq!(invalid_start, "name: Reviewer\n");
    assert_eq!(
        missing_end,
        "---\nname: Reviewer\ndescription: Reviews code"
    );
    assert_eq!(
        missing_prompt,
        "---\nname: Reviewer\ndescription: Reviews code\n---\n"
    );
}

#[tokio::test]
async fn test_summary_body_scan_validates_utf8_across_bounded_chunks() {
    // Arrange
    let mut split_body = vec![b' '; SUMMARY_BODY_BUFFER_BYTES.saturating_sub(1)];
    split_body.extend_from_slice("é".as_bytes());
    let mut split_reader = split_body.as_slice();
    let invalid_body = [b'x', 0xff];
    let mut invalid_reader = invalid_body.as_slice();
    let incomplete_body = [b'x', 0xc3];
    let mut incomplete_reader = incomplete_body.as_slice();

    // Act
    let has_content = body_contains_non_whitespace(&mut split_reader)
        .await
        .expect("split UTF-8 should remain valid");
    let invalid_error = body_contains_non_whitespace(&mut invalid_reader)
        .await
        .expect_err("invalid UTF-8 should fail");
    let incomplete_error = body_contains_non_whitespace(&mut incomplete_reader)
        .await
        .expect_err("incomplete UTF-8 should fail");

    // Assert
    assert!(has_content);
    assert_eq!(invalid_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(incomplete_error.kind(), io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn test_real_catalog_resolves_declared_id_and_directory_fallback() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    write_definition(
        workspace.path(),
        "reviewer-folder",
        "---\nid: reviewer\nname: Reviewer\ndescription: Reviews code\n---\nReview.",
    )
    .await;
    write_definition(
        workspace.path(),
        "planner",
        "---\nname: Planner\ndescription: Plans work\n---\nPlan.",
    )
    .await;
    let catalog = RealPersonalityCatalogClient;

    // Act
    let declared = catalog
        .resolve(workspace.path().to_path_buf(), "reviewer".to_string())
        .await;
    let fallback = catalog
        .resolve(workspace.path().to_path_buf(), "planner".to_string())
        .await;

    // Assert
    assert_eq!(
        declared.map(|personality| personality.name),
        Some("Reviewer".to_string())
    );
    assert_eq!(
        fallback.map(|personality| personality.name),
        Some("Planner".to_string())
    );
}

#[tokio::test]
async fn test_real_catalog_skips_unavailable_entries_and_returns_none() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let agents_directory = workspace.path().join(".agents").join("agents");
    tokio::fs::create_dir_all(agents_directory.join("a-missing"))
        .await
        .expect("create directory without definition");
    write_definition(
        workspace.path(),
        "requested",
        "---\nid: other\nname: Other\ndescription: Different id\n---\nOther prompt.",
    )
    .await;
    let invalid_directory = agents_directory.join("invalid");
    tokio::fs::create_dir_all(&invalid_directory)
        .await
        .expect("create invalid definition directory");
    tokio::fs::write(invalid_directory.join("agent.md"), [0xff])
        .await
        .expect("write invalid UTF-8 definition");
    let catalog = RealPersonalityCatalogClient;

    // Act
    let mismatched = catalog
        .resolve(workspace.path().to_path_buf(), "requested".to_string())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let unreadable = catalog
        .resolve(workspace.path().to_path_buf(), "invalid".to_string())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let missing = catalog
        .resolve(workspace.path().to_path_buf(), "absent".to_string())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert!(mismatched.is_none());
    assert!(unreadable.is_none());
    assert!(missing.is_none());
}

#[tokio::test]
async fn test_duplicate_id_resolution_matches_picker_directory_winner() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    write_definition(
        workspace.path(),
        "a",
        "---\nid: reviewer\nname: First Reviewer\ndescription: First duplicate\n---\nFirst prompt.",
    )
    .await;
    write_definition(
        workspace.path(),
        "reviewer",
        "---\nid: reviewer\nname: Named Reviewer\ndescription: Named directory\n---\nNamed prompt.",
    )
    .await;
    let catalog = RealPersonalityCatalogClient;

    // Act
    let summaries = catalog
        .list_summaries(workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let resolved = catalog
        .resolve(workspace.path().to_path_buf(), "reviewer".to_string())
        .await
        .expect("picker personality should resolve");

    // Assert
    assert_eq!(
        summaries,
        vec![PersonalitySummary {
            description: "First duplicate".to_string(),
            id: "reviewer".to_string(),
            name: "First Reviewer".to_string(),
        }]
    );
    assert_eq!(resolved.name, "First Reviewer");
    assert_eq!(resolved.prompt, "First prompt.");
}

#[tokio::test]
async fn test_real_catalog_returns_empty_for_missing_or_malformed_catalog() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let catalog = RealPersonalityCatalogClient;

    // Act
    let missing = catalog.list_summaries(workspace.path().to_path_buf()).await;
    write_definition(
        workspace.path(),
        "malformed",
        "---\nname malformed\n---\nPrompt.",
    )
    .await;
    tokio::fs::create_dir(
        workspace
            .path()
            .join(".agents")
            .join("agents")
            .join("missing-definition"),
    )
    .await
    .expect("create agent directory without a definition");
    let malformed = catalog
        .list_summaries(workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert_eq!(
        missing,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
    assert_eq!(
        malformed,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
}

#[tokio::test]
async fn test_real_catalog_handles_missing_workspace_and_unreadable_definition() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let missing_workspace = workspace.path().join("missing");
    let invalid_directory = workspace
        .path()
        .join(".agents")
        .join("agents")
        .join("invalid");
    tokio::fs::create_dir_all(&invalid_directory)
        .await
        .expect("create invalid definition directory");
    tokio::fs::write(invalid_directory.join("agent.md"), [0xff])
        .await
        .expect("write invalid UTF-8 definition");
    let catalog = RealPersonalityCatalogClient;

    // Act
    let missing = catalog
        .list_summaries(missing_workspace)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let unreadable = catalog
        .list_summaries(workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert_eq!(
        missing,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
    assert_eq!(
        unreadable,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
}

#[tokio::test]
async fn test_catalog_helpers_fail_closed_for_io_and_containment_errors() {
    // Arrange
    let workspace = Path::new("/workspace");
    let agents_directory = workspace.join(".agents").join("agents");
    let definition_path = agents_directory.join("reviewer").join("agent.md");
    let outside_definition = PathBuf::from("/outside/reviewer/agent.md");

    // Act
    let entries = list_catalog_personality_summaries(
        Err(io::Error::other("open failed")),
        workspace,
        &agents_directory,
    )
    .with_subscriber(crate::test_support::TestSubscriber)
    .await;
    let (
        next_entry,
        directory,
        missing_directory,
        missing_definition,
        definition,
        outside,
        unresolved,
    ) = tracing::subscriber::with_default(crate::test_support::TestSubscriber, || {
        (
            next_agent_directory(
                io::Result::Err(io::Error::other("enumeration failed")),
                &agents_directory,
            ),
            is_personality_directory(
                io::Result::Err(io::Error::other("metadata failed")),
                &agents_directory,
            ),
            is_personality_directory(
                io::Result::Err(io::Error::from(io::ErrorKind::NotFound)),
                &agents_directory,
            ),
            is_personality_definition(
                io::Result::Err(io::Error::from(io::ErrorKind::NotFound)),
                &definition_path,
            ),
            is_personality_definition(
                io::Result::Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "metadata failed",
                )),
                &definition_path,
            ),
            contain_personality_definition(Ok(outside_definition), workspace, &definition_path),
            contain_personality_definition(
                Err(io::Error::other("canonicalize failed")),
                workspace,
                &definition_path,
            ),
        )
    });

    // Assert
    assert_eq!(
        entries,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
    assert!(next_entry.is_none());
    assert!(!directory);
    assert!(!missing_directory);
    assert!(!missing_definition);
    assert!(!definition);
    assert!(outside.is_none());
    assert!(unresolved.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn test_real_catalog_rejects_catalog_links_outside_workspace() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let outside = tempfile::tempdir().expect("create outside directory");
    let agents_parent = workspace.path().join(".agents");
    tokio::fs::create_dir_all(&agents_parent)
        .await
        .expect("create agents parent");
    symlink(outside.path(), agents_parent.join("agents")).expect("create outside catalog link");
    let loop_workspace = tempfile::tempdir().expect("create loop workspace");
    let loop_agents_parent = loop_workspace.path().join(".agents");
    tokio::fs::create_dir_all(&loop_agents_parent)
        .await
        .expect("create loop agents parent");
    symlink("agents", loop_agents_parent.join("agents")).expect("create catalog link loop");
    let catalog = RealPersonalityCatalogClient;

    // Act
    let outside_personalities = catalog
        .list_summaries(workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let loop_personalities = catalog
        .list_summaries(loop_workspace.path().to_path_buf())
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert_eq!(
        outside_personalities,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
    assert_eq!(
        loop_personalities,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_real_catalog_rejects_symlinked_definitions_outside_workspace() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let outside = tempfile::tempdir().expect("create outside directory");
    let agents_directory = workspace.path().join(".agents").join("agents");
    tokio::fs::create_dir_all(&agents_directory)
        .await
        .expect("create catalog");
    write_definition(
        outside.path(),
        "external",
        "---\nname: External\ndescription: External prompt\n---\nExternal.",
    )
    .await;
    symlink(
        outside
            .path()
            .join(".agents")
            .join("agents")
            .join("external"),
        agents_directory.join("external"),
    )
    .expect("create symlink");
    let catalog = RealPersonalityCatalogClient;

    // Act
    let personalities = catalog.list_summaries(workspace.path().to_path_buf()).await;

    // Assert
    assert_eq!(
        personalities,
        [] as [crate::domain::personality::PersonalitySummary; 0]
    );
}

use super::support::{insert_session_fixture, load_session_row};
use crate::connection::Database;

/// Verifies stacked draft inserts persist their parent session link.
#[tokio::test]
async fn test_insert_stacked_draft_session_persists_parent_session_id() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert stacked draft session");
    let child_session = load_session_row(&database, "child-session").await;

    // Assert
    assert_eq!(child_session.base_branch, "wt/parent-session");
    assert!(child_session.is_draft);
    assert_eq!(
        child_session.parent_session_id.as_deref(),
        Some("parent-session")
    );
}

/// Verifies restacking clears active child parent links after parent merge.
#[tokio::test]
async fn test_restack_child_sessions_after_parent_merge_clears_active_children() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert active stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "review-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Review",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert review stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "canceled-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Canceled",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert canceled stacked child");

    // Act
    let restacked_child_session_ids = database
        .sessions()
        .restack_child_sessions_after_parent_merge(
            "parent-session",
            "main",
            Some("parent-tip".to_string()),
        )
        .await
        .expect("failed to restack child sessions");
    let child_session = load_session_row(&database, "child-session").await;
    let review_child = load_session_row(&database, "review-child").await;
    let review_child_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("review-child")
        .await
        .expect("failed to load review child stack base");
    let canceled_child = load_session_row(&database, "canceled-child").await;

    // Assert
    assert_eq!(
        restacked_child_session_ids,
        vec!["review-child".to_string()]
    );
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert_eq!(review_child.parent_session_id, None);
    assert_eq!(review_child.base_branch, "main");
    assert_eq!(review_child_stack_base.as_deref(), Some("parent-tip"));
    assert_eq!(
        canceled_child.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(canceled_child.base_branch, "wt/parent-session");
}

/// Verifies deleting a parent retargets surviving children onto the
/// parent's base branch instead of leaving them on the orphaned worktree
/// branch.
#[tokio::test]
async fn test_delete_session_retargets_children_base_branch() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "child-session",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Draft",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert active stacked child");
    database
        .sessions()
        .insert_stacked_draft_session(
            "canceled-child",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Canceled",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert canceled stacked child");

    // Act
    database
        .sessions()
        .delete_session("parent-session")
        .await
        .expect("failed to delete parent session");
    let child_session = load_session_row(&database, "child-session").await;
    let canceled_child = load_session_row(&database, "canceled-child").await;

    // Assert
    assert_eq!(child_session.parent_session_id, None);
    assert_eq!(child_session.base_branch, "main");
    assert_eq!(canceled_child.parent_session_id, None);
    assert_eq!(canceled_child.base_branch, "wt/parent-session");
}

#[tokio::test]
async fn test_load_pending_stack_restack_session_ids_returns_only_review_ready_parentless_rows() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "ready-child", "main", "Review", project_id).await;
    insert_session_fixture(&database, "draft-child", "main", "Draft", project_id).await;
    insert_session_fixture(&database, "plain-review", "main", "Review", project_id).await;
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    database
        .sessions()
        .insert_stacked_draft_session(
            "still-stacked",
            "gpt-5.6-sol",
            "wt/parent-session",
            "Review",
            "parent-session",
            project_id,
        )
        .await
        .expect("failed to insert stacked child");
    for session_id in ["ready-child", "draft-child", "still-stacked"] {
        database
            .sessions()
            .update_session_stack_base_commit_hash(session_id, Some("parent-tip".to_string()))
            .await
            .expect("failed to set stack base hash");
    }

    // Act
    let pending_session_ids = database
        .sessions()
        .load_pending_stack_restack_session_ids(project_id)
        .await
        .expect("failed to load pending restacks");

    // Assert
    assert_eq!(pending_session_ids, vec!["ready-child".to_string()]);
}

#[tokio::test]
async fn test_update_session_stack_membership_updates_and_clears_linkage_atomically() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "parent-session", "main", "Review", project_id).await;
    insert_session_fixture(&database, "child-session", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .update_session_stack_membership(
            "child-session",
            Some("parent-session"),
            "wt/parent-session",
            Some("old-parent-tip".to_string()),
        )
        .await
        .expect("failed to attach child");
    let attached = database
        .sessions()
        .load_session("child-session")
        .await
        .expect("failed to load attached child")
        .expect("attached child should exist");
    let attached_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("child-session")
        .await
        .expect("failed to load attached stack base");
    database
        .sessions()
        .update_session_stack_membership("child-session", None, "main", None)
        .await
        .expect("failed to clear child membership");
    let detached = database
        .sessions()
        .load_session("child-session")
        .await
        .expect("failed to load detached child")
        .expect("detached child should exist");
    let detached_stack_base = database
        .sessions()
        .get_session_stack_base_commit_hash("child-session")
        .await
        .expect("failed to load detached stack base");

    // Assert
    assert_eq!(
        attached.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(attached.base_branch, "wt/parent-session");
    assert_eq!(attached_stack_base.as_deref(), Some("old-parent-tip"));
    assert_eq!(detached.parent_session_id, None);
    assert_eq!(detached.base_branch, "main");
    assert_eq!(detached_stack_base, None);
}

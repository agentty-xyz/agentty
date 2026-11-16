//! Session message append and transcript loading persistence tests.

use ag_session::SessionMessageKind;

use super::support::insert_session_fixture;
use crate::connection::Database;

/// Verifies message appends write ordered rows for the canonical transcript
/// store.
#[tokio::test]
async fn test_append_session_message_writes_message_rows() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;

    // Act
    database
        .sessions()
        .append_session_message("session-a", SessionMessageKind::UserPrompt, "    hi ")
        .await
        .expect("failed to append prompt message");
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::AssistantAnswer,
            "\nHello\n",
        )
        .await
        .expect("failed to append assistant message");
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::WorkflowNotice,
            "\n[Sync Error] failed\n",
        )
        .await
        .expect("failed to append workflow notice");

    // Assert
    let messages = database
        .sessions()
        .load_session_messages("session-a")
        .await
        .expect("failed to load session messages");
    let detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load session detail")
        .expect("session detail should exist");
    assert_eq!(detail.prompt, "");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].position, 0);
    assert_eq!(messages[0].kind, SessionMessageKind::UserPrompt.as_str());
    assert_eq!(messages[0].content, "    hi");
    assert_eq!(messages[1].position, 1);
    assert_eq!(
        messages[1].kind,
        SessionMessageKind::AssistantAnswer.as_str()
    );
    assert_eq!(messages[1].content, "Hello");
    assert_eq!(messages[2].position, 2);
    assert_eq!(
        messages[2].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert_eq!(messages[2].content, "\n[Sync Error] failed\n");
}

/// Verifies canonical transcript appends refresh session list ordering
/// metadata.
#[tokio::test]
async fn test_append_session_message_refreshes_session_updated_at() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .sessions()
        .update_session_updated_at("session-a", 10)
        .await
        .expect("failed to backdate session updated_at");

    // Act
    database
        .sessions()
        .append_session_message(
            "session-a",
            SessionMessageKind::AssistantAnswer,
            "current answer",
        )
        .await
        .expect("failed to append assistant message");

    // Assert
    let (_, updated_at) = database
        .sessions()
        .load_session_timestamps("session-a")
        .await
        .expect("failed to load session timestamps")
        .expect("session timestamps should exist");
    assert!(
        updated_at > 10,
        "expected updated_at refresh, got {updated_at}"
    );
}

/// Verifies session detail loads transcript metadata from message rows.
#[tokio::test]
async fn test_load_session_detail_reads_message_transcript() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Review", project_id).await;
    database
        .sessions()
        .update_session_prompt("session-a", "Do something")
        .await
        .expect("failed to update prompt");
    // Act
    let detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load session detail")
        .expect("session detail should exist");

    // Assert
    assert_eq!(detail.prompt, "Do something");
}

/// Verifies staged draft attachments round-trip through session detail and
/// clear back to `None`.
#[tokio::test]
async fn test_update_session_draft_attachments_round_trips_through_detail() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;
    let serialized_attachments = r#"[{"placeholder":"[Image #1]"}]"#;

    // Act
    database
        .sessions()
        .update_session_draft_attachments("session-a", Some(serialized_attachments))
        .await
        .expect("failed to store draft attachments");
    let stored_detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load stored detail")
        .expect("session detail should exist");
    let stored_row = database
        .sessions()
        .load_session("session-a")
        .await
        .expect("failed to load stored session")
        .expect("session should exist");
    let stored_rows = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load stored sessions");
    database
        .sessions()
        .update_session_draft_attachments("session-a", None)
        .await
        .expect("failed to clear draft attachments");
    let cleared_detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load cleared detail")
        .expect("session detail should exist");

    // Assert
    assert_eq!(
        stored_detail.draft_attachments.as_deref(),
        Some(serialized_attachments)
    );
    assert_eq!(
        stored_row.draft_attachments.as_deref(),
        Some(serialized_attachments)
    );
    assert_eq!(
        stored_rows[0].draft_attachments.as_deref(),
        Some(serialized_attachments)
    );
    assert_eq!(cleared_detail.draft_attachments, None);
}

/// Verifies staged draft updates persist the prompt and attachments together
/// and leave both unchanged when the row update is rejected.
#[tokio::test]
async fn test_update_session_draft_writes_prompt_and_attachments_atomically() {
    // Arrange
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    insert_session_fixture(&database, "session-a", "main", "Draft", project_id).await;
    let first_attachments = r#"[{"placeholder":"[Image #1]"}]"#;
    let second_attachments = r#"[{"placeholder":"[Image #1]"},{"placeholder":"[Image #2]"}]"#;
    database
        .sessions()
        .update_session_draft("session-a", "first [Image #1]", Some(first_attachments))
        .await
        .expect("failed to store first draft");
    let first_detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load first draft")
        .expect("session detail should exist");
    sqlx::raw_sql(
        "CREATE TRIGGER reject_draft BEFORE UPDATE OF prompt ON session BEGIN SELECT RAISE(ABORT, \
         'draft failed'); END;",
    )
    .execute(database.pool())
    .await
    .expect("draft failure should be injected");

    // Act
    let result = database
        .sessions()
        .update_session_draft(
            "session-a",
            "second [Image #1] [Image #2]",
            Some(second_attachments),
        )
        .await;
    let rejected_detail = database
        .sessions()
        .load_session_detail("session-a")
        .await
        .expect("failed to load draft after rejection")
        .expect("session detail should exist");

    // Assert
    assert_eq!(first_detail.prompt, "first [Image #1]");
    assert_eq!(
        first_detail.draft_attachments.as_deref(),
        Some(first_attachments)
    );
    assert!(result.is_err());
    assert_eq!(rejected_detail.prompt, "first [Image #1]");
    assert_eq!(
        rejected_detail.draft_attachments.as_deref(),
        Some(first_attachments)
    );
}

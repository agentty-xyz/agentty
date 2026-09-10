use ag_session::SessionMessageKind;

use super::support::insert_session_fixture;
use crate::connection::Database;

/// Verifies message appends write ordered rows for the canonical transcript
/// store.
#[tokio::test]
async fn test_append_session_message_writes_message_rows() {
    // Arrange
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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
    let database = Database::open_in_memory()
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

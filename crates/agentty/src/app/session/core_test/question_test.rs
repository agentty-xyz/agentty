use std::path::PathBuf;

use ag_protocol::QuestionItem;
use tempfile::tempdir;

use super::super::session_folder;
use super::support::new_test_app_with_db;
use crate::domain::session::{SESSION_DATA_DIR, SessionId};
use crate::infra::db::AppRepositories;
use crate::presentation::app_mode::AppMode;

#[tokio::test]
async fn test_refresh_sessions_loads_question_detail_when_another_session_is_selected() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "alpha000",
            "gemini-3.8-flash",
            "main",
            "Question",
            project_id,
        )
        .await
        .expect("failed to insert alpha000");
    db.sessions()
        .insert_session("beta0000", "claude-opus-5", "main", "Done", project_id)
        .await
        .expect("failed to insert beta0000");
    db.sessions()
        .update_session_prompt("alpha000", "Alpha prompt")
        .await
        .expect("failed to set alpha000 prompt");
    db.sessions()
        .update_session_prompt("beta0000", "Beta prompt")
        .await
        .expect("failed to set beta0000 prompt");
    db.sessions()
        .update_session_updated_at("alpha000", 1)
        .await
        .expect("failed to set alpha000 timestamp");
    db.sessions()
        .update_session_updated_at("beta0000", 2)
        .await
        .expect("failed to set beta0000 timestamp");
    for session_id in ["alpha000", "beta0000"] {
        let session_dir = session_folder(dir.path(), session_id);
        let data_dir = session_dir.join(SESSION_DATA_DIR);
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
    }
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db,
    )
    .await;
    let question_session_id = SessionId::from("alpha000");
    let selected_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == "beta0000")
        .expect("beta0000 should be loaded");
    app.sessions.select_session_index(Some(selected_index));
    app.enter_question_mode(
        &question_session_id,
        vec![QuestionItem::new("Which target should be used?")],
    );

    // Act
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at("alpha000", "Question", 0)
        .await
        .expect("failed to update session status");
    app.refresh_sessions_now().await;

    // Assert
    assert_eq!(app.sessions.sessions()[0].id, "alpha000");
    assert!(matches!(
        app.mode,
        AppMode::Question { ref session_id, .. } if session_id == &question_session_id
    ));
    assert_eq!(
        app.sessions
            .selected_session()
            .map(|session| session.id.as_str()),
        Some("beta0000")
    );
    assert_eq!(
        app.sessions
            .session_for_id(&question_session_id)
            .map(|session| session.prompt.as_str()),
        Some("Alpha prompt")
    );
    assert_eq!(
        app.sessions
            .session_for_id("beta0000")
            .map(|session| session.prompt.as_str()),
        Some("")
    );
}

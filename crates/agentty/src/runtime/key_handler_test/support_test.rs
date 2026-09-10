use crate::app::App;
use crate::domain::session_message::SessionTranscript;

pub(super) fn session_replay_text(session: &crate::domain::session::Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

pub(super) async fn appendable_stack_test_app() -> (App, tempfile::TempDir, String, String) {
    let (mut app, base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let source_session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &parent_session_id,
        crate::domain::session::Status::Review,
    );
    crate::test_support::set_session_status_for_test(
        &mut app,
        &source_session_id,
        crate::domain::session::Status::Review,
    );

    (app, base_dir, parent_session_id, source_session_id)
}

/// Builds one in-memory project row for project switcher handler tests.
pub(super) fn switcher_project_item(
    project_id: i64,
    name: &str,
    path: std::path::PathBuf,
    last_opened_at: Option<i64>,
) -> crate::domain::project::ProjectListItem {
    crate::domain::project::ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: crate::domain::project::Project {
            created_at: 0,
            display_name: Some(name.to_string()),
            git_branch: None,
            id: project_id,
            is_favorite: false,
            last_opened_at,
            path,
            updated_at: 0,
        },
        session_count: 0,
    }
}

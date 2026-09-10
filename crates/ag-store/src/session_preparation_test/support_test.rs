use ag_agent::{PermissionMode, ReasoningLevel, ResponseStyle, SpeedMode};

use crate::AppRepositories;
use crate::session::PersistedSessionCreation;
use crate::session_preparation::SessionPreparationState;

/// Reserves a ready workspace with its first command durably queued.
pub(super) async fn prepare_saved_operation(db: &AppRepositories) {
    let project_id = db
        .projects()
        .upsert_project("/project", Some("main".to_string()))
        .await
        .expect("project");
    let sessions = db.sessions();
    sessions
        .reserve_session(reservation("first", project_id))
        .await
        .expect("session");
    sessions
        .save_preparation_prompt("first", "saved payload")
        .await
        .expect("prompt");
    sessions
        .update_session_preparation("first", SessionPreparationState::Ready, None)
        .await
        .expect("ready");
    db.operations()
        .insert_session_operation("workspace:first", "first", "start_prompt")
        .await
        .expect("operation");
}

pub(super) fn reservation(id: &str, project_id: i64) -> PersistedSessionCreation<'_> {
    PersistedSessionCreation {
        agent: "codex",
        base_branch: "main",
        id,
        is_draft: false,
        model: "gpt-5.6-sol",
        orchestration_task_id: None,
        parent_session_id: None,
        permission_mode: PermissionMode::AutoEdit,
        personality_id: None,
        project_id,
        reasoning_level: ReasoningLevel::default(),
        response_style: ResponseStyle::default(),
        role: None,
        speed_mode: SpeedMode::Normal,
        status: "Draft",
    }
}

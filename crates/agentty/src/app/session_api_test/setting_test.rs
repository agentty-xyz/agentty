use ag_agent::{AgentKind, AgentModel, PermissionMode, ReasoningLevel, SpeedMode};
use ag_forge::ForgeKind;

use super::super::build_api_session;
use super::support::session_row;
use crate::infra::db::SessionMessageRow;

#[test]
fn build_api_session_returns_complete_settings_and_messages() {
    // Arrange
    let row = session_row();
    let message_rows = vec![
        SessionMessageRow {
            content: "first".to_string(),
            kind: "user_prompt".to_string(),
            position: 0,
        },
        SessionMessageRow {
            content: "done".to_string(),
            kind: "assistant_answer".to_string(),
            position: 1,
        },
    ];

    // Act
    let session = build_api_session(row, message_rows, vec!["queued message".to_string()])
        .expect("row should convert");

    // Assert
    assert_eq!(session.id, "session-1");
    assert_eq!(session.draft_prompt.as_deref(), Some("staged prompt"));
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.questions[0].text, "Which target?");
    assert_eq!(session.queued_messages, ["queued message"]);
    assert_eq!(session.settings.project_id, 7);
    assert_eq!(session.settings.parent_session_id, Some("parent-1".into()));
    assert_eq!(session.settings.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(session.settings.personality_id.as_deref(), Some("reviewer"));
    assert_eq!(
        session.settings.agent,
        ag_agent::AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
    );
    assert_eq!(session.settings.reasoning_level, ReasoningLevel::XHigh);
    assert_eq!(session.settings.speed_mode, SpeedMode::Normal);
    assert_eq!(
        session
            .review_request
            .expect("review request should convert")
            .summary
            .forge_kind,
        ForgeKind::GitHub
    );
}

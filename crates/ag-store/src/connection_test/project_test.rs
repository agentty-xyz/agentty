use ag_agent::{AgentModel, SessionDiffState, SessionStats};

use crate::SessionTurnMetadata;
use crate::connection::Database;

#[tokio::test]
async fn test_load_projects_with_stats_returns_session_counts_tokens_and_last_update() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-a", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session-a");
    database
        .sessions()
        .persist_session_turn_metadata(
            "session-a",
            &SessionTurnMetadata {
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: None,
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 1_200,
                    output_tokens: 650,
                },
            },
        )
        .await
        .expect("failed to persist session-a token metadata");
    database
        .sessions()
        .insert_session("session-b", "gpt-5.6-sol", "main", "Done", project_id)
        .await
        .expect("failed to insert session-b");
    database
        .sessions()
        .persist_session_turn_metadata(
            "session-b",
            &SessionTurnMetadata {
                applied_personality_id: None,
                applied_personality_prompt_hash: None,
                instruction_conversation_id: None,
                model: AgentModel::Gpt56Sol.as_str().to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: SessionDiffState::Unknown,
                    input_tokens: 3,
                    output_tokens: 5,
                },
            },
        )
        .await
        .expect("failed to persist session-b token metadata");

    // Act
    let projects = database
        .projects()
        .load_projects_with_stats()
        .await
        .expect("failed to load projects with stats");

    // Assert
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].session_count, 2);
    assert_eq!(projects[0].input_tokens, 1_203);
    assert_eq!(projects[0].output_tokens, 655);
    assert!(projects[0].last_session_updated_at.is_some());
}

#[tokio::test]
async fn test_get_project_loads_persisted_favorite_state() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");

    sqlx::query("UPDATE project SET is_favorite = 1 WHERE id = ?")
        .bind(project_id)
        .execute(database.pool())
        .await
        .expect("failed to seed project favorite");

    // Act
    let project = database
        .projects()
        .get_project(project_id)
        .await
        .expect("failed to load project")
        .expect("expected existing project");

    // Assert
    assert!(project.is_favorite);
}

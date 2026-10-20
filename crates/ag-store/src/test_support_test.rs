//! Fixtures shared by persistence unit suites.

use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility,
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SubmitAgentTaskRequest,
    TaskAgentAssignment, TaskAgentInput,
};

use crate::Database;

/// Builds task repositories from the cached migrated schema.
pub(crate) async fn task_database_fixture() -> Database {
    let database = Database::in_memory().await.expect("database");
    let project = database
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    for session in ["session", "other"] {
        database
            .sessions()
            .insert_session(session, "gpt-6.1-sol", "main", "Review", project)
            .await
            .expect("session");
    }

    database
}

/// Supplies explicit role input without dispatching a model or accepting a
/// plan.
pub(crate) fn agent_task_request(attempt_id: &str) -> SubmitAgentTaskRequest {
    SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: attempt_id.to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: Some("Initial guidance".to_string()),
            user_request: "  Add the requested feature.\n".to_string(),
        },
        objective: "Implement the requested feature".to_string(),
        task_id: "task".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}

/// Builds the review-request domain fixture shared by repository persistence
/// tests.
pub(crate) fn review_request_fixture() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 456,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("2 approvals, checks passing".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

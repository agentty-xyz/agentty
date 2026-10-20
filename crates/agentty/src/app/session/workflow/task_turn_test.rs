use ag_contracts::LiveTranscript;
use ag_protocol::AgentResponse;
use ag_session::{
    AgentKind, AgentModel, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility,
    SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentContext, TaskAgentInput,
};

use super::{TaskReplay, effect_transcript, replay};
use crate::infra::db::{TaskAgentContextRow, TaskAgentHistoryRow, TaskAgentTurnRow};

#[test]
fn task_replay_is_json_encoded() {
    // Arrange
    let request = request();
    let mut history = TaskAgentHistoryRow {
        context: TaskAgentContextRow {
            context: TaskAgentContext {
                assignment: request.assignment,
                id: "context".to_string(),
                session_id: "session".into(),
                task_id: request.task_id.clone(),
            },
            input: request.input.clone(),
        },
        instruction_conversation_id: None,
        provider_conversation_id: None,
        turns: Vec::new(),
    };

    // Act / Assert
    assert_eq!(replay(&history), None);
    history.turns.push(TaskAgentTurnRow {
        assistant_message: "Quoted \"answer\"\n".to_string(),
        attempt_id: "old".to_string(),
        input: request.input.clone(),
        input_tokens: 1,
        output_tokens: 2,
    });
    let replay_text = replay(&history).expect("history");
    let parsed: serde_json::Value = serde_json::from_str(&replay_text).expect("JSON");
    assert_eq!(
        parsed["task_history"][0]["response"],
        history.turns[0].assistant_message
    );
    assert_eq!(
        TaskReplay(replay_text.clone()).replay_text(),
        Some(replay_text)
    );
    let transcript = effect_transcript(&history, &request, &AgentResponse::plain("Latest report"));
    assert!(!transcript.should_persist());
    assert_eq!(transcript.messages().len(), 4);
    assert!(
        transcript
            .replay_text()
            .expect("task effect context")
            .contains("Latest report")
    );
}

fn request() -> SubmitAgentTaskRequest {
    SubmitAgentTaskRequest {
        assignment: TaskAgentAssignment {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: BuiltinAgentDefinition::Planner,
        },
        attempt_id: "attempt".to_string(),
        input: TaskAgentInput {
            accepted_plan: None,
            clarification: None,
            user_request: "Task \"input\"\n".to_string(),
        },
        objective: "Plan".to_string(),
        task_id: "planner".to_string(),
        visibility: DurableMessageVisibility::Visible,
    }
}

use std::path::PathBuf;

use ag_contracts::{AgentRequestKind, ReasoningLevel, TurnEvent, TurnRequest};
use tokio::sync::mpsc;

use crate::app_server::AppServerTurnResponse;

pub(super) fn make_turn_request() -> TurnRequest {
    TurnRequest {
        continuation: ag_contracts::TurnContinuation::fresh(),
        folder: PathBuf::from("/tmp"),
        main_checkout_root: Some(PathBuf::from("/tmp/main")),
        model: "gpt-6-sol".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Do something".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ag_contracts::ResponseStyle::default(),
        speed_mode: ag_contracts::SpeedMode::default(),
    }
}

pub(super) fn make_ok_response(assistant_message: &str) -> AppServerTurnResponse {
    AppServerTurnResponse {
        assistant_message: assistant_message.to_string(),
        context_reset: false,
        input_tokens: 10,
        output_tokens: 5,
        pid: None,
        provider_conversation_id: None,
    }
}

pub(super) fn collect_pid_updates(
    events: &mut mpsc::UnboundedReceiver<TurnEvent>,
) -> Vec<Option<u32>> {
    let mut pids = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let TurnEvent::PidUpdate(pid) = event {
            pids.push(pid);
        }
    }

    pids
}

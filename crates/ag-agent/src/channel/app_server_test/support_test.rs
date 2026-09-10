use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::app_server::AppServerTurnResponse;
use crate::channel::contract::{AgentRequestKind, TurnEvent, TurnRequest};
use crate::model::agent::ReasoningLevel;

pub(super) fn make_turn_request() -> TurnRequest {
    TurnRequest {
        continuation: crate::channel::TurnContinuation::fresh(),
        folder: PathBuf::from("/tmp"),
        main_checkout_root: Some(PathBuf::from("/tmp/main")),
        model: "gpt-5.6-sol".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Do something".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: crate::ResponseStyle::default(),
        speed_mode: crate::model::session::SpeedMode::default(),
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

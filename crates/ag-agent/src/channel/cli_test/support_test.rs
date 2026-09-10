use std::path::PathBuf;

use crate::channel::contract::{AgentRequestKind, TurnRequest};
use crate::model::agent::ReasoningLevel;

pub(super) fn make_turn_request(folder: PathBuf) -> TurnRequest {
    TurnRequest {
        continuation: crate::channel::TurnContinuation::fresh(),
        folder,
        main_checkout_root: None,
        model: "claude-sonnet-5".to_string(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: "Write a test".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: crate::ResponseStyle::default(),
        speed_mode: crate::model::session::SpeedMode::default(),
    }
}

pub(super) fn stdin_capture_command(capture_path: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new("sh");
    command
        .arg("-c")
        .arg("cat > \"$CLI_CAPTURE_PATH\"; printf '%s' '{\"answer\":\"ok\",\"questions\":[]}'");
    command.env("CLI_CAPTURE_PATH", capture_path);

    command
}

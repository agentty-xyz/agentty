use std::path::PathBuf;

use ag_contracts::{AgentRequestKind, ReasoningLevel, TurnRequest};

pub(super) fn make_turn_request(folder: PathBuf) -> TurnRequest {
    TurnRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        continuation: ag_contracts::TurnContinuation::fresh(),
        folder,
        main_checkout_root: None,
        model: "claude-sonnet-5".to_string(),
        permission_mode: ag_contracts::PermissionMode::AutoEdit,
        personality: ag_contracts::PersonalityPrompt::default(),
        prompt: "Write a test".into(),
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::SessionStart,
        response_style: ag_contracts::ResponseStyle::default(),
        speed_mode: ag_contracts::SpeedMode::default(),
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

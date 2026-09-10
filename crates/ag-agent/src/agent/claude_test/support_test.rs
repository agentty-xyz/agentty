use super::*;

pub(super) fn session_start_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionStart
}

pub(super) fn utility_request_kind() -> AgentRequestKind {
    AgentRequestKind::UtilityPrompt
}

pub(super) fn settings_argument(command: &Command) -> Value {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let settings_position = args
        .iter()
        .position(|arg| arg == "--settings")
        .expect("--settings flag should be present");

    serde_json::from_str(&args[settings_position + 1]).expect("settings JSON should parse")
}

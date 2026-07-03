//! Shared app-server runtime command construction.
//!
//! Codex and Gemini use different process names and fixed arguments, but both
//! place the selected provider model into a long-lived app-server command that
//! runs in the session worktree.

use std::path::Path;
use std::process::Command;

/// Builds one `codex app-server` process command for one session folder.
pub(crate) fn build_codex_app_server_command(folder: &Path, model: &str) -> Command {
    let mut command = Command::new("codex");
    command
        .arg("--model")
        .arg(model)
        .arg("app-server")
        .arg("--listen")
        .arg("stdio://")
        .current_dir(folder);

    command
}

/// Builds one `gemini --acp` process command for one session folder.
pub(crate) fn build_gemini_acp_command(folder: &Path, model: &str) -> Command {
    let mut command = Command::new("gemini");
    command
        .arg("--acp")
        .arg("--model")
        .arg(model)
        .current_dir(folder);

    command
}

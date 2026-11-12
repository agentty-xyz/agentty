//! App-server runtime command construction.
//!
//! Places the selected provider model into a long-lived app-server command that
//! runs in the session worktree.

use std::path::Path;
use std::process::Command;

/// Builds one `codex app-server` process command for one session folder.
///
/// Applies the subagent limit at process startup, including resumed threads.
pub(crate) fn build_codex_app_server_command(
    folder: &Path,
    model: &str,
    policy: &ag_contracts::ExecutionPolicy,
) -> Command {
    let mut command = Command::new("codex");
    command.arg("--model").arg(model);
    if let Some(limit) = policy.max_concurrent_subagents {
        command
            .arg("-c")
            .arg(format!("agents.max_concurrent_threads_per_session={limit}"));
    }
    command
        .arg("app-server")
        .arg("--listen")
        .arg("stdio://")
        .current_dir(folder);

    command
}

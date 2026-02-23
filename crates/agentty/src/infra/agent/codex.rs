use std::path::Path;
use std::process::{Command, Stdio};

use super::backend::{AgentBackend, build_resume_prompt};

/// Uses non-interactive Codex commands so Agentty can capture piped output.
///
/// Interactive `codex` requires a TTY and fails in this app with
/// `Error: stdout is not a terminal`, so this backend runs
/// `codex exec --full-auto` and `codex exec resume --last --full-auto`.
pub(super) struct CodexBackend;

impl AgentBackend for CodexBackend {
    fn setup(&self, _folder: &Path) {
        // Codex CLI needs no config files
    }

    fn build_start_command(&self, folder: &Path, prompt: &str, model: &str) -> Command {
        let mut command = Command::new("codex");
        command
            .arg("exec")
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg("--json")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command
    }

    fn build_resume_command(
        &self,
        folder: &Path,
        prompt: &str,
        model: &str,
        session_output: Option<String>,
    ) -> Command {
        let prompt = build_resume_prompt(prompt, session_output.as_deref());
        let mut command = Command::new("codex");
        command
            .arg("exec")
            .arg("resume")
            .arg("--last")
            .arg("--model")
            .arg(model)
            .arg("--full-auto")
            .arg("--json")
            .arg(prompt)
            .current_dir(folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command
    }
}

impl CodexBackend {}

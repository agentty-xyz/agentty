use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Output;
use std::{env, io};

/// Returns whether Agentty is running inside a `tmux` session.
pub(crate) fn is_tmux_session() -> bool {
    has_tmux_environment(|name| env::var_os(name))
}

/// Returns whether the `TMUX` pane environment variable is nonempty.
fn has_tmux_environment(mut get_var: impl FnMut(&str) -> Option<OsString>) -> bool {
    get_var("TMUX").is_some_and(|value| !value.is_empty())
}

/// Boxed async result returned by [`TmuxClient`] methods.
pub type TmuxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Async tmux boundary used by app orchestration.
#[cfg_attr(test, mockall::automock)]
pub trait TmuxClient: Send + Sync {
    /// Opens one tmux window rooted at `session_folder`.
    ///
    /// Returns the tmux window id when creation succeeds.
    fn open_window_for_folder(&self, session_folder: PathBuf) -> TmuxFuture<Option<String>>;

    /// Sends `command` followed by Enter to the target tmux `window_id`.
    fn run_command_in_window(&self, window_id: String, command: String) -> TmuxFuture<()>;
}

/// Captured tmux subprocess result used by injected command runners.
#[derive(Debug, Eq, PartialEq)]
struct TmuxCommandOutput {
    status_success: bool,
    stdout: Vec<u8>,
}

impl TmuxCommandOutput {
    /// Converts one subprocess output into the reduced tmux result shape.
    fn from_process_output(output: Output) -> Self {
        Self {
            status_success: output.status.success(),
            stdout: output.stdout,
        }
    }
}

/// Async tmux command boundary used to test multi-command flows
/// deterministically.
#[cfg_attr(test, mockall::automock)]
trait TmuxCommandRunner: Send + Sync {
    /// Opens one tmux window rooted at `session_folder`.
    fn open_window(&self, session_folder: PathBuf) -> TmuxFuture<io::Result<TmuxCommandOutput>>;

    /// Sends literal `command` bytes to the target tmux `window_id`.
    fn send_literal_keys(
        &self,
        window_id: String,
        command: String,
    ) -> TmuxFuture<io::Result<TmuxCommandOutput>>;

    /// Sends Enter to the target tmux `window_id`.
    fn send_enter_key(&self, window_id: String) -> TmuxFuture<io::Result<TmuxCommandOutput>>;
}

/// Production tmux command runner backed by subprocess calls.
struct ProcessTmuxCommandRunner;

impl ProcessTmuxCommandRunner {
    /// Builds the `tmux new-window` command for one session folder.
    fn open_window_command(session_folder: PathBuf) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("tmux");
        command
            .arg("new-window")
            .arg("-P")
            .arg("-F")
            .arg("#{window_id}")
            .arg("-c")
            .arg(session_folder);

        command
    }

    /// Opens one tmux window in `session_folder`.
    async fn open_window_impl(session_folder: PathBuf) -> io::Result<TmuxCommandOutput> {
        let mut command = Self::open_window_command(session_folder);
        let output = command.output().await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }

    /// Builds the `tmux send-keys -l` command for one literal command string.
    fn send_literal_keys_command(window_id: String, command: String) -> tokio::process::Command {
        let mut tmux_command = tokio::process::Command::new("tmux");
        tmux_command
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("-l")
            .arg(command);

        tmux_command
    }

    /// Sends literal `command` bytes to one tmux `window_id`.
    async fn send_literal_keys_impl(
        window_id: String,
        command: String,
    ) -> io::Result<TmuxCommandOutput> {
        let mut tmux_command = Self::send_literal_keys_command(window_id, command);
        let output = tmux_command.output().await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }

    /// Builds the `tmux send-keys C-m` command for one window id.
    fn send_enter_key_command(window_id: String) -> tokio::process::Command {
        let mut tmux_command = tokio::process::Command::new("tmux");
        tmux_command
            .arg("send-keys")
            .arg("-t")
            .arg(window_id)
            .arg("C-m");

        tmux_command
    }

    /// Sends Enter to one tmux `window_id`.
    async fn send_enter_key_impl(window_id: String) -> io::Result<TmuxCommandOutput> {
        let mut tmux_command = Self::send_enter_key_command(window_id);
        let output = tmux_command.output().await?;

        Ok(TmuxCommandOutput::from_process_output(output))
    }
}

impl TmuxCommandRunner for ProcessTmuxCommandRunner {
    fn open_window(&self, session_folder: PathBuf) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::open_window_impl(session_folder).await })
    }

    fn send_literal_keys(
        &self,
        window_id: String,
        command: String,
    ) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::send_literal_keys_impl(window_id, command).await })
    }

    fn send_enter_key(&self, window_id: String) -> TmuxFuture<io::Result<TmuxCommandOutput>> {
        Box::pin(async move { Self::send_enter_key_impl(window_id).await })
    }
}

/// Production [`TmuxClient`] implementation backed by tmux subprocess calls.
pub struct RealTmuxClient;

impl RealTmuxClient {
    /// Opens one tmux window in `session_folder` and returns its window id.
    async fn open_window_for_folder_impl(session_folder: PathBuf) -> Option<String> {
        let command_runner = ProcessTmuxCommandRunner;

        Self::open_window_for_folder_with_runner(&command_runner, session_folder).await
    }

    /// Sends `command` and Enter to one tmux `window_id`.
    async fn run_command_in_window_impl(window_id: String, command: String) {
        let command_runner = ProcessTmuxCommandRunner;

        Self::run_command_in_window_with_runner(&command_runner, window_id, command).await;
    }

    /// Opens one tmux window using the provided command runner.
    async fn open_window_for_folder_with_runner(
        command_runner: &dyn TmuxCommandRunner,
        session_folder: PathBuf,
    ) -> Option<String> {
        let output = command_runner.open_window(session_folder).await.ok()?;
        if !output.status_success {
            return None;
        }

        Self::parse_tmux_window_id(&output.stdout)
    }

    /// Sends `command` and Enter using the provided command runner.
    async fn run_command_in_window_with_runner(
        command_runner: &dyn TmuxCommandRunner,
        window_id: String,
        command: String,
    ) {
        let send_literal_output = command_runner
            .send_literal_keys(window_id.clone(), command)
            .await;

        let Ok(send_literal_output) = send_literal_output else {
            return;
        };
        if !send_literal_output.status_success {
            return;
        }

        // Best-effort: tmux window may have already been closed.
        let _ = command_runner.send_enter_key(window_id).await;
    }

    /// Parses a tmux window id from command output bytes.
    fn parse_tmux_window_id(stdout: &[u8]) -> Option<String> {
        let window_id = std::str::from_utf8(stdout).ok()?.trim();
        if window_id.is_empty() {
            return None;
        }

        Some(window_id.to_string())
    }
}

impl TmuxClient for RealTmuxClient {
    fn open_window_for_folder(&self, session_folder: PathBuf) -> TmuxFuture<Option<String>> {
        Box::pin(async move { Self::open_window_for_folder_impl(session_folder).await })
    }

    fn run_command_in_window(&self, window_id: String, command: String) -> TmuxFuture<()> {
        Box::pin(async move { Self::run_command_in_window_impl(window_id, command).await })
    }
}

#[cfg(test)]
#[path = "tmux_test.rs"]
mod tests;

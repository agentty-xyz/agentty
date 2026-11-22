//! `tmux` boundary for detecting sessions, opening windows, and sending
//! commands.

use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Output, Stdio};
use std::{env, io};

/// Server option that lets pane applications request modified-key reports.
const EXTENDED_KEYS_OPTION: &str = "extended-keys";
/// Server option that selects the modified-key report encoding.
const EXTENDED_KEYS_FORMAT_OPTION: &str = "extended-keys-format";
/// Session option that routes mouse input through `tmux` key bindings.
const MOUSE_OPTION: &str = "mouse";

/// How the enclosing `tmux` server reports modified keys to Agentty's pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TmuxKeyReporting {
    /// `tmux` reports modified keys as CSI-u sequences that the TUI parses.
    CsiU,
    /// Agentty is not running inside `tmux`.
    Outside,
    /// `tmux` keeps a modified-key format that the TUI cannot parse.
    Unsupported,
}

/// Returns whether Agentty is running inside a `tmux` session.
pub(crate) fn is_tmux_session() -> bool {
    has_tmux_environment(|name| env::var_os(name))
}

/// Prepares the enclosing `tmux` server so modified keys such as
/// `Shift+Enter` reach the TUI in a format it can parse.
///
/// `tmux` defaults to the xterm `CSI 27;…~` report format, which the terminal
/// input parser drops; requesting modified keys under it would silently
/// swallow `Shift+Enter` and every `Ctrl` shortcut. Switching the server to
/// CSI-u is safe for other panes because applications that request modified
/// keys also accept CSI-u. Enabling `extended-keys` only affects panes that
/// request it and applies from the next client attach.
pub(crate) fn prepare_key_reporting() -> TmuxKeyReporting {
    if !is_tmux_session() {
        return TmuxKeyReporting::Outside;
    }

    prepare_key_reporting_with_runner(&ProcessTmuxOptionRunner)
}

/// Returns whether Agentty runs inside a `tmux` session with `mouse` on.
///
/// With `mouse on`, `tmux` discards wheel input over an alternate-screen pane
/// unless the pane requested mouse reports. With `mouse off`, the outer
/// terminal already turns the wheel into arrow keys, so requesting reports
/// would only take native text selection and link clicks away from it.
pub(crate) fn is_mouse_enabled() -> bool {
    is_tmux_session() && ProcessTmuxOptionRunner.is_session_option_on(MOUSE_OPTION)
}

/// Returns whether the `TMUX` pane environment variable is nonempty.
fn has_tmux_environment(mut get_var: impl FnMut(&str) -> Option<OsString>) -> bool {
    get_var("TMUX").is_some_and(|value| !value.is_empty())
}

/// Applies the `tmux` server options needed for parseable modified keys.
fn prepare_key_reporting_with_runner(runner: &dyn TmuxOptionRunner) -> TmuxKeyReporting {
    if runner.show_server_option(EXTENDED_KEYS_OPTION).as_deref() == Some("off") {
        // Best-effort: the outer terminal is asked for extended keys only on
        // the next attach, so this cannot change the current result.
        let _ = runner.set_server_option(EXTENDED_KEYS_OPTION, "on");
    }

    let is_csi_u = match runner
        .show_server_option(EXTENDED_KEYS_FORMAT_OPTION)
        .as_deref()
    {
        // `tmux` before 3.5 has no format option and always reports CSI-u.
        Some("csi-u") | None => true,
        Some(_) => runner.set_server_option(EXTENDED_KEYS_FORMAT_OPTION, "csi-u"),
    };

    if is_csi_u {
        TmuxKeyReporting::CsiU
    } else {
        TmuxKeyReporting::Unsupported
    }
}

/// Synchronous `tmux` option boundary used during terminal setup.
#[cfg_attr(test, mockall::automock)]
trait TmuxOptionRunner {
    /// Returns one server option value, or `None` when `tmux` cannot report
    /// it.
    fn show_server_option(&self, name: &str) -> Option<String>;

    /// Sets one server option and returns whether `tmux` accepted it.
    fn set_server_option(&self, name: &str, value: &str) -> bool;

    /// Returns whether one current-session flag option, including a value
    /// inherited from global options, resolves to `on`; `false` when `tmux`
    /// cannot report it.
    fn is_session_option_on(&self, name: &str) -> bool;
}

/// Production option runner backed by `tmux` subprocess calls.
struct ProcessTmuxOptionRunner;

impl ProcessTmuxOptionRunner {
    /// Builds the `tmux show-options -sv` command for one server option.
    fn show_server_option_command(name: &str) -> std::process::Command {
        let mut command = Self::tmux_command();
        command.arg("show-options").arg("-sv").arg(name);

        command
    }

    /// Builds the `tmux show-options -Av` command for one session option,
    /// resolving inherited global values.
    fn show_session_option_command(name: &str) -> std::process::Command {
        let mut command = Self::tmux_command();
        command.arg("show-options").arg("-Av").arg(name);

        command
    }

    /// Builds the `tmux set-option -s` command for one server option.
    fn set_server_option_command(name: &str, value: &str) -> std::process::Command {
        let mut command = Self::tmux_command();
        command.arg("set-option").arg("-s").arg(name).arg(value);

        command
    }

    /// Builds a `tmux` command that cannot read or draw on the TUI terminal.
    fn tmux_command() -> std::process::Command {
        let mut command = std::process::Command::new("tmux");
        command.stdin(Stdio::null()).stderr(Stdio::null());

        command
    }

    /// Runs one `tmux show-options` command and returns its trimmed value.
    fn option_value(mut command: std::process::Command) -> Option<String> {
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }

        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl TmuxOptionRunner for ProcessTmuxOptionRunner {
    fn show_server_option(&self, name: &str) -> Option<String> {
        Self::option_value(Self::show_server_option_command(name))
    }

    fn set_server_option(&self, name: &str, value: &str) -> bool {
        Self::set_server_option_command(name, value)
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn is_session_option_on(&self, name: &str) -> bool {
        Self::option_value(Self::show_session_option_command(name)).as_deref() == Some("on")
    }
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

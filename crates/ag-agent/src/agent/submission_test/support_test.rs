use super::*;

/// Builds one shell command that emits controlled stdout/stderr and exits.
pub(super) fn mock_shell_command(stdout: &str, stderr: &str, exit_code: i32) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(
        "printf '%s' \"$ONE_SHOT_STDOUT\"; printf '%s' \"$ONE_SHOT_STDERR\" >&2; exit \
         \"$ONE_SHOT_EXIT\"",
    );
    command.env("ONE_SHOT_STDOUT", stdout);
    command.env("ONE_SHOT_STDERR", stderr);
    command.env("ONE_SHOT_EXIT", exit_code.to_string());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    command
}

/// Builds one shell command that captures stdin before returning JSON.
pub(super) fn stdin_capture_shell_command(capture_path: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(
        "cat > \"$ONE_SHOT_CAPTURE_PATH\"; printf '%s' \
         '{\"answer\":\"captured\",\"questions\":[]}'",
    );
    command.env("ONE_SHOT_CAPTURE_PATH", capture_path);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    command
}

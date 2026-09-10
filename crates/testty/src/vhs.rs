//! VHS tape compiler for generating visual screenshot tapes from scenarios.
//!
//! Compiles a [`Scenario`] into VHS tape syntax so the same test journey
//! that runs semantically in a PTY also produces a visual screenshot via
//! the `vhs` tool. The tape includes environment setup, binary launch,
//! interaction steps, and screenshot capture.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::scenario::Scenario;
use crate::step::Step;

/// Maximum number of VHS execution retries.
const MAX_VHS_RETRIES: u8 = 3;

/// Configurable VHS tape rendering settings.
///
/// Controls the visual appearance of generated GIF recordings. Use
/// [`VhsTapeSettings::default()`] for compact proof GIFs or
/// [`VhsTapeSettings::feature_demo()`] for browser-ready feature
/// showcase recordings.
#[derive(Debug, Clone)]
pub struct VhsTapeSettings {
    /// Font size in points.
    pub font_size: u16,
    /// GIF framerate in frames per second.
    pub framerate: u16,
    /// Terminal height in pixels.
    pub height: u16,
    /// Terminal padding in pixels.
    pub padding: u16,
    /// VHS theme name (e.g. `"OneDark"`, `"Dracula"`).
    pub theme: String,
    /// Terminal width in pixels.
    pub width: u16,
}

impl VhsTapeSettings {
    /// Browser-ready preset for feature demo GIFs.
    ///
    /// Produces sharp recordings at 1600×800, font size 18,
    /// `OneDark` theme, and 30 fps.
    pub fn feature_demo() -> Self {
        Self {
            width: 1600,
            height: 800,
            font_size: 18,
            theme: "OneDark".to_string(),
            framerate: 30,
            padding: 0,
        }
    }
}

impl Default for VhsTapeSettings {
    /// Return compact settings matching the legacy VHS tape defaults.
    fn default() -> Self {
        Self {
            width: 1200,
            height: 600,
            font_size: 14,
            theme: String::new(),
            framerate: 0,
            padding: 0,
        }
    }
}

/// A compiled VHS tape ready for writing and execution.
///
/// Generated from a [`Scenario`] with environment and binary configuration.
/// The tape uses VHS commands (`Set`, `Hide`, `Show`, `Type`, `Sleep`,
/// `Wait+Screen`, `Wait+Line`, `Screenshot`) to reproduce the scenario
/// journey and capture a PNG screenshot.
pub struct VhsTape {
    /// The rendered tape content as VHS syntax.
    content: String,
    /// Path where the screenshot will be saved.
    screenshot_path: PathBuf,
}

impl VhsTape {
    /// Compile a scenario into a VHS tape using default settings.
    ///
    /// The tape sets up the environment, launches the binary, executes
    /// the scenario steps, and captures a screenshot at each `Capture`
    /// step.
    pub fn from_scenario(
        scenario: &Scenario,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
    ) -> Self {
        Self::from_scenario_with_settings(
            scenario,
            binary_path,
            screenshot_path,
            env_vars,
            &VhsTapeSettings::default(),
        )
    }

    /// Compile a scenario into a VHS tape with explicit rendering settings.
    ///
    /// Use [`VhsTapeSettings::feature_demo()`] for browser-ready feature
    /// GIFs or [`VhsTapeSettings::default()`] for compact proof recordings.
    pub fn from_scenario_with_settings(
        scenario: &Scenario,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
        settings: &VhsTapeSettings,
    ) -> Self {
        let gif_stem = screenshot_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let gif_path = screenshot_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{gif_stem}.gif"));

        Self::from_scenario_with_output_path(
            scenario,
            binary_path,
            &gif_path,
            screenshot_path,
            env_vars,
            settings,
        )
    }

    /// Return the rendered tape content as a string.
    pub fn render(&self) -> &str {
        &self.content
    }

    /// Write the tape to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the file fails.
    pub fn write_to(&self, tape_path: &Path) -> Result<(), std::io::Error> {
        std::fs::write(tape_path, &self.content)
    }

    /// Execute the tape using the `vhs` CLI and return the screenshot path.
    ///
    /// Retries up to [`MAX_VHS_RETRIES`] times if the screenshot is not
    /// produced.
    ///
    /// # Errors
    ///
    /// Returns an error if VHS is not installed, execution fails, or the
    /// screenshot is not produced after retries.
    pub fn execute(&self, tape_path: &Path) -> Result<PathBuf, VhsError> {
        check_vhs_installed()?;
        self.write_to(tape_path)
            .map_err(|err| VhsError::IoError(err.to_string()))?;

        let mut last_error = String::new();

        for attempt in 1..=MAX_VHS_RETRIES {
            // Best-effort cleanup: screenshot file may already be removed.
            let _ = std::fs::remove_file(&self.screenshot_path);

            let output = Command::new("vhs")
                .arg(tape_path)
                .output()
                .map_err(|err| VhsError::ExecutionFailed(err.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);

                return Err(VhsError::ExecutionFailed(format!(
                    "VHS exited with error: {stderr}"
                )));
            }

            if self.screenshot_path.exists() {
                return Ok(self.screenshot_path.clone());
            }

            last_error = format!(
                "Attempt {attempt}/{MAX_VHS_RETRIES}: screenshot not produced at {}",
                self.screenshot_path.display()
            );
        }

        Err(VhsError::ScreenshotNotProduced(last_error))
    }

    /// Return the path where the screenshot will be saved.
    pub fn screenshot_path(&self) -> &Path {
        &self.screenshot_path
    }

    /// Compile a scenario while keeping GIF output separate from screenshots.
    pub(crate) fn from_scenario_with_output_path(
        scenario: &Scenario,
        binary_path: &Path,
        gif_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
        settings: &VhsTapeSettings,
    ) -> Self {
        let content = compile_tape(
            scenario,
            binary_path,
            gif_path,
            screenshot_path,
            env_vars,
            settings,
        );

        Self {
            content,
            screenshot_path: screenshot_path.to_path_buf(),
        }
    }
}

/// Errors from VHS tape operations.
#[derive(Debug, thiserror::Error)]
pub enum VhsError {
    /// VHS is not installed or not on PATH.
    #[error("VHS not installed: {0}")]
    NotInstalled(String),

    /// VHS execution failed.
    #[error("VHS execution failed: {0}")]
    ExecutionFailed(String),

    /// VHS ran but did not produce a screenshot.
    #[error("Screenshot not produced: {0}")]
    ScreenshotNotProduced(String),

    /// I/O error writing or reading files.
    #[error("I/O error: {0}")]
    IoError(String),
}

/// Verify that VHS is installed and available on `PATH`.
///
/// # Errors
///
/// Returns [`VhsError::NotInstalled`] when `vhs --version` cannot be
/// executed (binary missing or not on `PATH`).
pub fn check_vhs_installed() -> Result<(), VhsError> {
    Command::new("vhs").arg("--version").output().map_err(|_| {
        VhsError::NotInstalled("VHS is not installed. Install with: brew install vhs".to_string())
    })?;

    Ok(())
}

/// Compile a scenario into VHS tape syntax.
fn compile_tape(
    scenario: &Scenario,
    binary_path: &Path,
    gif_path: &Path,
    screenshot_path: &Path,
    env_vars: &[(&str, &str)],
    settings: &VhsTapeSettings,
) -> String {
    let mut tape = String::new();

    // Infallible: all `writeln!` calls below write to a String, which cannot
    // fail. Header settings.
    let _ = writeln!(tape, "Set Shell \"bash\"");
    let _ = writeln!(tape, "Set FontSize {}", settings.font_size);
    let _ = writeln!(tape, "Set Width {}", settings.width);
    let _ = writeln!(tape, "Set Height {}", settings.height);
    let _ = writeln!(tape, "Set Padding {}", settings.padding);
    let _ = writeln!(tape, "Set TypingSpeed 0");

    if !settings.theme.is_empty() {
        let _ = writeln!(
            tape,
            "Set Theme \"{}\"",
            escape_vhs_double_quote(&settings.theme)
        );
    }

    if settings.framerate > 0 {
        let _ = writeln!(tape, "Set Framerate {}", settings.framerate);
    }

    let _ = writeln!(tape);
    let _ = writeln!(
        tape,
        "Output \"{}\"",
        escape_vhs_double_quote(&gif_path.display().to_string())
    );
    let _ = writeln!(tape);

    // Hidden setup: export environment variables, clear terminal, and
    // launch the binary so only the running application is recorded.
    let _ = writeln!(tape, "Hide");
    for (key, value) in env_vars {
        let escaped_value = escape_shell_single_quote(value);
        let export_cmd = format!("export {key}='{escaped_value}'");
        let _ = writeln!(tape, "Type \"{}\"", escape_vhs_double_quote(&export_cmd));
        let _ = writeln!(tape, "Enter");
        let _ = writeln!(tape, "Sleep 200ms");
    }

    // Clear the terminal so the export commands are not visible when
    // recording starts, then launch the binary while still hidden.
    let _ = writeln!(tape, "Type \"clear\"");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let escaped_binary = escape_shell_single_quote(&binary_path.display().to_string());
    let _ = writeln!(
        tape,
        "Type \"{}\"",
        escape_vhs_double_quote(&format!("'{escaped_binary}'"))
    );
    let _ = writeln!(tape, "Enter");
    // Wait for the application to start and take over the terminal
    // before beginning the visible recording.
    let _ = writeln!(tape, "Sleep 2s");
    let _ = writeln!(tape, "Show");
    let _ = writeln!(tape);

    // Compile scenario steps.
    for step in &scenario.steps {
        compile_step(&mut tape, step, screenshot_path);
    }

    // Hidden teardown.
    let _ = writeln!(tape);
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(tape, "Type \"q\"");
    let _ = writeln!(tape, "Sleep 1s");

    tape
}

/// Compile a single step into VHS tape commands.
fn compile_step(tape: &mut String, step: &Step, screenshot_path: &Path) {
    // Infallible: all `writeln!` calls below write to a String, which cannot
    // fail.
    match step {
        Step::WriteText(text) => {
            let _ = writeln!(tape, "Type \"{}\"", escape_vhs_double_quote(text));
        }
        Step::PressKey(key) => {
            let vhs_key = key_to_vhs_command(key);
            let _ = writeln!(tape, "{vhs_key}");
        }
        Step::Sleep(duration) | Step::ViewingPause(duration) => {
            let ms = duration.as_millis();

            if ms >= 1000 && ms % 1000 == 0 {
                let _ = writeln!(tape, "Sleep {}s", ms / 1000);
            } else {
                let _ = writeln!(tape, "Sleep {ms}ms");
            }
        }
        Step::WaitForText { needle, timeout_ms } => {
            let timeout = format_vhs_duration(*timeout_ms);
            let _ = writeln!(
                tape,
                "Wait+Screen@{timeout} /{needle}/",
                needle = escape_vhs_regex(needle)
            );
        }
        Step::WaitForStableFrame {
            stable_ms,
            timeout_ms: _,
        } => {
            // VHS does not have a direct "wait for stable" command.
            // Approximate by sleeping for the stable duration.
            let _ = writeln!(tape, "Sleep {stable_ms}ms");
        }
        Step::Capture | Step::CaptureLabeled { .. } => {
            let _ = writeln!(
                tape,
                "Screenshot \"{}\"",
                escape_vhs_double_quote(&screenshot_path.display().to_string())
            );
        }
        Step::Eventually { timeout, .. } => {
            // VHS recordings have no predicate-driven wait primitive, so
            // approximate `Eventually` with a fixed `Sleep` for the full
            // timeout. Skipping the step would let the next `Screenshot`
            // fire before the predicate condition is satisfied and
            // capture a misleading frame; sleeping the full window
            // preserves the upper bound the PTY executor would have
            // observed in the worst case while still bounding total
            // recording time.
            let ms = timeout.as_millis();

            if ms >= 1000 && ms % 1000 == 0 {
                let _ = writeln!(tape, "Sleep {}s", ms / 1000);
            } else {
                let _ = writeln!(tape, "Sleep {ms}ms");
            }
        }
    }
}

/// Convert a key name to the corresponding VHS command.
fn key_to_vhs_command(key: &str) -> String {
    match key.to_lowercase().as_str() {
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "backtab" | "shift+tab" => "Shift+Tab".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "backspace" => "Backspace".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "right" => "Right".to_string(),
        "left" => "Left".to_string(),
        "space" => "Space".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        other => {
            if let Some(character) = other.strip_prefix("ctrl+") {
                format!("Ctrl+{}", character.to_uppercase())
            } else {
                format!("Type \"{}\"", escape_vhs_double_quote(other))
            }
        }
    }
}

/// Escape double quotes inside a string for use in VHS double-quoted
/// arguments (e.g., `Type "..."`, `Screenshot "..."`).
fn escape_vhs_double_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape single quotes inside a value for use in a POSIX single-quoted
/// shell string. The standard trick is to end the current single-quoted
/// segment, insert an escaped single quote, and restart a new segment:
/// `'` → `'\''`.
fn escape_shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Format a millisecond duration as a VHS-compatible Go duration string.
///
/// Produces `"{n}s"` when the value is an exact multiple of 1000,
/// otherwise `"{n}ms"`. VHS expects Go `time.Duration` syntax
/// (e.g. `5s`, `500ms`), not decimal seconds like `5.0s`.
fn format_vhs_duration(milliseconds: u32) -> String {
    if milliseconds >= 1000 && milliseconds.is_multiple_of(1000) {
        format!("{}s", milliseconds / 1000)
    } else {
        format!("{milliseconds}ms")
    }
}

/// Escape special regex metacharacters for use inside a VHS `/regex/`
/// pattern. VHS `Wait+Screen` and `Wait+Line` use Go-style regex, so
/// forward slashes and common metacharacters need escaping.
fn escape_vhs_regex(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        if matches!(
            character,
            '/' | '.'
                | '*'
                | '+'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | '^'
                | '$'
                | '\\'
        ) {
            escaped.push('\\');
        }

        escaped.push(character);
    }

    escaped
}

#[cfg(test)]
#[path = "vhs_test.rs"]
mod tests;

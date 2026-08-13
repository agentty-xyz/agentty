//! VHS tape compiler for generating visual recordings from scenarios.
//!
//! Compiles a [`Scenario`] into VHS tape syntax so the same test journey
//! that runs semantically in a PTY also produces a visual recording via
//! the `vhs` tool. The tape includes environment setup, binary launch,
//! interaction steps, and final-frame poster extraction.

use std::fmt::Write;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, Frame};

use crate::scenario::Scenario;
use crate::step::{Key, Step};

/// Maximum number of VHS execution retries.
const MAX_VHS_RETRIES: u8 = 3;
/// Longest fixed fallback wait emitted for a predicate-only PTY step.
const MAX_VHS_EVENTUALLY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// Render-settle delay between discrete key events in VHS replays.
const VHS_KEY_SETTLE_WAIT_MS: u16 = 100;
/// Recorder implementation represented by committed feature sidecars.
///
/// Keep the VHS version aligned with `container/e2e.Containerfile` and bump
/// the schema suffix whenever poster extraction changes without changing the
/// compiled tape.
pub(crate) const VHS_RECORDER_FINGERPRINT: &str = "vhs@0.11.0;testty-tape-v1;final-frame-poster-v1";
/// Extra horizontal cell spacing that makes the feature preset exactly 80
/// columns.
const FEATURE_DEMO_LETTER_SPACING: f64 = 9.25;
/// Vertical cell multiplier that makes the feature preset exactly 24 rows.
const FEATURE_DEMO_LINE_HEIGHT: f64 = 1.35;

/// Configurable VHS tape rendering settings.
///
/// Controls the visual appearance of generated GIF recordings. Use
/// [`VhsTapeSettings::default()`] for compact proof GIFs or
/// [`VhsTapeSettings::feature_demo()`] for browser-ready feature
/// showcase recordings.
#[derive(Debug, Clone)]
pub struct VhsTapeSettings {
    /// Terminal width in pixels.
    pub width: u16,
    /// Terminal height in pixels.
    pub height: u16,
    /// Font size in points.
    pub font_size: u16,
    /// VHS theme name (e.g. `"OneDark"`, `"Dracula"`).
    pub theme: String,
    /// GIF framerate in frames per second.
    pub framerate: u16,
    /// Terminal padding in pixels.
    pub padding: u16,
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

impl VhsTapeSettings {
    /// Browser-ready preset for feature demo GIFs.
    ///
    /// Produces sharp 80×24 recordings at 1600×800, font size 18,
    /// `OneDark` theme, 20-pixel padding, and 30 fps.
    pub fn feature_demo() -> Self {
        Self {
            width: 1600,
            height: 800,
            font_size: 18,
            theme: "OneDark".to_string(),
            framerate: 30,
            padding: 20,
        }
    }

    /// Return xterm letter spacing for this rendering preset.
    pub(crate) fn letter_spacing(&self) -> f64 {
        if self.is_feature_demo() {
            FEATURE_DEMO_LETTER_SPACING
        } else {
            0.0
        }
    }

    /// Return xterm line height for this rendering preset.
    pub(crate) fn line_height(&self) -> f64 {
        if self.is_feature_demo() {
            FEATURE_DEMO_LINE_HEIGHT
        } else {
            1.0
        }
    }

    /// Return whether these settings are the browser-ready feature preset.
    fn is_feature_demo(&self) -> bool {
        self.width == 1600 && self.height == 800 && self.font_size == 18 && self.padding == 20
    }
}

/// A compiled VHS tape ready for writing and execution.
///
/// Generated from a [`Scenario`] with environment and binary configuration.
/// The tape uses VHS commands (`Set`, `Hide`, `Show`, `Type`, `Sleep`,
/// `Wait+Screen`, `Wait+Line`) to reproduce the scenario journey. After VHS
/// succeeds, the final visible GIF frame is decoded into the PNG poster.
pub struct VhsTape {
    /// The rendered tape content as VHS syntax.
    content: String,
    /// Path where VHS writes the animated recording.
    gif_path: PathBuf,
    /// Path where the screenshot will be saved.
    screenshot_path: PathBuf,
    /// VHS executable used for availability checks and recording.
    vhs_binary: PathBuf,
}

impl VhsTape {
    /// Compile a scenario into a VHS tape using default settings.
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
    /// Retries up to [`MAX_VHS_RETRIES`] times if the recording or poster is
    /// not produced.
    ///
    /// # Errors
    ///
    /// Returns an error if VHS is not installed, execution fails, or the
    /// screenshot is not produced after retries.
    pub fn execute(&self, tape_path: &Path) -> Result<PathBuf, VhsError> {
        check_vhs_installed_at(&self.vhs_binary)?;
        self.write_to(tape_path)
            .map_err(|err| VhsError::IoError(err.to_string()))?;

        let mut last_error = String::new();

        for attempt in 1..=MAX_VHS_RETRIES {
            remove_if_exists(&self.gif_path)?;
            remove_if_exists(&self.screenshot_path)?;

            let output = Command::new(&self.vhs_binary)
                .arg(tape_path)
                .output()
                .map_err(|err| VhsError::ExecutionFailed(err.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);

                return Err(VhsError::ExecutionFailed(format!(
                    "VHS exited with error: {stderr}"
                )));
            }

            if self.gif_path.is_file() {
                if let Err(error) = write_gif_poster(&self.gif_path, &self.screenshot_path) {
                    let gif_path = self.gif_path.display();
                    last_error = format!("Attempt {attempt}/{MAX_VHS_RETRIES}");
                    let _ = write!(
                        last_error,
                        ": could not extract poster from {gif_path}: {error}"
                    );

                    continue;
                }

                return Ok(self.screenshot_path.clone());
            }

            last_error = format!(
                "Attempt {attempt}/{MAX_VHS_RETRIES}: GIF not produced at {}",
                self.gif_path.display()
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
        let vhs_binary = PathBuf::from("vhs");

        Self {
            content,
            gif_path: gif_path.to_path_buf(),
            screenshot_path: screenshot_path.to_path_buf(),
            vhs_binary,
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

    /// VHS ran but did not produce a recording poster.
    #[error("Recording poster not produced: {0}")]
    ScreenshotNotProduced(String),

    /// I/O error writing or reading files.
    #[error("I/O error: {0}")]
    IoError(String),
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

    // Infallible: all `writeln!` calls below write to a String, which cannot fail.
    // Header settings.
    let _ = writeln!(tape, "Set Shell \"bash\"");
    let _ = writeln!(tape, "Set FontSize {}", settings.font_size);
    let _ = writeln!(tape, "Set Width {}", settings.width);
    let _ = writeln!(tape, "Set Height {}", settings.height);
    let _ = writeln!(tape, "Set Padding {}", settings.padding);
    let _ = writeln!(tape, "Set LetterSpacing {}", settings.letter_spacing());
    let _ = writeln!(tape, "Set LineHeight {}", settings.line_height());
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

    if let Some((_, workdir)) = env_vars.iter().find(|(key, _)| *key == "PWD") {
        let change_directory = format!("cd -- '{}'", escape_shell_single_quote(workdir));
        let _ = writeln!(
            tape,
            "Type \"{}\"",
            escape_vhs_double_quote(&change_directory)
        );
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
    let _ = writeln!(tape, "Sleep 200ms");
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
fn compile_step(tape: &mut String, step: &Step, _screenshot_path: &Path) {
    // Infallible: all `writeln!` calls below write to a String, which cannot fail.
    match step {
        Step::WriteText(text) => {
            let _ = writeln!(tape, "Type \"{}\"", escape_vhs_double_quote(text));
        }
        Step::PressKey(key) => {
            let vhs_key = key_to_vhs_command(key);
            let _ = writeln!(tape, "{vhs_key}");
            let _ = writeln!(tape, "Sleep {VHS_KEY_SETTLE_WAIT_MS}ms");
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
            // The pinned VHS release accepts `Screenshot` syntax without
            // writing a PNG. Keep the capture moment in the GIF, then extract
            // its final visible frame after recording succeeds.
            let _ = writeln!(tape, "Sleep {VHS_KEY_SETTLE_WAIT_MS}ms");
        }
        Step::Eventually { timeout, .. } => {
            // VHS recordings have no predicate-driven wait primitive, so
            // approximate `Eventually` with a bounded fixed `Sleep`.
            // Predicate timeouts are failure budgets, not intended viewing
            // pauses; replaying every full budget made long scenarios produce
            // oversized GIFs and could exhaust VHS/FFmpeg. Explicit
            // `ViewingPause` steps remain uncapped.
            let ms = (*timeout).min(MAX_VHS_EVENTUALLY_WAIT).as_millis();

            if ms >= 1000 && ms % 1000 == 0 {
                let _ = writeln!(tape, "Sleep {}s", ms / 1000);
            } else {
                let _ = writeln!(tape, "Sleep {ms}ms");
            }
        }
    }
}

/// Remove a prior recording artifact while treating absence as success.
fn remove_if_exists(path: &Path) -> Result<(), VhsError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VhsError::IoError(format!(
            "failed to remove {}: {error}",
            path.display()
        ))),
    }
}

/// Decode the last visible GIF frame and publish it as a PNG poster.
fn write_gif_poster(gif_path: &Path, poster_path: &Path) -> Result<(), VhsError> {
    let gif = File::open(gif_path).map_err(|error| {
        VhsError::IoError(format!("failed to open {}: {error}", gif_path.display()))
    })?;
    let decoder = GifDecoder::new(BufReader::new(gif)).map_err(|error| {
        VhsError::IoError(format!("failed to decode {}: {error}", gif_path.display()))
    })?;
    let mut final_frame = None;

    for frame in decoder.into_frames() {
        final_frame = Some(frame.map_err(|error| {
            VhsError::IoError(format!(
                "failed to decode a frame from {}: {error}",
                gif_path.display()
            ))
        })?);
    }

    let final_frame = require_final_frame(final_frame, gif_path)?;
    final_frame
        .into_buffer()
        .save_with_format(poster_path, image::ImageFormat::Png)
        .map_err(|error| {
            let poster_path = poster_path.display();

            VhsError::IoError(format!("failed to write poster {poster_path}: {error}"))
        })
}

/// Require a decoded final frame before poster publication.
fn require_final_frame(final_frame: Option<Frame>, gif_path: &Path) -> Result<Frame, VhsError> {
    final_frame
        .ok_or_else(|| VhsError::IoError(format!("GIF has no frames: {}", gif_path.display())))
}

/// Convert a key name to the corresponding VHS command.
fn key_to_vhs_command(key: &str) -> String {
    match Key::parse(key) {
        Key::AltEnter => "Alt+Enter".to_string(),
        Key::Enter => "Enter".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::BackTab => "Shift+Tab".to_string(),
        Key::Escape => "Escape".to_string(),
        Key::Backspace => "Backspace".to_string(),
        Key::Up => "Up".to_string(),
        Key::Down => "Down".to_string(),
        Key::Right => "Right".to_string(),
        Key::Left => "Left".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::Delete => "Delete".to_string(),
        Key::PageUp => "PageUp".to_string(),
        Key::PageDown => "PageDown".to_string(),
        Key::Space => "Space".to_string(),
        Key::Ctrl(character) => format!("Ctrl+{}", character.to_ascii_uppercase()),
        Key::Text(text) => format!("Type \"{}\"", escape_vhs_double_quote(&text)),
    }
}

/// Escape double quotes inside a string for use in VHS double-quoted
/// arguments (for example, `Type "..."`).
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

/// Verify one configured VHS executable can be launched.
fn check_vhs_installed_at(vhs_binary: &Path) -> Result<(), VhsError> {
    Command::new(vhs_binary)
        .arg("--version")
        .output()
        .map_err(|_| {
            VhsError::NotInstalled(
                "VHS is not installed. Install with: brew install vhs".to_string(),
            )
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[cfg(unix)]
    fn write_fake_vhs(path: &Path, action: &str) {
        let script =
            format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\n{action}\n");
        std::fs::write(path, script).expect("write fake VHS executable");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("make fake VHS executable");
    }

    fn write_valid_gif(path: &Path) {
        let gif = File::create(path).expect("create GIF");
        image::codecs::gif::GifEncoder::new(gif)
            .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                2,
                1,
                image::Rgba([0, 0, 255, 255]),
            )))
            .expect("encode GIF frame");
    }

    fn write_gif_with_corrupt_later_frame(path: &Path) {
        let first = image::Frame::from_parts(
            image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 255])),
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        let second = image::Frame::from_parts(
            image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 255, 255, 255])),
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        let mut encoded = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut encoded)
            .encode_frames([first, second])
            .expect("encode GIF frames");
        let second_control_extension = encoded
            .windows(3)
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == [0x21, 0xf9, 0x04]).then_some(index))
            .nth(1)
            .expect("find second frame control extension");
        encoded.truncate(second_control_extension + 5);
        std::fs::write(path, encoded).expect("write GIF with corrupt later frame");
    }

    #[test]
    fn compile_tape_includes_header_settings() {
        // Arrange
        let scenario = Scenario::new("test").sleep_ms(100).capture();
        let settings = VhsTapeSettings::default();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[],
            &settings,
        );

        // Assert
        assert!(tape.contains("Set Shell \"bash\""));
        assert!(tape.contains(&format!("Set FontSize {}", settings.font_size)));
        assert!(tape.contains(&format!("Set Width {}", settings.width)));
        assert!(tape.contains("Set Padding 0"));
        assert!(tape.contains("Set LetterSpacing 0"));
        assert!(tape.contains("Set LineHeight 1"));
    }

    #[test]
    fn compile_tape_includes_env_vars() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[("AGENTTY_ROOT", "/tmp/root")],
            &VhsTapeSettings::default(),
        );

        // Assert
        assert!(tape.contains("export AGENTTY_ROOT='/tmp/root'"));
    }

    #[test]
    fn compile_tape_capture_keeps_the_final_frame_visible() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[],
            &VhsTapeSettings::default(),
        );

        // Assert
        assert!(tape.contains("Show\n\nSleep 100ms"));
        assert!(!tape.contains("Screenshot"));
    }

    #[test]
    fn key_to_vhs_command_maps_common_keys() {
        // Arrange / Act / Assert
        assert_eq!(key_to_vhs_command("Enter"), "Enter");
        assert_eq!(key_to_vhs_command("Alt+Enter"), "Alt+Enter");
        assert_eq!(key_to_vhs_command("tab"), "Tab");
        assert_eq!(key_to_vhs_command("escape"), "Escape");
        assert_eq!(key_to_vhs_command("backspace"), "Backspace");
        assert_eq!(key_to_vhs_command("up"), "Up");
        assert_eq!(key_to_vhs_command("down"), "Down");
        assert_eq!(key_to_vhs_command("right"), "Right");
        assert_eq!(key_to_vhs_command("left"), "Left");
        assert_eq!(key_to_vhs_command("pageup"), "PageUp");
        assert_eq!(key_to_vhs_command("pagedown"), "PageDown");
        assert_eq!(key_to_vhs_command("space"), "Space");
        assert_eq!(key_to_vhs_command("ctrl+c"), "Ctrl+C");
        assert_eq!(key_to_vhs_command("BackTab"), "Shift+Tab");
        assert_eq!(key_to_vhs_command("Home"), "Home");
        assert_eq!(key_to_vhs_command("End"), "End");
        assert_eq!(key_to_vhs_command("Delete"), "Delete");
        assert_eq!(key_to_vhs_command("BackTabb"), "Type \"BackTabb\"");
    }

    #[test]
    fn compile_step_waits_between_key_events() {
        // Arrange
        let step = Step::press_key("Enter");
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert_eq!(tape, "Enter\nSleep 100ms\n");
    }

    #[test]
    fn compile_tape_enters_the_pty_working_directory() {
        // Arrange
        let scenario = Scenario::new("working-directory");
        let env_vars = [("PWD", "/tmp/test-project")];

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/tmp/app"),
            Path::new("/tmp/demo.gif"),
            Path::new("/tmp/demo.png"),
            &env_vars,
            &VhsTapeSettings::default(),
        );

        // Assert
        assert!(tape.contains("Type \"cd -- '/tmp/test-project'\"\nEnter"));
    }

    #[test]
    fn feature_demo_emits_canonical_terminal_geometry_settings() {
        // Arrange
        let scenario = Scenario::new("feature-geometry");

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/tmp/app"),
            Path::new("/tmp/demo.gif"),
            Path::new("/tmp/demo.png"),
            &[],
            &VhsTapeSettings::feature_demo(),
        );

        // Assert
        assert!(tape.contains("Set Padding 20"));
        assert!(tape.contains("Set LetterSpacing 9.25"));
        assert!(tape.contains("Set LineHeight 1.35"));
        assert!(tape.contains("Enter\nSleep 200ms\nShow"));
        assert!(!tape.contains("Enter\nSleep 2s\nShow"));
    }

    #[test]
    fn compile_step_wait_for_text_emits_wait_screen_with_regex() {
        // Arrange
        let step = Step::wait_for_text("Loading", 5000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Wait+Screen@5s /Loading/"));
    }

    #[test]
    fn compile_step_wait_for_text_formats_fractional_timeout_as_milliseconds() {
        // Arrange
        let step = Step::wait_for_text("Startup", 1500);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Wait+Screen@1500ms /Startup/"));
    }

    #[test]
    fn compile_step_wait_for_text_escapes_regex_metacharacters() {
        // Arrange
        let step = Step::wait_for_text("[test] foo.bar", 3000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert — brackets and dot are escaped.
        assert!(tape.contains(r"Wait+Screen@3s /\[test\] foo\.bar/"));
    }

    #[test]
    fn compile_step_viewing_pause_emits_sleep_seconds() {
        // Arrange
        let step = Step::viewing_pause_ms(2000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 2s"));
    }

    #[test]
    fn compile_step_viewing_pause_emits_sleep_milliseconds() {
        // Arrange
        let step = Step::viewing_pause_ms(1500);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 1500ms"));
    }

    /// Verifies long predicate budgets use the bounded VHS fallback wait.
    #[test]
    fn compile_step_eventually_caps_long_timeout() {
        // Arrange
        let step = Step::eventually(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            |_frame| Ok(()),
        );
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(
            tape.contains("Sleep 2s"),
            "expected capped Sleep 2s fallback, got: {tape}"
        );
    }

    /// Verifies short fractional predicate budgets stay unchanged.
    #[test]
    fn compile_step_eventually_emits_sleep_milliseconds_for_fractional_timeout() {
        // Arrange
        let step = Step::eventually(
            std::time::Duration::from_millis(750),
            std::time::Duration::from_millis(25),
            |_frame| Ok(()),
        );
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(
            tape.contains("Sleep 750ms"),
            "expected Sleep 750ms fallback, got: {tape}"
        );
    }

    #[test]
    fn compile_step_sleep_uses_seconds_when_even() {
        // Arrange
        let step = Step::sleep_ms(3000);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 3s"));
    }

    #[test]
    fn compile_step_sleep_uses_milliseconds_when_fractional() {
        // Arrange
        let step = Step::sleep_ms(500);
        let mut tape = String::new();

        // Act
        compile_step(&mut tape, &step, Path::new("/tmp/shot.png"));

        // Assert
        assert!(tape.contains("Sleep 500ms"));
    }

    #[test]
    fn escape_vhs_double_quote_escapes_quotes_and_backslashes() {
        // Arrange / Act / Assert
        assert_eq!(escape_vhs_double_quote(r#"hello"world"#), r#"hello\"world"#);
        assert_eq!(escape_vhs_double_quote(r"back\slash"), r"back\\slash");
        assert_eq!(escape_vhs_double_quote("clean"), "clean");
    }

    #[test]
    fn escape_shell_single_quote_wraps_internal_quotes() {
        // Arrange / Act / Assert
        assert_eq!(escape_shell_single_quote("it's"), "it'\\''s");
        assert_eq!(escape_shell_single_quote("clean"), "clean");
    }

    #[test]
    fn escape_vhs_regex_escapes_metacharacters() {
        // Arrange / Act / Assert
        assert_eq!(escape_vhs_regex("plain"), "plain");
        assert_eq!(escape_vhs_regex("a.b"), r"a\.b");
        assert_eq!(escape_vhs_regex("[x]"), r"\[x\]");
        assert_eq!(escape_vhs_regex("a/b"), r"a\/b");
        assert_eq!(escape_vhs_regex("a+b*c?"), r"a\+b\*c\?");
        assert_eq!(escape_vhs_regex(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn format_vhs_duration_uses_seconds_for_even_multiples() {
        // Arrange / Act / Assert
        assert_eq!(format_vhs_duration(1000), "1s");
        assert_eq!(format_vhs_duration(5000), "5s");
        assert_eq!(format_vhs_duration(30000), "30s");
    }

    #[test]
    fn format_vhs_duration_uses_milliseconds_for_fractional() {
        // Arrange / Act / Assert
        assert_eq!(format_vhs_duration(500), "500ms");
        assert_eq!(format_vhs_duration(1500), "1500ms");
        assert_eq!(format_vhs_duration(100), "100ms");
    }

    #[test]
    fn compile_tape_escapes_env_value_with_single_quote() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[("KEY", "it's a value")],
            &VhsTapeSettings::default(),
        );

        // Assert — the single quote is shell-escaped to '\'' and the
        // backslash is then VHS-double-quote-escaped to '\\', giving '\\''
        // in the final tape string.
        assert!(tape.contains(r"it'\\''s a value"));
    }

    #[test]
    fn compile_tape_shell_quotes_binary_path() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[],
            &VhsTapeSettings::default(),
        );

        // Assert — binary path is wrapped in single quotes for the shell.
        assert!(tape.contains("Type \"'/usr/bin/echo'\""));
    }

    #[test]
    fn compile_tape_clears_terminal_and_launches_binary_before_show() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[("KEY", "val")],
            &VhsTapeSettings::default(),
        );

        // Assert — clear and binary launch happen inside the Hide section,
        // and Show comes after all of them.
        let hide_pos = tape.find("Hide").expect("tape must contain Hide");
        let clear_pos = tape
            .find("Type \"clear\"")
            .expect("tape must contain clear");
        let binary_pos = tape
            .find("Type \"'/usr/bin/echo'\"")
            .expect("tape must contain binary launch");
        let show_pos = tape.find("Show").expect("tape must contain Show");

        assert!(hide_pos < clear_pos, "Hide must precede clear");
        assert!(clear_pos < binary_pos, "clear must precede binary launch");
        assert!(binary_pos < show_pos, "binary launch must precede Show");
    }

    #[test]
    fn compile_tape_shell_quotes_binary_path_with_spaces() {
        // Arrange
        let scenario = Scenario::new("test").capture();

        // Act
        let tape = compile_tape(
            &scenario,
            Path::new("/path with spaces/bin"),
            Path::new("/tmp/shot.gif"),
            Path::new("/tmp/shot.png"),
            &[],
            &VhsTapeSettings::default(),
        );

        // Assert — spaces are safe inside single quotes.
        assert!(tape.contains("Type \"'/path with spaces/bin'\""));
    }

    #[test]
    fn feature_demo_settings_have_expected_values() {
        // Arrange / Act
        let settings = VhsTapeSettings::feature_demo();

        // Assert
        assert_eq!(settings.width, 1600);
        assert_eq!(settings.height, 800);
        assert_eq!(settings.font_size, 18);
        assert_eq!(settings.theme, "OneDark");
        assert_eq!(settings.framerate, 30);
        assert_eq!(settings.padding, 20);
        assert!((settings.letter_spacing() - 9.25).abs() < f64::EPSILON);
        assert!((settings.line_height() - 1.35).abs() < f64::EPSILON);
    }

    #[test]
    fn feature_demo_geometry_survives_theme_and_framerate_customization() {
        // Arrange
        let mut settings = VhsTapeSettings::feature_demo();
        settings.theme = "Dracula".to_string();
        settings.framerate = 24;

        // Act / Assert
        assert!((settings.letter_spacing() - 9.25).abs() < f64::EPSILON);
        assert!((settings.line_height() - 1.35).abs() < f64::EPSILON);
    }

    #[test]
    fn default_settings_match_legacy_constants() {
        // Arrange / Act
        let settings = VhsTapeSettings::default();

        // Assert
        assert_eq!(settings.width, 1200);
        assert_eq!(settings.height, 600);
        assert_eq!(settings.font_size, 14);
        assert_eq!(settings.theme, "");
        assert_eq!(settings.framerate, 0);
        assert_eq!(settings.padding, 0);
    }

    #[test]
    fn from_scenario_with_settings_applies_feature_demo() {
        // Arrange
        let scenario = Scenario::new("feature_test").sleep_ms(100).capture();
        let settings = VhsTapeSettings::feature_demo();

        // Act
        let tape = VhsTape::from_scenario_with_settings(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
            &settings,
        );

        // Assert
        let content = tape.render();
        assert!(content.contains("Set FontSize 18"));
        assert!(content.contains("Set Width 1600"));
        assert!(content.contains("Set Height 800"));
        assert!(content.contains("Set Theme \"OneDark\""));
        assert!(content.contains("Set Framerate 30"));
    }

    #[test]
    fn from_scenario_with_output_path_separates_gif_and_screenshot() {
        // Arrange
        let scenario = Scenario::new("separate_paths").capture();
        let settings = VhsTapeSettings::feature_demo();
        let gif_path = Path::new("/tmp/feature.gif");
        let screenshot_path = Path::new("/tmp/.feature.capture.png");

        // Act
        let tape = VhsTape::from_scenario_with_output_path(
            &scenario,
            Path::new("/usr/bin/echo"),
            gif_path,
            screenshot_path,
            &[],
            &settings,
        );

        // Assert
        assert!(tape.render().contains("Output \"/tmp/feature.gif\""));
        assert!(!tape.render().contains("Screenshot"));
        assert_eq!(tape.screenshot_path(), screenshot_path);
    }

    #[cfg(unix)]
    #[test]
    fn execute_returns_poster_from_fake_vhs_recording() {
        // Arrange
        let temp = tempfile::tempdir().expect("create VHS test directory");
        let gif_path = temp.path().join("recording.gif");
        let poster_path = temp.path().join("poster.png");
        let tape_path = temp.path().join("recording.tape");
        let source_gif_path = temp.path().join("source.gif");
        let fake_vhs_path = temp.path().join("vhs");
        write_valid_gif(&source_gif_path);
        write_fake_vhs(
            &fake_vhs_path,
            &format!(
                "cp '{}' '{}'",
                source_gif_path.display(),
                gif_path.display()
            ),
        );
        let scenario = Scenario::new("execute_success").capture();
        let mut tape = VhsTape::from_scenario_with_output_path(
            &scenario,
            Path::new("/bin/true"),
            &gif_path,
            &poster_path,
            &[],
            &VhsTapeSettings::default(),
        );
        tape.vhs_binary = fake_vhs_path;

        // Act
        let result = tape.execute(&tape_path).expect("execute fake VHS");

        // Assert
        assert_eq!(result, poster_path);
        assert!(image::open(result).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn execute_reports_missing_gif_after_retries() {
        // Arrange
        let temp = tempfile::tempdir().expect("create VHS test directory");
        let gif_path = temp.path().join("missing.gif");
        let poster_path = temp.path().join("poster.png");
        let tape_path = temp.path().join("recording.tape");
        let fake_vhs_path = temp.path().join("vhs");
        write_fake_vhs(&fake_vhs_path, "exit 0");
        let scenario = Scenario::new("execute_missing").capture();
        let mut tape = VhsTape::from_scenario_with_output_path(
            &scenario,
            Path::new("/bin/true"),
            &gif_path,
            &poster_path,
            &[],
            &VhsTapeSettings::default(),
        );
        tape.vhs_binary = fake_vhs_path;

        // Act
        let error = tape
            .execute(&tape_path)
            .expect_err("missing GIF should exhaust retries");

        // Assert
        assert!(matches!(
            error,
            VhsError::ScreenshotNotProduced(ref message)
                if message.contains("Attempt 3/3") && message.contains("GIF not produced")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn execute_reports_invalid_gif_after_retries() {
        // Arrange
        let temp = tempfile::tempdir().expect("create VHS test directory");
        let gif_path = temp.path().join("invalid.gif");
        let poster_path = temp.path().join("poster.png");
        let tape_path = temp.path().join("recording.tape");
        let fake_vhs_path = temp.path().join("vhs");
        write_fake_vhs(
            &fake_vhs_path,
            &format!("printf 'not-a-gif' > '{}'", gif_path.display()),
        );
        let scenario = Scenario::new("execute_invalid").capture();
        let mut tape = VhsTape::from_scenario_with_output_path(
            &scenario,
            Path::new("/bin/true"),
            &gif_path,
            &poster_path,
            &[],
            &VhsTapeSettings::default(),
        );
        tape.vhs_binary = fake_vhs_path;

        // Act
        let error = tape
            .execute(&tape_path)
            .expect_err("invalid GIF should exhaust retries");

        // Assert
        assert!(matches!(
            error,
            VhsError::ScreenshotNotProduced(ref message)
                if message.contains("Attempt 3/3")
                    && message.contains("could not extract poster")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn execute_reports_recorder_failure() {
        // Arrange
        let temp = tempfile::tempdir().expect("create VHS test directory");
        let fake_vhs_path = temp.path().join("vhs");
        write_fake_vhs(&fake_vhs_path, "echo recorder-failed >&2; exit 2");
        let scenario = Scenario::new("execute_failure").capture();
        let mut tape = VhsTape::from_scenario(
            &scenario,
            Path::new("/bin/true"),
            &temp.path().join("poster.png"),
            &[],
        );
        tape.vhs_binary = fake_vhs_path;

        // Act
        let error = tape
            .execute(&temp.path().join("recording.tape"))
            .expect_err("recorder failure should be returned");

        // Assert
        assert!(matches!(
            error,
            VhsError::ExecutionFailed(ref message) if message.contains("recorder-failed")
        ));
    }

    #[test]
    fn execute_reports_missing_configured_recorder() {
        // Arrange
        let temp = tempfile::tempdir().expect("create VHS test directory");
        let scenario = Scenario::new("missing_recorder").capture();
        let mut tape = VhsTape::from_scenario(
            &scenario,
            Path::new("/bin/true"),
            &temp.path().join("poster.png"),
            &[],
        );
        tape.vhs_binary = temp.path().join("missing-vhs");

        // Act
        let error = tape
            .execute(&temp.path().join("recording.tape"))
            .expect_err("missing recorder should be returned");

        // Assert
        assert!(matches!(error, VhsError::NotInstalled(_)));
    }

    #[test]
    fn remove_if_exists_handles_present_missing_and_invalid_targets() {
        // Arrange
        let temp = tempfile::tempdir().expect("create removal test directory");
        let file_path = temp.path().join("recording.gif");
        let missing_path = temp.path().join("missing.gif");
        let directory_path = temp.path().join("poster.png");
        std::fs::write(&file_path, b"gif").expect("write recording file");
        std::fs::create_dir(&directory_path).expect("create invalid poster directory");

        // Act
        let present_result = remove_if_exists(&file_path);
        let missing_result = remove_if_exists(&missing_path);
        let invalid_error =
            remove_if_exists(&directory_path).expect_err("directory should fail file removal");

        // Assert
        assert!(present_result.is_ok());
        assert!(missing_result.is_ok());
        assert!(invalid_error.to_string().contains("failed to remove"));
        assert!(invalid_error.to_string().contains("poster.png"));
    }

    #[test]
    fn write_gif_poster_reports_missing_and_invalid_gifs() {
        // Arrange
        let temp = tempfile::tempdir().expect("create poster test directory");
        let missing_gif_path = temp.path().join("missing.gif");
        let invalid_gif_path = temp.path().join("invalid.gif");
        let poster_path = temp.path().join("poster.png");
        std::fs::write(&invalid_gif_path, b"not-a-gif").expect("write invalid GIF");

        // Act
        let missing_error =
            write_gif_poster(&missing_gif_path, &poster_path).expect_err("missing GIF should fail");
        let invalid_error =
            write_gif_poster(&invalid_gif_path, &poster_path).expect_err("invalid GIF should fail");

        // Assert
        assert!(missing_error.to_string().contains("failed to open"));
        assert!(invalid_error.to_string().contains("failed to decode"));
    }

    #[test]
    fn write_gif_poster_reports_corrupt_later_frame() {
        // Arrange
        let temp = tempfile::tempdir().expect("create poster test directory");
        let gif_path = temp.path().join("corrupt.gif");
        let poster_path = temp.path().join("poster.png");
        write_gif_with_corrupt_later_frame(&gif_path);

        // Act
        let error = write_gif_poster(&gif_path, &poster_path)
            .expect_err("later-frame corruption should fail");

        // Assert
        assert!(error.to_string().contains("failed to decode a frame"));
    }

    #[test]
    fn require_final_frame_reports_empty_animation() {
        // Arrange
        let gif_path = Path::new("empty.gif");

        // Act
        let error = require_final_frame(None, gif_path)
            .err()
            .expect("frame-less GIF should fail");

        // Assert
        assert!(
            error.to_string().contains("GIF has no frames"),
            "unexpected empty GIF error: {error}"
        );
    }

    #[test]
    fn write_gif_poster_reports_poster_write_failure() {
        // Arrange
        let temp = tempfile::tempdir().expect("create poster test directory");
        let gif_path = temp.path().join("recording.gif");
        let poster_path = temp.path().join("poster.png");
        write_valid_gif(&gif_path);
        std::fs::create_dir(&poster_path).expect("create conflicting poster directory");

        // Act
        let error = write_gif_poster(&gif_path, &poster_path)
            .expect_err("poster directory should fail PNG write");

        // Assert
        assert!(error.to_string().contains("failed to write poster"));
        assert!(error.to_string().contains("poster.png"));
    }

    #[test]
    fn write_gif_poster_uses_the_final_animation_frame() {
        // Arrange
        let temp = tempfile::tempdir().expect("create poster test directory");
        let gif_path = temp.path().join("recording.gif");
        let poster_path = temp.path().join("poster.png");
        let gif_file = File::create(&gif_path).expect("create GIF");
        let mut encoder = image::codecs::gif::GifEncoder::new(gif_file);
        encoder
            .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                2,
                1,
                image::Rgba([255, 0, 0, 255]),
            )))
            .expect("encode first frame");
        encoder
            .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                2,
                1,
                image::Rgba([0, 0, 255, 255]),
            )))
            .expect("encode final frame");
        drop(encoder);

        // Act
        write_gif_poster(&gif_path, &poster_path).expect("extract poster");

        // Assert
        let poster = image::open(&poster_path).expect("open poster").into_rgba8();
        assert_eq!(poster.dimensions(), (2, 1));
        assert_eq!(poster.get_pixel(0, 0), &image::Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn from_scenario_with_settings_omits_empty_theme() {
        // Arrange
        let scenario = Scenario::new("no_theme").capture();
        let settings = VhsTapeSettings::default();

        // Act
        let tape = VhsTape::from_scenario_with_settings(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
            &settings,
        );

        // Assert — default settings have empty theme, so no Theme line.
        let content = tape.render();
        assert!(!content.contains("Set Theme"));
    }

    #[test]
    fn from_scenario_with_settings_omits_zero_framerate() {
        // Arrange
        let scenario = Scenario::new("no_framerate").capture();
        let settings = VhsTapeSettings::default();

        // Act
        let tape = VhsTape::from_scenario_with_settings(
            &scenario,
            Path::new("/usr/bin/echo"),
            Path::new("/tmp/shot.png"),
            &[],
            &settings,
        );

        // Assert — default settings have framerate 0, so no Framerate line.
        let content = tape.render();
        assert!(!content.contains("Set Framerate"));
    }
}

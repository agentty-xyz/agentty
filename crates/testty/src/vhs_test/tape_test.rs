use std::path::Path;

use super::super::compile_tape;
use crate::scenario::Scenario;
use crate::vhs::{VhsTape, VhsTapeSettings};

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
fn compile_tape_includes_screenshot() {
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
    assert!(tape.contains("Screenshot \"/tmp/shot.png\""));
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
    assert_eq!(settings.padding, 0);
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
    assert!(
        tape.render()
            .contains("Screenshot \"/tmp/.feature.capture.png\"")
    );
    assert_eq!(tape.screenshot_path(), screenshot_path);
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

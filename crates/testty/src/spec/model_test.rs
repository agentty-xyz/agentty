use std::path::PathBuf;

use crate::spec::model::{ExpectSpec, SUPPORTED_VERSION, ScenarioSpec, StepSpec};

#[test]
fn deserializes_press_key_step() {
    // Arrange
    let yaml = "
session:
  bin: ./app
steps:
  - press_key: Tab
";

    // Act
    let spec: ScenarioSpec = serde_yaml_ng::from_str(yaml).expect("parse");

    // Assert
    assert_eq!(spec.version, SUPPORTED_VERSION);
    assert_eq!(spec.session.bin, PathBuf::from("./app"));
    assert_eq!(spec.steps.len(), 1);
    assert!(matches!(&spec.steps[0], StepSpec::PressKey(key) if key == "Tab"));
}

#[test]
fn deserializes_session_size_args_and_expect() {
    // Arrange
    let yaml = "
version: 1
session:
  bin: ./app
  size: [80, 24]
  args: [--flag, value]
steps:
  - write_text: hello
  - wait_for_stable_frame: { stable_ms: 500, timeout_ms: 5000 }
expect:
  - selected_tab: Sessions
  - text_in_region: { text: \"Counter: 3\", region: [0, 0, 80, 24] }
";

    // Act
    let spec: ScenarioSpec = serde_yaml_ng::from_str(yaml).expect("parse");

    // Assert
    assert_eq!(spec.session.size, Some([80, 24]));
    assert_eq!(spec.session.args, vec!["--flag", "value"]);
    assert!(matches!(&spec.steps[0], StepSpec::WriteText(text) if text == "hello"));
    assert!(matches!(
        spec.steps[1],
        StepSpec::WaitForStableFrame {
            stable_ms: 500,
            timeout_ms: 5000
        }
    ));
    assert!(matches!(&spec.expect[0], ExpectSpec::SelectedTab(tab) if tab == "Sessions"));
    assert!(matches!(
        &spec.expect[1],
        ExpectSpec::TextInRegion { text, region }
            if text == "Counter: 3" && (region.0, region.1, region.2, region.3) == (0, 0, 80, 24)
    ));
}

#[test]
fn deserializes_bare_capture_step() {
    // Arrange
    let yaml = "
session:
  bin: ./app
steps:
  - capture
";

    // Act
    let spec: ScenarioSpec = serde_yaml_ng::from_str(yaml).expect("parse");

    // Assert
    assert!(matches!(spec.steps[0], StepSpec::Capture));
}

#[test]
fn deserializes_eventually_step_with_nested_matcher() {
    // Arrange
    let yaml = "
session:
  bin: ./app
steps:
  - eventually:
      match: { not_visible: Loading }
      timeout_ms: 3000
      poll_ms: 50
";

    // Act
    let spec: ScenarioSpec = serde_yaml_ng::from_str(yaml).expect("parse");

    // Assert
    assert!(matches!(
        &spec.steps[0],
        StepSpec::Eventually { matcher, timeout_ms: 3000, poll_ms: 50 }
            if matches!(matcher, ExpectSpec::NotVisible(text) if text == "Loading")
    ));
}

#[test]
fn rejects_unknown_top_level_field() {
    // Arrange — `step` (singular) is a typo for `steps`.
    let yaml = "
session:
  bin: ./app
step:
  - press_key: Tab
";

    // Act
    let result: Result<ScenarioSpec, _> = serde_yaml_ng::from_str(yaml);

    // Assert
    assert!(result.is_err());
}

#[test]
fn rejects_unknown_step_key() {
    // Arrange
    let yaml = "
session:
  bin: ./app
steps:
  - press_buttn: Tab
";

    // Act
    let result: Result<ScenarioSpec, _> = serde_yaml_ng::from_str(yaml);

    // Assert
    assert!(result.is_err());
}

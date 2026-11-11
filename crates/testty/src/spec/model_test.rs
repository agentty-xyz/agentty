use std::path::PathBuf;

use crate::spec::model::{ExpectSpec, RegionSpec, SUPPORTED_VERSION, ScenarioSpec, StepSpec};

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
fn deserializes_every_step_and_expectation_kind() {
    // Arrange
    let yaml = "
session:
  bin: ./app
steps:
  - press_key: Tab
  - write_text: hi
  - sleep_ms: 25
  - wait_for_text: { needle: Ready, timeout_ms: 500 }
  - wait_for_stable_frame: { stable_ms: 100, timeout_ms: 900 }
  - eventually: { match: { status_message: Done }, timeout_ms: 300, poll_ms: 10 }
  - capture
  - capture_labeled: { label: final, description: Final screen }
expect:
  - selected_tab: One
  - unselected_tab: Two
  - instruction_visible: Press Enter
  - keybinding_hint: q quit
  - footer_action: Save
  - dialog_title: Confirm
  - status_message: Saved
  - not_visible: Error
  - text_in_region: { text: Hi, region: [1, 2, 3, 4] }
";
    let expected_steps = vec![
        StepSpec::PressKey("Tab".to_string()),
        StepSpec::WriteText("hi".to_string()),
        StepSpec::SleepMs(25),
        StepSpec::WaitForText {
            needle: "Ready".to_string(),
            timeout_ms: 500,
        },
        StepSpec::WaitForStableFrame {
            stable_ms: 100,
            timeout_ms: 900,
        },
        StepSpec::Eventually {
            matcher: ExpectSpec::StatusMessage("Done".to_string()),
            timeout_ms: 300,
            poll_ms: 10,
        },
        StepSpec::Capture,
        StepSpec::CaptureLabeled {
            label: "final".to_string(),
            description: "Final screen".to_string(),
        },
    ];
    let expected_expectations = vec![
        ExpectSpec::SelectedTab("One".to_string()),
        ExpectSpec::UnselectedTab("Two".to_string()),
        ExpectSpec::InstructionVisible("Press Enter".to_string()),
        ExpectSpec::KeybindingHint("q quit".to_string()),
        ExpectSpec::FooterAction("Save".to_string()),
        ExpectSpec::DialogTitle("Confirm".to_string()),
        ExpectSpec::StatusMessage("Saved".to_string()),
        ExpectSpec::NotVisible("Error".to_string()),
        ExpectSpec::TextInRegion {
            text: "Hi".to_string(),
            region: RegionSpec(1, 2, 3, 4),
        },
    ];

    // Act
    let spec: ScenarioSpec = serde_yaml_ng::from_str(yaml).expect("parse");

    // Assert
    assert_eq!(format!("{:?}", spec.steps), format!("{expected_steps:?}"));
    assert_eq!(
        format!("{:?}", spec.expect),
        format!("{expected_expectations:?}")
    );
}

#[test]
fn rejects_malformed_steps_and_expectations() {
    // Arrange
    let cases = [
        ("steps: [jump]", "unknown step `jump`"),
        ("steps: [{}]", "a step map must have one key"),
        (
            "steps: [{ press_key: Tab, write_text: x }]",
            "a step must have exactly one key",
        ),
        (
            "steps: [42]",
            "expected a step: `capture` or a single-key map",
        ),
        ("expect: [{}]", "an expectation map must have one key"),
        ("expect: [{ bogus: x }]", "unknown expectation `bogus`"),
        (
            "expect: [{ not_visible: x, footer_action: y }]",
            "an expectation must have exactly one key",
        ),
        ("expect: [Sessions]", "expected an expectation map such as"),
    ];

    for (body, message) in cases {
        let yaml = format!("session:\n  bin: ./app\n{body}\n");

        // Act
        let error = serde_yaml_ng::from_str::<ScenarioSpec>(&yaml)
            .expect_err("malformed scenario must be rejected");

        // Assert
        assert!(
            error.to_string().contains(message),
            "`{body}` produced `{error}`, expected `{message}`"
        );
    }
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

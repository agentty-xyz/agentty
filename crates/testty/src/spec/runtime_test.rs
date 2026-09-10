#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::evaluate;
use crate::frame::TerminalFrame;
use crate::region::Region;
use crate::scenario::Scenario;
use crate::session::PtySessionBuilder;
use crate::spec::model::{ExpectSpec, RegionSpec, ScenarioSpec};
use crate::spec::runtime::SpecError;
use crate::{assertion, recipe};

#[test]
fn from_yaml_rejects_unsupported_version() {
    // Arrange
    let yaml = "
version: 999
session:
  bin: ./app
";

    // Act
    let result = ScenarioSpec::from_yaml(yaml);

    // Assert
    assert!(matches!(
        result,
        Err(SpecError::UnsupportedVersion {
            found: 999,
            supported: 1
        })
    ));
}

#[test]
fn from_yaml_accepts_supported_version() {
    // Arrange
    let yaml = "
version: 1
session:
  bin: ./app
steps:
  - press_key: Tab
";

    // Act
    let spec = ScenarioSpec::from_yaml(yaml).expect("supported version parses");

    // Assert
    assert_eq!(spec.steps.len(), 1);
}

#[test]
fn evaluate_text_in_region_passes_when_present() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let spec = ExpectSpec::TextInRegion {
        text: "World".to_string(),
        region: RegionSpec(0, 0, 80, 1),
    };

    // Act
    let result = evaluate(&spec, &frame);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn evaluate_not_visible_fails_when_text_present() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let spec = ExpectSpec::NotVisible("Hello".to_string());

    // Act
    let result = evaluate(&spec, &frame);

    // Assert — "Hello" is visible, so "not visible" fails.
    assert!(result.is_err());
}

#[test]
fn check_collects_only_failing_expectations() {
    // Arrange — one passing, one failing expectation.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let spec = ScenarioSpec {
        version: 1,
        name: None,
        session: crate::spec::model::SessionSpec {
            bin: "./app".into(),
            size: None,
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            workdir: None,
        },
        steps: Vec::new(),
        expect: vec![
            ExpectSpec::TextInRegion {
                text: "World".to_string(),
                region: RegionSpec(0, 0, 80, 1),
            },
            ExpectSpec::NotVisible("Hello".to_string()),
        ],
    };

    // Act
    let lowered = spec.lower();
    let failures = lowered.check(&frame);

    // Assert — only the NotVisible("Hello") expectation fails.
    assert_eq!(failures.len(), 1);
}

/// A YAML scenario lowered and run must produce the same outcome as the
/// hand-written code-API equivalent driving the same binary. This proves
/// the lowering is behavior-preserving end to end.
#[cfg(unix)]
#[test]
fn yaml_scenario_matches_code_api_against_same_binary() {
    // Arrange — a fixture that renders deterministic text and stays alive
    // so the PTY does not close before the frame is captured.
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let script = temp_dir.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750))
        .expect("set permissions");

    // Act — YAML path: parse -> lower -> run.
    let yaml = format!(
        "
session:
  bin: {bin}
  size: [80, 24]
steps:
  - wait_for_stable_frame: {{ stable_ms: 300, timeout_ms: 5000 }}
expect:
  - text_in_region: {{ text: \"Hello World\", region: [0, 0, 80, 1] }}
  - not_visible: Goodbye
",
        bin = script.display()
    );
    let (yaml_frame, yaml_failures) = ScenarioSpec::from_yaml(&yaml)
        .expect("parse")
        .lower()
        .run()
        .expect("yaml run");

    // Act — code path: the same scenario built and asserted by hand.
    let builder = PtySessionBuilder::new(&script).size(80, 24);
    let code_frame = Scenario::new("code")
        .wait_for_stable_frame(300, 5000)
        .run(builder)
        .expect("code run");
    let region = Region::new(0, 0, 80, 1);
    let code_text_ok = assertion::match_text_in_region(&code_frame, "Hello World", &region).is_ok();
    let code_not_visible_ok = recipe::match_not_visible(&code_frame, "Goodbye").is_ok();

    // Assert — both paths pass, and the rendered frames match.
    assert!(yaml_failures.is_empty(), "yaml expectations should pass");
    assert!(
        code_text_ok && code_not_visible_ok,
        "code expectations should pass"
    );
    assert_eq!(
        yaml_frame.all_text(),
        code_frame.all_text(),
        "lowered YAML scenario must render the same frame as the code API"
    );
}

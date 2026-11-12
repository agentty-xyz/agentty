#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::evaluate;
use crate::frame::TerminalFrame;
use crate::region::Region;
use crate::scenario::Scenario;
use crate::session::PtySessionBuilder;
use crate::spec::model::{ExpectSpec, RegionSpec, ScenarioSpec};
use crate::spec::runtime::SpecError;
use crate::step::Step;
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
fn evaluate_passes_every_expectation_kind_found_in_its_region() {
    // Arrange — a bold selected tab and a plain tab in the header, a dialog
    // title and messages in the body, and hints in the footer.
    let mut data = b"\x1b[1mOne\x1b[0m Two\r\nConfirm\r\nPress Enter\r\nSaved".to_vec();
    for _ in 0..6 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"q quit Save");
    let frame = TerminalFrame::new(40, 10, &data);
    let specs = [
        ExpectSpec::SelectedTab("One".to_string()),
        ExpectSpec::UnselectedTab("Two".to_string()),
        ExpectSpec::InstructionVisible("Press Enter".to_string()),
        ExpectSpec::KeybindingHint("q quit".to_string()),
        ExpectSpec::FooterAction("Save".to_string()),
        ExpectSpec::DialogTitle("Confirm".to_string()),
        ExpectSpec::StatusMessage("Saved".to_string()),
    ];

    // Act
    let failures: Vec<String> = specs
        .iter()
        .filter_map(|spec| evaluate(spec, &frame).err())
        .map(|failure| failure.message)
        .collect();

    // Assert
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
}

#[test]
fn lower_translates_every_step_kind() {
    // Arrange
    let yaml = "
name: every_step
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
";
    let spec = ScenarioSpec::from_yaml(yaml).expect("parse");
    let expected = Scenario::new("every_step")
        .press_key("Tab")
        .write_text("hi")
        .sleep_ms(25)
        .wait_for_text("Ready", 500)
        .wait_for_stable_frame(100, 900)
        .eventually(
            Duration::from_millis(300),
            Duration::from_millis(10),
            |_| Ok(()),
        )
        .capture()
        .capture_labeled("final", "Final screen");

    // Act
    let lowered = spec.lower();

    // Assert — same steps in order, and the `eventually` predicate runs the
    // declared matcher.
    assert_eq!(lowered.scenario.name, "every_step");
    assert_eq!(
        format!("{:?}", lowered.scenario.steps),
        format!("{:?}", expected.steps)
    );
    let Some(Step::Eventually { predicate, .. }) = lowered.scenario.steps.get(5) else {
        unreachable!("step 6 must be `eventually`");
    };
    assert!(predicate(&TerminalFrame::new(20, 2, b"Done")).is_ok());
    assert!(predicate(&TerminalFrame::new(20, 2, b"Busy")).is_err());
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
    // The fixture also echoes its argument, an environment variable, and a
    // marker file found in its working directory so session lowering is
    // observable.
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let script = temp_dir.path().join("greet.sh");
    let workdir = temp_dir.path().join("work");
    std::fs::create_dir(&workdir).expect("create workdir");
    std::fs::write(workdir.join("marker.txt"), "").expect("write marker");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf 'Hello World %s %s ' \"$1\" \"$GREETING\"\n[ -f marker.txt ] && printf \
         'marker.txt'\nsleep 60\n",
    )
    .expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750))
        .expect("set permissions");

    // Act — YAML path: parse -> lower -> run.
    let yaml = format!(
        "
session:
  bin: {bin}
  size: [80, 24]
  args: [one]
  env: {{ GREETING: two }}
  workdir: {workdir}
steps:
  - wait_for_stable_frame: {{ stable_ms: 300, timeout_ms: 5000 }}
expect:
  - text_in_region: {{ text: \"Hello World one two marker.txt\", region: [0, 0, 80, 1] }}
  - not_visible: Goodbye
",
        bin = script.display(),
        workdir = workdir.display()
    );
    let (yaml_frame, yaml_failures) = ScenarioSpec::from_yaml(&yaml)
        .expect("parse")
        .lower()
        .run()
        .expect("yaml run");

    // Act — code path: the same scenario built and asserted by hand.
    let builder = PtySessionBuilder::new(&script)
        .size(80, 24)
        .args(["one"])
        .env("GREETING", "two")
        .workdir(&workdir);
    let code_frame = Scenario::new("code")
        .wait_for_stable_frame(300, 5000)
        .run(builder)
        .expect("code run");
    let region = Region::new(0, 0, 80, 1);
    let code_text_ok =
        assertion::match_text_in_region(&code_frame, "Hello World one two marker.txt", &region)
            .is_ok();
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

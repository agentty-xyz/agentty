//! Lowering and evaluation: translate a [`ScenarioSpec`] onto the runtime
//! engine and assert its expectations.
//!
//! The spec carries non-serializable engine concepts as plain data (for
//! example `eventually` holds a declarative matcher instead of a closure).
//! [`ScenarioSpec::lower`] resolves those into the runtime `Scenario` builder
//! and matcher functions, so YAML scenarios and the Rust authoring API share
//! one execution path.

use std::time::Duration;

use super::model::{ExpectSpec, SUPPORTED_VERSION, ScenarioSpec, StepSpec};
use crate::assertion::{self, AssertionFailure, MatchResult};
use crate::frame::TerminalFrame;
use crate::recipe;
use crate::region::Region;
use crate::scenario::Scenario;
use crate::session::{PtySessionBuilder, PtySessionError};

/// Errors produced while loading or running a declarative scenario.
///
/// `#[non_exhaustive]`: new error variants stay non-breaking for downstream
/// callers that match on this type (they keep a `_` arm).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpecError {
    /// Reading the scenario file failed.
    #[error("scenario I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Parsing the scenario YAML failed.
    #[error("scenario parse error: {0}")]
    Parse(#[from] serde_yaml_ng::Error),

    /// The scenario file declared a `version` this build does not support.
    #[error("unsupported scenario version {found} (this build supports version {supported})")]
    UnsupportedVersion {
        /// The version found in the file.
        found: u32,
        /// The version this build supports.
        supported: u32,
    },
}

impl ScenarioSpec {
    /// Parse a scenario from YAML, rejecting unsupported format versions.
    ///
    /// # Errors
    ///
    /// Returns [`SpecError::Parse`] if the YAML is malformed and
    /// [`SpecError::UnsupportedVersion`] if `version` is not
    /// [`SUPPORTED_VERSION`].
    pub fn from_yaml(input: &str) -> Result<Self, SpecError> {
        let spec: ScenarioSpec = serde_yaml_ng::from_str(input)?;

        if spec.version != SUPPORTED_VERSION {
            return Err(SpecError::UnsupportedVersion {
                found: spec.version,
                supported: SUPPORTED_VERSION,
            });
        }

        Ok(spec)
    }

    /// Lower this spec onto the runtime engine.
    ///
    /// Produces the [`PtySessionBuilder`] and [`Scenario`] the runtime
    /// executes, plus the expectations to assert against the final frame.
    pub fn lower(&self) -> LoweredScenario {
        let mut builder = PtySessionBuilder::new(self.session.bin.clone());

        if let Some([cols, rows]) = self.session.size {
            builder = builder.size(cols, rows);
        }

        if !self.session.args.is_empty() {
            builder = builder.args(self.session.args.clone());
        }

        for (key, value) in &self.session.env {
            builder = builder.env(key, value);
        }

        if let Some(workdir) = &self.session.workdir {
            builder = builder.workdir(workdir.clone());
        }

        let name = self.name.clone().unwrap_or_else(|| "scenario".to_string());
        let mut scenario = Scenario::new(name);
        for step in &self.steps {
            scenario = lower_step(scenario, step);
        }

        LoweredScenario {
            builder,
            scenario,
            expect: self.expect.clone(),
        }
    }
}

/// A scenario lowered onto the runtime engine, ready to run and assert.
pub struct LoweredScenario {
    /// The session builder that launches the binary under test.
    pub builder: PtySessionBuilder,
    /// The runtime scenario that drives the session.
    pub scenario: Scenario,
    expect: Vec<ExpectSpec>,
}

impl LoweredScenario {
    /// Run the scenario and assert every expectation against the final frame.
    ///
    /// Returns the final frame and any assertion failures. An empty failure
    /// list means the scenario passed.
    ///
    /// # Errors
    ///
    /// Returns a [`PtySessionError`] if spawning or driving the binary fails.
    pub fn run(self) -> Result<(TerminalFrame, Vec<AssertionFailure>), PtySessionError> {
        let LoweredScenario {
            builder,
            scenario,
            expect,
        } = self;

        let frame = scenario.run(builder)?;
        let failures = check_expectations(&expect, &frame);

        Ok((frame, failures))
    }

    /// Assert this scenario's expectations against an already-captured frame.
    ///
    /// Returns the failures; an empty list means every expectation passed.
    pub fn check(&self, frame: &TerminalFrame) -> Vec<AssertionFailure> {
        check_expectations(&self.expect, frame)
    }
}

/// Evaluate every expectation against `frame`, collecting the failures.
fn check_expectations(expect: &[ExpectSpec], frame: &TerminalFrame) -> Vec<AssertionFailure> {
    let mut failures = Vec::new();

    for spec in expect {
        if let Err(failure) = evaluate(spec, frame) {
            failures.push(*failure);
        }
    }

    failures
}

/// Apply one declarative step to the scenario builder.
fn lower_step(scenario: Scenario, step: &StepSpec) -> Scenario {
    match step {
        StepSpec::PressKey(key) => scenario.press_key(key.clone()),
        StepSpec::WriteText(text) => scenario.write_text(text.clone()),
        StepSpec::SleepMs(ms) => scenario.sleep_ms(*ms),
        StepSpec::WaitForText { needle, timeout_ms } => {
            scenario.wait_for_text(needle.clone(), *timeout_ms)
        }
        StepSpec::WaitForStableFrame {
            stable_ms,
            timeout_ms,
        } => scenario.wait_for_stable_frame(*stable_ms, *timeout_ms),
        StepSpec::Eventually {
            matcher,
            timeout_ms,
            poll_ms,
        } => {
            let matcher = matcher.clone();

            scenario.eventually(
                Duration::from_millis(*timeout_ms),
                Duration::from_millis(*poll_ms),
                move |frame| evaluate(&matcher, frame),
            )
        }
        StepSpec::Capture => scenario.capture(),
        StepSpec::CaptureLabeled { label, description } => {
            scenario.capture_labeled(label.clone(), description.clone())
        }
    }
}

/// Resolve one expectation against a frame via the matching recipe/assertion.
///
/// This single dispatch is shared by the `eventually` step (which polls it
/// during the run) and the top-level `expect` list (asserted on the final
/// frame), so both paths stay behaviorally identical.
fn evaluate(spec: &ExpectSpec, frame: &TerminalFrame) -> MatchResult {
    match spec {
        ExpectSpec::SelectedTab(label) => recipe::match_selected_tab(frame, label),
        ExpectSpec::UnselectedTab(label) => recipe::match_unselected_tab(frame, label),
        ExpectSpec::InstructionVisible(text) => recipe::match_instruction_visible(frame, text),
        ExpectSpec::KeybindingHint(text) => recipe::match_keybinding_hint(frame, text),
        ExpectSpec::FooterAction(text) => recipe::match_footer_action(frame, text),
        ExpectSpec::DialogTitle(text) => recipe::match_dialog_title(frame, text),
        ExpectSpec::StatusMessage(text) => recipe::match_status_message(frame, text),
        ExpectSpec::NotVisible(text) => recipe::match_not_visible(frame, text),
        ExpectSpec::TextInRegion { text, region } => assertion::match_text_in_region(
            frame,
            text,
            &Region::new(region.0, region.1, region.2, region.3),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::model::RegionSpec;

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
        use std::os::unix::fs::PermissionsExt;

        // Arrange — a fixture that renders deterministic text and stays alive
        // so the PTY does not close before the frame is captured.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let script = temp_dir.path().join("greet.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n")
            .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
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
        let code_text_ok =
            assertion::match_text_in_region(&code_frame, "Hello World", &region).is_ok();
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
}

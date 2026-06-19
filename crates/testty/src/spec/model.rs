//! Declarative scenario specification — the pure-data model that a YAML
//! scenario file deserializes into.
//!
//! [`ScenarioSpec`] and its children carry no behavior; they are lowered onto
//! the runtime engine (`Scenario`, `Step`, matcher functions) by
//! [`ScenarioSpec::lower`](super::runtime). Keeping the spec as plain data lets
//! the same engine power both the Rust authoring API and the language-agnostic
//! `testty run scenario.yaml` front end.
//!
//! [`StepSpec`] and [`ExpectSpec`] use a hand-written `Deserialize` so each
//! YAML list item is a friendly **single-key map** (`press_key: Tab`) rather
//! than serde's default YAML-tag form (`!press_key Tab`), which non-expert
//! authors rarely recognize.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// The scenario-file format version this build understands.
///
/// A file whose `version:` differs is rejected by
/// [`ScenarioSpec::from_yaml`](super::runtime) so non-Rust authors get a clear
/// error instead of silent misbehavior.
pub const SUPPORTED_VERSION: u32 = 1;

/// Default `version` when a scenario file omits the field.
fn default_version() -> u32 {
    SUPPORTED_VERSION
}

/// A complete declarative scenario: how to launch the binary, the steps to
/// drive it, and the expectations to assert against the final frame.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    /// Scenario-file format version. Defaults to [`SUPPORTED_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// Optional human-readable scenario name used in proof output.
    #[serde(default)]
    pub name: Option<String>,
    /// How to launch the binary under test.
    pub session: SessionSpec,
    /// Ordered steps that drive the session.
    #[serde(default)]
    pub steps: Vec<StepSpec>,
    /// Expectations asserted against the final frame after the steps run.
    #[serde(default)]
    pub expect: Vec<ExpectSpec>,
}

/// How to launch the binary under test.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    /// Path to the binary under test. The CLI `--bin` flag overrides this.
    pub bin: PathBuf,
    /// Terminal size as `[cols, rows]`. Defaults to the engine default.
    #[serde(default)]
    pub size: Option<[u16; 2]>,
    /// Command-line arguments passed to the binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables set for the binary.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory the binary launches in.
    #[serde(default)]
    pub workdir: Option<PathBuf>,
}

/// A single declarative step. In YAML each list item is a single-key map
/// (`press_key: Tab`) or the bare word `capture`.
///
/// `#[non_exhaustive]`: new step kinds can be added without breaking external
/// Rust code that matches this enum (such code keeps a `_` arm).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StepSpec {
    /// Press a named key (for example `Tab`, `Enter`, `Esc`).
    PressKey(String),
    /// Type literal text.
    WriteText(String),
    /// Sleep for the given number of milliseconds.
    SleepMs(u64),
    /// Wait until `needle` appears, up to `timeout_ms`.
    WaitForText {
        /// Text to wait for.
        needle: String,
        /// Maximum time to wait, in milliseconds.
        timeout_ms: u32,
    },
    /// Wait until the screen is unchanged for `stable_ms`, up to `timeout_ms`.
    WaitForStableFrame {
        /// Quiet period the frame must hold, in milliseconds.
        stable_ms: u32,
        /// Maximum time to wait, in milliseconds.
        timeout_ms: u32,
    },
    /// Poll an expectation until it passes or `timeout_ms` elapses.
    Eventually {
        /// The expectation to poll for (the `match` key in YAML).
        matcher: ExpectSpec,
        /// Maximum time to poll, in milliseconds.
        timeout_ms: u64,
        /// Delay between polls, in milliseconds.
        poll_ms: u64,
    },
    /// Capture the current frame for proof output.
    Capture,
    /// Capture the current frame with a label and description.
    CaptureLabeled {
        /// Short identifier for the capture.
        label: String,
        /// Human-readable description of the capture.
        description: String,
    },
}

/// A declarative expectation. In YAML each entry is a single-key map
/// (`selected_tab: Sessions`). Each variant maps to one `recipe::match_*` or
/// `assertion::match_*` matcher when evaluated against a frame.
///
/// `#[non_exhaustive]`: new matcher keys can be added without breaking external
/// Rust matchers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ExpectSpec {
    /// The named tab is selected.
    SelectedTab(String),
    /// The named tab is present but not selected.
    UnselectedTab(String),
    /// An instruction line with the given text is visible.
    InstructionVisible(String),
    /// A keybinding hint with the given text is visible.
    KeybindingHint(String),
    /// A footer action with the given label is visible.
    FooterAction(String),
    /// A dialog with the given title is visible.
    DialogTitle(String),
    /// A status message with the given text is visible.
    StatusMessage(String),
    /// The given text is not visible anywhere on screen.
    NotVisible(String),
    /// The given text appears within a `[col, row, width, height]` region.
    TextInRegion {
        /// Text expected within the region.
        text: String,
        /// Region as `[col, row, width, height]`.
        region: RegionSpec,
    },
}

/// A rectangular region expressed as `[col, row, width, height]`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RegionSpec(pub u16, pub u16, pub u16, pub u16);

// Typed payloads for the struct-shaped step/expect variants. Each rejects
// unknown fields so author typos surface immediately.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForTextArgs {
    needle: String,
    timeout_ms: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForStableFrameArgs {
    stable_ms: u32,
    timeout_ms: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventuallyArgs {
    #[serde(rename = "match")]
    matcher: ExpectSpec,
    timeout_ms: u64,
    poll_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureLabeledArgs {
    label: String,
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextInRegionArgs {
    text: String,
    region: RegionSpec,
}

/// Reject a second key in a single-key step/expect map.
fn reject_extra_key<'de, A: MapAccess<'de>>(map: &mut A, kind: &str) -> Result<(), A::Error> {
    if map.next_key::<IgnoredAny>()?.is_some() {
        return Err(de::Error::custom(format!(
            "a {kind} must have exactly one key"
        )));
    }

    Ok(())
}

impl<'de> Deserialize<'de> for StepSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StepVisitor;

        impl<'de> Visitor<'de> for StepVisitor {
            type Value = StepSpec;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter
                    .write_str("a step: `capture` or a single-key map such as `press_key: Tab`")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<StepSpec, E> {
                match value {
                    "capture" => Ok(StepSpec::Capture),
                    other => Err(E::custom(format!("unknown step `{other}`"))),
                }
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<StepSpec, A::Error> {
                let Some(key) = map.next_key::<String>()? else {
                    return Err(de::Error::custom("a step map must have one key"));
                };

                let step = match key.as_str() {
                    "press_key" => StepSpec::PressKey(map.next_value()?),
                    "write_text" => StepSpec::WriteText(map.next_value()?),
                    "sleep_ms" => StepSpec::SleepMs(map.next_value()?),
                    "wait_for_text" => {
                        let args: WaitForTextArgs = map.next_value()?;

                        StepSpec::WaitForText {
                            needle: args.needle,
                            timeout_ms: args.timeout_ms,
                        }
                    }
                    "wait_for_stable_frame" => {
                        let args: WaitForStableFrameArgs = map.next_value()?;

                        StepSpec::WaitForStableFrame {
                            stable_ms: args.stable_ms,
                            timeout_ms: args.timeout_ms,
                        }
                    }
                    "eventually" => {
                        let args: EventuallyArgs = map.next_value()?;

                        StepSpec::Eventually {
                            matcher: args.matcher,
                            timeout_ms: args.timeout_ms,
                            poll_ms: args.poll_ms,
                        }
                    }
                    "capture_labeled" => {
                        let args: CaptureLabeledArgs = map.next_value()?;

                        StepSpec::CaptureLabeled {
                            label: args.label,
                            description: args.description,
                        }
                    }
                    other => return Err(de::Error::custom(format!("unknown step `{other}`"))),
                };

                reject_extra_key(&mut map, "step")?;

                Ok(step)
            }
        }

        deserializer.deserialize_any(StepVisitor)
    }
}

impl<'de> Deserialize<'de> for ExpectSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ExpectVisitor;

        impl<'de> Visitor<'de> for ExpectVisitor {
            type Value = ExpectSpec;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an expectation map such as `selected_tab: Sessions`")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ExpectSpec, A::Error> {
                let Some(key) = map.next_key::<String>()? else {
                    return Err(de::Error::custom("an expectation map must have one key"));
                };

                let expect = match key.as_str() {
                    "selected_tab" => ExpectSpec::SelectedTab(map.next_value()?),
                    "unselected_tab" => ExpectSpec::UnselectedTab(map.next_value()?),
                    "instruction_visible" => ExpectSpec::InstructionVisible(map.next_value()?),
                    "keybinding_hint" => ExpectSpec::KeybindingHint(map.next_value()?),
                    "footer_action" => ExpectSpec::FooterAction(map.next_value()?),
                    "dialog_title" => ExpectSpec::DialogTitle(map.next_value()?),
                    "status_message" => ExpectSpec::StatusMessage(map.next_value()?),
                    "not_visible" => ExpectSpec::NotVisible(map.next_value()?),
                    "text_in_region" => {
                        let args: TextInRegionArgs = map.next_value()?;

                        ExpectSpec::TextInRegion {
                            text: args.text,
                            region: args.region,
                        }
                    }
                    other => {
                        return Err(de::Error::custom(format!("unknown expectation `{other}`")));
                    }
                };

                reject_extra_key(&mut map, "expectation")?;

                Ok(expect)
            }
        }

        deserializer.deserialize_any(ExpectVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

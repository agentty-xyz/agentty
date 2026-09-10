use std::time::Duration;

use crate::journey::{Journey, StartupWait};
use crate::step::Step;

#[test]
fn journey_new_creates_empty() {
    // Arrange / Act
    let journey = Journey::new("test_journey");

    // Assert
    assert_eq!(journey.name, "test_journey");
    assert!(journey.description.is_none());
    assert!(journey.steps.is_empty());
}

#[test]
fn journey_with_description_sets_description() {
    // Arrange / Act
    let journey = Journey::new("described").with_description("Does something");

    // Assert
    assert_eq!(journey.description.as_deref(), Some("Does something"));
}

#[test]
fn journey_step_appends() {
    // Arrange / Act
    let journey = Journey::new("custom")
        .step(Step::write_text("hello"))
        .step(Step::press_key("Enter"));

    // Assert
    assert_eq!(journey.steps.len(), 2);
}

#[test]
fn wait_for_startup_produces_stable_frame_step() {
    // Arrange / Act
    let journey = Journey::wait_for_startup(300, 5000);

    // Assert
    assert_eq!(journey.name, "wait_for_startup");
    assert_eq!(journey.steps.len(), 1);
    assert!(matches!(
        &journey.steps[0],
        Step::WaitForStableFrame {
            stable_ms: 300,
            timeout_ms: 5000
        }
    ));
}

#[test]
fn startup_wait_presets_expose_documented_durations() {
    // Arrange / Act / Assert — each named preset reports the documented
    // `(stable_ms, timeout_ms)` numbers callers can pin against.
    assert_eq!(StartupWait::Default.stable_ms(), 300);
    assert_eq!(StartupWait::Default.timeout_ms(), 5_000);

    assert_eq!(StartupWait::FastNative.stable_ms(), 200);
    assert_eq!(StartupWait::FastNative.timeout_ms(), 3_000);

    assert_eq!(StartupWait::SlowNode.stable_ms(), 500);
    assert_eq!(StartupWait::SlowNode.timeout_ms(), 10_000);

    let custom = StartupWait::Custom {
        stable_ms: 123,
        timeout_ms: 4_567,
    };
    assert_eq!(custom.stable_ms(), 123);
    assert_eq!(custom.timeout_ms(), 4_567);
}

#[test]
fn wait_for_startup_preset_uses_preset_durations() {
    // Arrange / Act
    let journey = Journey::wait_for_startup_preset(StartupWait::SlowNode);

    // Assert
    assert_eq!(journey.name, "wait_for_startup");
    assert_eq!(journey.steps.len(), 1);
    assert!(matches!(
        &journey.steps[0],
        Step::WaitForStableFrame {
            stable_ms: 500,
            timeout_ms: 10_000,
        }
    ));
}

#[test]
fn wait_for_startup_default_matches_default_preset() {
    // Arrange / Act
    let from_helper = Journey::wait_for_startup_default();
    let from_preset = Journey::wait_for_startup_preset(StartupWait::Default);

    // Assert — both helpers must produce the same documented step so
    // call sites can mix and match without behavioral drift.
    assert_eq!(from_helper.steps.len(), 1);
    assert_eq!(from_preset.steps.len(), 1);
    assert!(matches!(
        &from_helper.steps[0],
        Step::WaitForStableFrame {
            stable_ms: 300,
            timeout_ms: 5_000,
        }
    ));
    assert!(matches!(
        &from_preset.steps[0],
        Step::WaitForStableFrame {
            stable_ms: 300,
            timeout_ms: 5_000,
        }
    ));
}

#[test]
fn wait_for_startup_raw_args_route_through_custom_preset() {
    // Arrange / Act — the raw constructor should still produce the
    // legacy `(stable_ms, timeout_ms)` step shape so existing callers
    // keep working.
    let journey = Journey::wait_for_startup(150, 2_500);

    // Assert
    assert_eq!(journey.steps.len(), 1);
    assert!(matches!(
        &journey.steps[0],
        Step::WaitForStableFrame {
            stable_ms: 150,
            timeout_ms: 2_500,
        }
    ));
}

#[test]
fn navigate_with_key_produces_press_then_wait() {
    // Arrange / Act
    let journey = Journey::navigate_with_key("Tab", "Sessions", 3000);

    // Assert
    assert_eq!(journey.name, "navigate_Tab");
    assert_eq!(journey.steps.len(), 2);
    assert!(matches!(&journey.steps[0], Step::PressKey(key) if key == "Tab"));
    assert!(
        matches!(&journey.steps[1], Step::WaitForText { needle, timeout_ms: 3000 } if needle == "Sessions")
    );
}

#[test]
fn type_and_confirm_produces_write_then_enter() {
    // Arrange / Act
    let journey = Journey::type_and_confirm("hello world");

    // Assert
    assert_eq!(journey.name, "type_and_confirm");
    assert_eq!(journey.steps.len(), 2);
    assert!(matches!(&journey.steps[0], Step::WriteText(text) if text == "hello world"));
    assert!(matches!(&journey.steps[1], Step::PressKey(key) if key == "Enter"));
}

#[test]
fn press_and_wait_produces_key_then_sleep() {
    // Arrange / Act
    let journey = Journey::press_and_wait("Escape", 200);

    // Assert
    assert_eq!(journey.name, "press_Escape");
    assert_eq!(journey.steps.len(), 2);
    assert!(matches!(&journey.steps[0], Step::PressKey(key) if key == "Escape"));
    assert!(
        matches!(&journey.steps[1], Step::Sleep(duration) if *duration == Duration::from_millis(200))
    );
}

#[test]
fn capture_labeled_produces_capture_step() {
    // Arrange / Act
    let journey = Journey::capture_labeled("state", "Current state");

    // Assert
    assert_eq!(journey.name, "capture_state");
    assert_eq!(journey.steps.len(), 1);
    assert!(matches!(
        &journey.steps[0],
        Step::CaptureLabeled { label, description }
        if label == "state" && description == "Current state"
    ));
}

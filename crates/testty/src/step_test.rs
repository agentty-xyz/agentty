use std::time::Duration;

use crate::frame::TerminalFrame;
use crate::step::Step;

#[test]
fn write_text_stores_content() {
    // Arrange / Act
    let step = Step::write_text("hello");

    // Assert
    let Step::WriteText(text) = step else {
        unreachable!("Expected WriteText variant");
    };
    assert_eq!(text, "hello");
}

#[test]
fn sleep_ms_converts_to_duration() {
    // Arrange / Act
    let step = Step::sleep_ms(500);

    // Assert
    let Step::Sleep(duration) = step else {
        unreachable!("Expected Sleep variant");
    };
    assert_eq!(duration, Duration::from_millis(500));
}

#[test]
fn capture_labeled_stores_label_and_description() {
    // Arrange / Act
    let step = Step::capture_labeled("startup", "App launched");

    // Assert
    let Step::CaptureLabeled { label, description } = step else {
        unreachable!("Expected CaptureLabeled variant");
    };
    assert_eq!(label, "startup");
    assert_eq!(description, "App launched");
}

#[test]
fn wait_for_text_stores_needle_and_timeout() {
    // Arrange / Act
    let step = Step::wait_for_text("Loading", 5000);

    // Assert
    let Step::WaitForText { needle, timeout_ms } = step else {
        unreachable!("Expected WaitForText variant");
    };
    assert_eq!(needle, "Loading");
    assert_eq!(timeout_ms, 5000);
}

#[test]
fn viewing_pause_stores_duration() {
    // Arrange / Act
    let step = Step::viewing_pause(Duration::from_secs(2));

    // Assert
    let Step::ViewingPause(duration) = step else {
        unreachable!("Expected ViewingPause variant");
    };
    assert_eq!(duration, Duration::from_secs(2));
}

#[test]
fn viewing_pause_ms_converts_to_duration() {
    // Arrange / Act
    let step = Step::viewing_pause_ms(1500);

    // Assert
    let Step::ViewingPause(duration) = step else {
        unreachable!("Expected ViewingPause variant");
    };
    assert_eq!(duration, Duration::from_millis(1500));
}

/// Verifies `Step::eventually` stores the timeout, poll, and a callable
/// predicate, and that destructuring through the documented field shape
/// keeps compiling against the public variant layout.
#[test]
fn eventually_stores_timeout_poll_and_callable_predicate() {
    // Arrange / Act
    let step = Step::eventually(
        Duration::from_secs(5),
        Duration::from_millis(100),
        |frame: &TerminalFrame| {
            if frame.all_text().contains("Ready") {
                Ok(())
            } else {
                Err(Box::new(crate::assertion::AssertionFailure {
                    message: "missing Ready".to_string(),
                    expected: crate::assertion::Expected::TextInRegion {
                        needle: "Ready".to_string(),
                    },
                    region: None,
                    matched_spans: Vec::new(),
                    frame_excerpt: String::new(),
                }))
            }
        },
    );

    // Assert
    let Step::Eventually {
        timeout,
        poll,
        predicate,
    } = step
    else {
        unreachable!("Expected Eventually variant");
    };
    assert_eq!(timeout, Duration::from_secs(5));
    assert_eq!(poll, Duration::from_millis(100));

    let ready_frame = TerminalFrame::new(80, 24, b"Ready");
    let blank_frame = TerminalFrame::new(80, 24, b"");
    assert!(predicate(&ready_frame).is_ok());
    assert!(predicate(&blank_frame).is_err());
}

/// Verifies the manual `Debug` impl for [`Step::Eventually`] redacts the
/// predicate body so the derived `Clone` keeps compiling and consumers
/// see a stable placeholder rather than a closure pointer.
#[test]
fn eventually_debug_redacts_predicate_body() {
    // Arrange
    let step = Step::eventually(
        Duration::from_millis(250),
        Duration::from_millis(25),
        |_| Ok(()),
    );

    // Act
    let rendered = format!("{step:?}");

    // Assert
    assert!(rendered.contains("Eventually"));
    assert!(rendered.contains("frame predicate"));
}

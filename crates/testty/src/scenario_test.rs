use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::assertion::{AssertionFailure, Expected, MatchResult};
use crate::frame::TerminalFrame;
use crate::journey::Journey;
use crate::scenario::{Scenario, eventually_loop};
use crate::session::PtySessionBuilder;
use crate::step::Step;

#[test]
fn scenario_builder_chains_steps() {
    // Arrange / Act
    let scenario = Scenario::new("test")
        .write_text("hello")
        .press_key("Enter")
        .sleep_ms(100)
        .capture();

    // Assert
    assert_eq!(scenario.name, "test");
    assert_eq!(scenario.steps.len(), 4);
}

#[test]
fn scenario_builder_appends_duration_and_predicate_steps() {
    // Arrange / Act
    let scenario = Scenario::new("timed")
        .sleep(Duration::from_millis(5))
        .eventually(
            Duration::from_secs(1),
            Duration::from_millis(10),
            |_| Ok(()),
        )
        .viewing_pause(Duration::from_millis(7));

    // Assert
    assert!(matches!(
        scenario.steps.as_slice(),
        [
            Step::Sleep(sleep),
            Step::Eventually { timeout, poll, .. },
            Step::ViewingPause(pause),
        ] if *sleep == Duration::from_millis(5)
            && *timeout == Duration::from_secs(1)
            && *poll == Duration::from_millis(10)
            && *pause == Duration::from_millis(7)
    ));
}

#[test]
fn run_with_proof_records_only_labeled_captures_and_captures_empty_scenarios() {
    // Arrange — the trailing wait forms an unlabeled final batch.
    let spawn_shell =
        || PtySessionBuilder::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 60"]);
    let labeled = Scenario::new("labeled")
        .wait_for_text("ready", 3_000)
        .capture_labeled("ready", "Shell is ready")
        .wait_for_text("ready", 3_000);
    let empty = Scenario::new("empty");

    // Act
    let (labeled_frame, labeled_report) = labeled
        .run_with_proof(spawn_shell())
        .expect("labeled scenario should run");
    let (empty_frame, empty_report) = empty
        .run_with_proof(spawn_shell())
        .expect("empty scenario should run");

    // Assert
    assert!(labeled_frame.all_text().contains("ready"));
    assert_eq!(labeled_report.captures.len(), 1);
    assert_eq!(labeled_report.captures[0].label, "ready");
    assert!(
        empty_frame.all_text().contains("ready"),
        "an empty scenario must still capture the current frame"
    );
    assert!(empty_report.captures.is_empty());
}

#[test]
fn write_vhs_tape_writes_compiled_tape_or_reports_io_failure() {
    // Arrange
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let scenario = Scenario::new("tape").capture();
    let binary = Path::new("/usr/bin/echo");
    let screenshot = temp_dir.path().join("shot.png");
    let tape_path = temp_dir.path().join("demo.tape");

    // Act
    let written = scenario.write_vhs_tape(binary, &screenshot, &[], &tape_path);
    let failed = scenario.write_vhs_tape(
        binary,
        &screenshot,
        &[],
        &temp_dir.path().join("missing").join("demo.tape"),
    );

    // Assert
    assert_eq!(written.expect("tape should be written"), tape_path);
    assert_eq!(
        fs::read_to_string(&tape_path).expect("read written tape"),
        scenario.to_vhs_tape(binary, &screenshot, &[]).render()
    );
    assert!(failed.is_err(), "a missing tape directory must fail");
}

#[test]
fn scenario_compiles_to_vhs_tape() {
    // Arrange
    let scenario = Scenario::new("startup").sleep_ms(500).capture();

    // Act
    let tape = scenario.to_vhs_tape(
        Path::new("/usr/bin/echo"),
        Path::new("/tmp/screenshot.png"),
        &[],
    );
    let content = tape.render();

    // Assert
    assert!(content.contains("Screenshot"));
    assert!(content.contains("Sleep"));
}

#[test]
fn scenario_capture_labeled_adds_step() {
    // Arrange / Act
    let scenario = Scenario::new("labeled")
        .capture_labeled("init", "Initial state")
        .capture_labeled("done", "Final state");

    // Assert
    assert_eq!(scenario.steps.len(), 2);
}

#[test]
fn proof_step_ranges_end_at_labeled_captures() {
    // Arrange
    let scenario = Scenario::new("proof-batches")
        .press_key("Tab")
        .viewing_pause_ms(500)
        .capture_labeled("first", "First capture")
        .write_text("hello")
        .capture_labeled("second", "Second capture")
        .press_key("Enter");

    // Act
    let ranges = scenario.proof_step_ranges();

    // Assert
    assert_eq!(ranges, vec![0..=2, 3..=4, 5..=5]);
}

#[test]
fn scenario_compose_appends_journey_steps() {
    // Arrange
    let startup = Journey::wait_for_startup(300, 5000);
    let navigate = Journey::navigate_with_key("Tab", "Sessions", 3000);

    // Act
    let scenario = Scenario::new("composed")
        .compose(&startup)
        .compose(&navigate)
        .capture();

    // Assert — 1 from startup + 2 from navigate + 1 capture = 4.
    assert_eq!(scenario.steps.len(), 4);
}

/// Verifies that `eventually_loop` returns the captured frame the first
/// tick after the predicate succeeds, and that the predicate is polled
/// at least twice when it initially fails before recovering.
#[test]
fn eventually_loop_returns_ok_after_predicate_succeeds() {
    // Arrange — frame source produces three identical frames; predicate
    // tracks the call count and only succeeds on the third tick.
    let frame_calls = AtomicUsize::new(0);
    let predicate_calls = AtomicUsize::new(0);
    let now_calls = AtomicUsize::new(0);
    let sleep_calls = Mutex::new(Vec::<Duration>::new());

    let predicate: Box<dyn Fn(&TerminalFrame) -> MatchResult + Send + Sync> =
        Box::new(|_frame: &TerminalFrame| {
            let count = predicate_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if count >= 3 {
                Ok(())
            } else {
                Err(Box::new(AssertionFailure {
                    message: format!("not yet (call {count})"),
                    expected: Expected::TextInRegion {
                        needle: "Ready".to_string(),
                    },
                    region: None,
                    matched_spans: Vec::new(),
                    frame_excerpt: String::new(),
                }))
            }
        });
    let start = Instant::now();

    // Act
    let result = eventually_loop(
        Duration::from_mins(1),
        Duration::from_millis(10),
        || {
            frame_calls.fetch_add(1, Ordering::SeqCst);
            TerminalFrame::new(80, 24, b"")
        },
        predicate.as_ref(),
        |duration| {
            sleep_calls
                .lock()
                .expect("sleep mutex must not be poisoned")
                .push(duration);
        },
        || {
            now_calls.fetch_add(1, Ordering::SeqCst);
            start
        },
    );

    // Assert
    assert!(result.is_ok(), "expected Ok once the predicate succeeds");
    assert!(
        predicate_calls.load(Ordering::SeqCst) >= 2,
        "predicate must run at least twice before succeeding, got {}",
        predicate_calls.load(Ordering::SeqCst)
    );
    assert_eq!(
        frame_calls.load(Ordering::SeqCst),
        predicate_calls.load(Ordering::SeqCst),
        "every tick must re-read the frame"
    );
    let recorded_sleeps = sleep_calls
        .lock()
        .expect("sleep mutex must not be poisoned")
        .clone();
    assert!(
        recorded_sleeps
            .iter()
            .all(|duration| *duration == Duration::from_millis(10)),
        "every sleep tick must use the configured poll interval"
    );
}

/// Verifies that `eventually_loop` surfaces the last `AssertionFailure`
/// produced by the predicate when the deadline elapses, instead of
/// throwing away the structured failure context.
#[test]
fn eventually_loop_returns_last_failure_on_timeout() {
    // Arrange — predicate always fails with a counter-tagged message;
    // the injected `now` advances past the deadline after the second
    // call so the loop times out deterministically.
    let predicate_calls = AtomicUsize::new(0);
    let now_calls = AtomicUsize::new(0);
    let start = Instant::now();
    let timeout = Duration::from_millis(50);

    let predicate: Box<dyn Fn(&TerminalFrame) -> MatchResult + Send + Sync> =
        Box::new(|_frame: &TerminalFrame| {
            let count = predicate_calls.fetch_add(1, Ordering::SeqCst) + 1;
            Err(Box::new(AssertionFailure {
                message: format!("attempt {count} failed"),
                expected: Expected::TextInRegion {
                    needle: "Ready".to_string(),
                },
                region: None,
                matched_spans: Vec::new(),
                frame_excerpt: String::new(),
            }))
        });

    // Act
    let result = eventually_loop(
        timeout,
        Duration::from_millis(5),
        || TerminalFrame::new(80, 24, b""),
        predicate.as_ref(),
        |_duration| {},
        || {
            let count = now_calls.fetch_add(1, Ordering::SeqCst);
            // First call seeds the deadline at `start + timeout`.
            // Second call (after the first predicate evaluation) returns
            // a value past the deadline so the loop exits with the
            // freshly produced failure.
            if count == 0 {
                start
            } else {
                start + timeout + Duration::from_millis(1)
            }
        },
    );

    // Assert
    let Err(failure) = result else {
        unreachable!("loop must time out when predicate never succeeds");
    };
    assert_eq!(
        predicate_calls.load(Ordering::SeqCst),
        1,
        "the surfaced failure must be the most recent predicate result"
    );
    assert_eq!(failure.message, "attempt 1 failed");
    assert!(matches!(
        failure.expected,
        Expected::TextInRegion { ref needle, .. } if needle == "Ready"
    ));
}

/// Verifies the cadence wait is owned by `eventually_loop`: the sleep
/// is clamped to the time remaining before the deadline, so a final
/// tick whose nominal `poll` would overshoot the deadline only sleeps
/// for the remaining slice.
#[test]
fn eventually_loop_clamps_final_sleep_to_remaining_budget() {
    // Arrange — predicate always fails. `now` advances 30ms per call
    // so the second wait would normally exceed the 50ms budget, but
    // the loop must clamp it to the remaining 20ms before timing out.
    let predicate_calls = AtomicUsize::new(0);
    let now_calls = AtomicUsize::new(0);
    let sleep_calls = Mutex::new(Vec::<Duration>::new());
    let start = Instant::now();
    let timeout = Duration::from_millis(50);
    let poll = Duration::from_millis(40);

    let predicate: Box<dyn Fn(&TerminalFrame) -> MatchResult + Send + Sync> =
        Box::new(|_frame: &TerminalFrame| {
            predicate_calls.fetch_add(1, Ordering::SeqCst);
            Err(Box::new(AssertionFailure {
                message: "still failing".to_string(),
                expected: Expected::TextInRegion {
                    needle: "Ready".to_string(),
                },
                region: None,
                matched_spans: Vec::new(),
                frame_excerpt: String::new(),
            }))
        });

    // Act
    let result = eventually_loop(
        timeout,
        poll,
        || TerminalFrame::new(80, 24, b""),
        predicate.as_ref(),
        |duration| {
            sleep_calls
                .lock()
                .expect("sleep mutex must not be poisoned")
                .push(duration);
        },
        || {
            let count = now_calls.fetch_add(1, Ordering::SeqCst);
            // 1st call seeds deadline = start + 50ms.
            // 2nd call (post first predicate) returns start + 30ms,
            // leaving 20ms before the deadline.
            // 3rd call (post second predicate) returns start + 60ms,
            // already past the deadline so the loop returns the
            // structured failure instead of sleeping again.
            start + Duration::from_millis(30 * count as u64)
        },
    );

    // Assert
    assert!(result.is_err(), "predicate never succeeds");
    assert_eq!(
        predicate_calls.load(Ordering::SeqCst),
        2,
        "the loop must give the predicate one final tick after the clamped sleep"
    );
    let recorded_sleeps = sleep_calls
        .lock()
        .expect("sleep mutex must not be poisoned")
        .clone();
    assert_eq!(
        recorded_sleeps,
        vec![Duration::from_millis(20)],
        "final sleep must be clamped to the remaining time before the deadline"
    );
}

#[test]
fn scenario_compose_preserves_existing_steps() {
    // Arrange
    let journey = Journey::type_and_confirm("hello");

    // Act
    let scenario = Scenario::new("mixed")
        .sleep_ms(100)
        .compose(&journey)
        .capture();

    // Assert — 1 sleep + 2 from journey + 1 capture = 4.
    assert_eq!(scenario.steps.len(), 4);
}

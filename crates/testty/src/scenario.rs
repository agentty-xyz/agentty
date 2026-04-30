//! Scenario builder for composing test scenarios from steps.
//!
//! A [`Scenario`] is an ordered sequence of [`Step`] actions that describe
//! a complete user journey through a TUI application. Scenarios are authored
//! in Rust and can be compiled into both PTY executor actions (for semantic
//! assertions) and VHS tape files (for visual screenshot capture).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::assertion::{AssertionFailure, MatchResult};
use crate::frame::TerminalFrame;
use crate::journey::Journey;
use crate::proof::report::ProofReport;
use crate::session::{PtySession, PtySessionBuilder, PtySessionError};
use crate::step::Step;
use crate::vhs::VhsTape;

/// A test scenario describing a user journey through a TUI application.
///
/// Built using a fluent API, then executed against either the PTY executor
/// or compiled into a VHS tape.
#[must_use]
pub struct Scenario {
    /// Human-readable name for this scenario (used in artifact file names).
    pub name: String,
    /// Ordered sequence of steps to execute.
    pub steps: Vec<Step>,
}

impl Scenario {
    /// Create a new empty scenario with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
        }
    }

    /// Append a step to the scenario and return `self` for chaining.
    pub fn step(mut self, step: Step) -> Self {
        self.steps.push(step);

        self
    }

    /// Type text into the terminal.
    pub fn write_text(self, text: impl Into<String>) -> Self {
        self.step(Step::write_text(text))
    }

    /// Press a named key.
    pub fn press_key(self, key: impl Into<String>) -> Self {
        self.step(Step::press_key(key))
    }

    /// Sleep for a duration.
    pub fn sleep(self, duration: Duration) -> Self {
        self.step(Step::sleep(duration))
    }

    /// Sleep for a number of milliseconds.
    pub fn sleep_ms(self, ms: u64) -> Self {
        self.step(Step::sleep_ms(ms))
    }

    /// Wait for text to appear in the terminal.
    pub fn wait_for_text(self, needle: impl Into<String>, timeout_ms: u32) -> Self {
        self.step(Step::wait_for_text(needle, timeout_ms))
    }

    /// Wait for the terminal frame to stabilize.
    pub fn wait_for_stable_frame(self, stable_ms: u32, timeout_ms: u32) -> Self {
        self.step(Step::wait_for_stable_frame(stable_ms, timeout_ms))
    }

    /// Poll a frame predicate until it returns `Ok(())` or `timeout` elapses.
    ///
    /// Wraps [`Step::eventually`] for fluent chaining inside scenario
    /// builders. The predicate runs against the live PTY frame on every
    /// `poll` tick and the scenario fails with the last
    /// [`AssertionFailure`] the predicate produced if `timeout` elapses
    /// without success.
    pub fn eventually<F>(self, timeout: Duration, poll: Duration, predicate: F) -> Self
    where
        F: Fn(&TerminalFrame) -> MatchResult + Send + Sync + 'static,
    {
        self.step(Step::eventually(timeout, poll, predicate))
    }

    /// Insert a viewing pause that only affects VHS GIF output.
    ///
    /// The PTY executor skips this step, keeping assertion runs fast while
    /// giving human viewers time to absorb the current frame in GIFs.
    pub fn viewing_pause(self, duration: Duration) -> Self {
        self.step(Step::viewing_pause(duration))
    }

    /// Insert a viewing pause in milliseconds (VHS-only, PTY no-op).
    pub fn viewing_pause_ms(self, ms: u64) -> Self {
        self.step(Step::viewing_pause_ms(ms))
    }

    /// Capture the current terminal state.
    pub fn capture(self) -> Self {
        self.step(Step::capture())
    }

    /// Capture the current terminal state with a label and description.
    ///
    /// Labeled captures are collected into a
    /// [`crate::proof::report::ProofReport`] when running with
    /// `run_with_proof()`.
    pub fn capture_labeled(self, label: impl Into<String>, description: impl Into<String>) -> Self {
        self.step(Step::capture_labeled(label, description))
    }

    /// Append all steps from a journey to this scenario.
    ///
    /// Enables declarative test building by composing reusable
    /// building blocks.
    pub fn compose(mut self, journey: &Journey) -> Self {
        self.steps.extend(journey.steps.iter().cloned());

        self
    }

    /// Execute this scenario in a PTY session and return the final frame.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    pub fn execute_in_pty(
        &self,
        session: &mut PtySession,
    ) -> Result<crate::frame::TerminalFrame, PtySessionError> {
        session.execute_steps(&self.steps)
    }

    /// Execute this scenario in a PTY session with proof collection.
    ///
    /// Returns both the final frame and a [`ProofReport`] containing all
    /// labeled captures encountered during execution.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    pub fn execute_in_pty_with_proof(
        &self,
        session: &mut PtySession,
    ) -> Result<(crate::frame::TerminalFrame, ProofReport), PtySessionError> {
        let mut report = ProofReport::new(&self.name);
        let mut last_frame = None;

        for step in &self.steps {
            match step {
                Step::CaptureLabeled { label, description } => {
                    let frame = session.capture_frame();
                    report.add_capture(label, description, &frame);
                    last_frame = Some(frame);
                }
                _ => {
                    last_frame = Some(session.execute_steps(std::slice::from_ref(step))?);
                }
            }
        }

        let final_frame = last_frame.unwrap_or_else(|| session.capture_frame());

        Ok((final_frame, report))
    }

    /// Execute this scenario against a binary, creating a new PTY session
    /// with the given builder configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if spawning or execution fails.
    pub fn run(
        &self,
        builder: PtySessionBuilder,
    ) -> Result<crate::frame::TerminalFrame, PtySessionError> {
        let mut session = builder.spawn()?;

        self.execute_in_pty(&mut session)
    }

    /// Execute this scenario with proof collection, creating a new PTY
    /// session with the given builder configuration.
    ///
    /// Returns both the final frame and a [`ProofReport`] containing all
    /// labeled captures.
    ///
    /// # Errors
    ///
    /// Returns an error if spawning or execution fails.
    pub fn run_with_proof(
        &self,
        builder: PtySessionBuilder,
    ) -> Result<(crate::frame::TerminalFrame, ProofReport), PtySessionError> {
        let mut session = builder.spawn()?;

        self.execute_in_pty_with_proof(&mut session)
    }

    /// Compile this scenario into a VHS tape.
    ///
    /// The tape can be written to disk and executed with `vhs` to produce
    /// a screenshot of the same journey.
    pub fn to_vhs_tape(
        &self,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
    ) -> VhsTape {
        VhsTape::from_scenario(self, binary_path, screenshot_path, env_vars)
    }

    /// Compile this scenario into a VHS tape and write it to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the tape file fails.
    pub fn write_vhs_tape(
        &self,
        binary_path: &Path,
        screenshot_path: &Path,
        env_vars: &[(&str, &str)],
        tape_path: &Path,
    ) -> Result<PathBuf, std::io::Error> {
        let tape = self.to_vhs_tape(binary_path, screenshot_path, env_vars);
        tape.write_to(tape_path)?;

        Ok(tape_path.to_path_buf())
    }
}

/// Runtime helper that drives [`Step::Eventually`] semantics against an
/// injectable frame source.
///
/// The helper re-reads a frame on every tick by calling `frame_source`,
/// runs the predicate against it, and returns the captured frame the
/// instant the predicate returns `Ok(())`. When `now()` reaches the
/// computed deadline before the predicate succeeds, the helper returns
/// the last [`AssertionFailure`] the predicate produced so the executor
/// can route it through [`PtySessionError::Assertion`] without losing
/// the structured failure context the proof report renders.
///
/// The wait between predicate evaluations is owned exclusively by this
/// helper: it sleeps for `poll`, clamped to the remaining time before
/// `deadline`, so the predicate cadence matches the configured `poll`
/// value and the loop never sleeps past the deadline. Callers must keep
/// their `frame_source` non-blocking so a 50ms `poll` does not turn into
/// a 100ms tick from a redundant blocking drain.
///
/// `sleep` and `now` are injected so unit tests can drive the loop with
/// deterministic timing instead of waiting on the wall clock. Production
/// callers pass [`std::thread::sleep`] and [`Instant::now`].
pub(crate) fn eventually_loop<F, S, N>(
    timeout: Duration,
    poll: Duration,
    mut frame_source: F,
    predicate: &(dyn Fn(&TerminalFrame) -> MatchResult + Send + Sync),
    mut sleep: S,
    mut now: N,
) -> Result<TerminalFrame, Box<AssertionFailure>>
where
    F: FnMut() -> TerminalFrame,
    S: FnMut(Duration),
    N: FnMut() -> Instant,
{
    let deadline = now() + timeout;

    loop {
        let frame = frame_source();
        let failure = match predicate(&frame) {
            Ok(()) => return Ok(frame),
            Err(failure) => failure,
        };

        let remaining = deadline
            .checked_duration_since(now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(failure);
        }

        sleep(poll.min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::assertion::{AssertionFailure, Expected};

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
            Duration::from_secs(60),
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
}

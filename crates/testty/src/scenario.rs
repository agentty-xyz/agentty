//! Scenario builder for composing test scenarios from steps.
//!
//! A [`Scenario`] is an ordered sequence of [`Step`] actions that describe
//! a complete user journey through a TUI application. Scenarios are authored
//! in Rust and can be compiled into both PTY executor actions (for semantic
//! assertions) and VHS tape files (for visual screenshot capture).

use std::ops::RangeInclusive;
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

    /// Wait for terminal text, styling, and cursor state to stabilize.
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
    /// labeled captures encountered during execution. Steps are executed in
    /// batches ending at each labeled capture so input-only and VHS viewing
    /// steps do not trigger redundant PTY frame drains.
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

        for step_range in self.proof_step_ranges() {
            let final_step_index = *step_range.end();
            let frame = session.execute_steps(&self.steps[step_range])?;

            if let Step::CaptureLabeled { label, description } = &self.steps[final_step_index] {
                report.add_capture(label, description, &frame);
            }

            last_frame = Some(frame);
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

    /// Group PTY proof steps so each labeled capture terminates one batch.
    ///
    /// Executing a whole batch avoids the implicit final-frame capture that
    /// [`PtySession::execute_steps`] performs when called with one input-only
    /// step while still preserving every labeled proof boundary.
    fn proof_step_ranges(&self) -> Vec<RangeInclusive<usize>> {
        let mut ranges = Vec::new();
        let mut range_start = 0;

        for (step_index, step) in self.steps.iter().enumerate() {
            if matches!(step, Step::CaptureLabeled { .. }) {
                ranges.push(range_start..=step_index);
                range_start = step_index + 1;
            }
        }

        if range_start < self.steps.len() {
            ranges.push(range_start..=self.steps.len() - 1);
        }

        ranges
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
#[path = "scenario_test.rs"]
mod tests;

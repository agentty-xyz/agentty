//! Scenario step definitions for TUI test automation.
//!
//! Each [`Step`] represents a single action in a test scenario, such as
//! typing text, pressing a key, waiting for output, or capturing a frame.
//! Steps are designed to be compiled into both PTY executor actions and
//! VHS tape commands from a single authored scenario.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::assertion::MatchResult;
use crate::frame::TerminalFrame;

/// Shared boxed predicate evaluated against a [`TerminalFrame`].
///
/// Used as the payload of [`Step::Eventually`] so the same predicate value
/// can be cloned with the enclosing [`Step`] without re-allocating the
/// underlying closure.
pub type FramePredicate = Arc<dyn Fn(&TerminalFrame) -> MatchResult + Send + Sync>;

/// A single action in a test scenario.
///
/// Steps describe user interactions and wait conditions in a
/// platform-neutral way. The PTY executor runs them directly against
/// the terminal, while the VHS compiler translates them into tape syntax.
#[derive(Clone)]
pub enum Step {
    /// Type text into the terminal.
    WriteText(String),

    /// Press a named key (e.g., `"Enter"`, `"Tab"`, `"Escape"`, `"Up"`).
    PressKey(String),

    /// Sleep for a fixed duration.
    Sleep(Duration),

    /// Wait until the specified text appears in the terminal output.
    WaitForText {
        /// The text to search for.
        needle: String,
        /// Maximum time to wait in milliseconds.
        timeout_ms: u32,
    },

    /// Wait until the terminal frame stops changing.
    WaitForStableFrame {
        /// Duration of stability required in milliseconds.
        stable_ms: u32,
        /// Maximum time to wait in milliseconds.
        timeout_ms: u32,
    },

    /// Poll a frame predicate until it returns `Ok(())` or the timeout elapses.
    ///
    /// The PTY executor re-reads the live terminal frame on every `poll`
    /// tick and runs `predicate` against it. The first `Ok(())` result
    /// returns immediately, while a timeout surfaces the last
    /// [`crate::assertion::AssertionFailure`] the predicate produced so the
    /// proof report keeps its structured failure context. VHS recordings
    /// have no equivalent semantics and skip this step.
    Eventually {
        /// Maximum time to spend polling before surfacing the last failure.
        timeout: Duration,
        /// Interval between predicate evaluations.
        poll: Duration,
        /// Frame predicate evaluated on every tick.
        predicate: FramePredicate,
    },

    /// Capture the current terminal state for assertions.
    Capture,

    /// Capture the current terminal state with a label and description.
    ///
    /// Labeled captures are collected into a
    /// [`crate::proof::report::ProofReport`] during execution, enabling
    /// annotated proof output that documents each step of a test journey.
    CaptureLabeled {
        /// Short identifier for this capture (used in headings and file names).
        label: String,
        /// Human-readable description of what this capture documents.
        description: String,
    },

    /// Viewing pause for GIF recordings — no-op in PTY execution.
    ///
    /// Compiles to a `Sleep` in VHS tape output so human viewers can absorb
    /// the current frame before the next action. The PTY executor skips
    /// this step entirely, keeping assertion-only test runs fast.
    ViewingPause(Duration),
}

impl Step {
    /// Create a step that types the given text.
    pub fn write_text(text: impl Into<String>) -> Self {
        Self::WriteText(text.into())
    }

    /// Create a step that presses a named key.
    pub fn press_key(key: impl Into<String>) -> Self {
        Self::PressKey(key.into())
    }

    /// Create a step that sleeps for the given duration.
    pub fn sleep(duration: Duration) -> Self {
        Self::Sleep(duration)
    }

    /// Create a step that sleeps for the given number of milliseconds.
    pub fn sleep_ms(ms: u64) -> Self {
        Self::Sleep(Duration::from_millis(ms))
    }

    /// Create a step that waits for text to appear.
    pub fn wait_for_text(needle: impl Into<String>, timeout_ms: u32) -> Self {
        Self::WaitForText {
            needle: needle.into(),
            timeout_ms,
        }
    }

    /// Create a step that waits for terminal text, styling, and cursor state
    /// to stabilize.
    pub fn wait_for_stable_frame(stable_ms: u32, timeout_ms: u32) -> Self {
        Self::WaitForStableFrame {
            stable_ms,
            timeout_ms,
        }
    }

    /// Create a step that polls `predicate` against the live frame until it
    /// returns `Ok(())` or `timeout` elapses.
    ///
    /// The predicate is wrapped in an [`Arc`] so the resulting [`Step`] can
    /// be cloned across scenario reuse without re-allocating the closure.
    /// On timeout the executor surfaces the last
    /// [`crate::assertion::AssertionFailure`] the predicate produced so the
    /// proof report can render the structured context.
    pub fn eventually<F>(timeout: Duration, poll: Duration, predicate: F) -> Self
    where
        F: Fn(&TerminalFrame) -> MatchResult + Send + Sync + 'static,
    {
        Self::Eventually {
            timeout,
            poll,
            predicate: Arc::new(predicate),
        }
    }

    /// Create a capture step.
    pub fn capture() -> Self {
        Self::Capture
    }

    /// Create a labeled capture step.
    ///
    /// The `label` is a short identifier used in headings, and the
    /// `description` explains what this capture documents.
    pub fn capture_labeled(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self::CaptureLabeled {
            label: label.into(),
            description: description.into(),
        }
    }

    /// Create a viewing pause step with the given duration.
    ///
    /// The pause only affects VHS tape output. PTY execution skips it.
    pub fn viewing_pause(duration: Duration) -> Self {
        Self::ViewingPause(duration)
    }

    /// Create a viewing pause step from a millisecond count.
    ///
    /// The pause only affects VHS tape output. PTY execution skips it.
    pub fn viewing_pause_ms(ms: u64) -> Self {
        Self::ViewingPause(Duration::from_millis(ms))
    }
}

/// Manual `Debug` implementation that hides the [`Step::Eventually`]
/// predicate body so the enclosing `#[derive(Clone)]` keeps compiling
/// even though `dyn Fn` does not implement [`fmt::Debug`].
impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriteText(text) => f.debug_tuple("WriteText").field(text).finish(),
            Self::PressKey(key) => f.debug_tuple("PressKey").field(key).finish(),
            Self::Sleep(duration) => f.debug_tuple("Sleep").field(duration).finish(),
            Self::WaitForText { needle, timeout_ms } => f
                .debug_struct("WaitForText")
                .field("needle", needle)
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::WaitForStableFrame {
                stable_ms,
                timeout_ms,
            } => f
                .debug_struct("WaitForStableFrame")
                .field("stable_ms", stable_ms)
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::Eventually { timeout, poll, .. } => f
                .debug_struct("Eventually")
                .field("timeout", timeout)
                .field("poll", poll)
                .field("predicate", &"<frame predicate>")
                .finish(),
            Self::Capture => f.debug_tuple("Capture").finish(),
            Self::CaptureLabeled { label, description } => f
                .debug_struct("CaptureLabeled")
                .field("label", label)
                .field("description", description)
                .finish(),
            Self::ViewingPause(duration) => f.debug_tuple("ViewingPause").field(duration).finish(),
        }
    }
}

#[cfg(test)]
#[path = "step_test.rs"]
mod tests;

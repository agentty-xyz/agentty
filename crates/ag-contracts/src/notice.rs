//! Noteworthy changes to how one agent turn ran.

/// Something about how a turn ran that the user should know, reported at
/// most once per occurrence while the turn runs. Hosts show it in the
/// session transcript beside the turn's answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnNotice {
    /// Earlier turns were summarized into a checkpoint that later requests
    /// carry in their place, so the session fits the model's context window.
    ContextCompacted {
        /// Model that wrote the summary, when known.
        model: Option<String>,
    },
    /// Summarizing earlier turns failed, so the oldest turns that no longer
    /// fit were dropped from the context instead.
    ContextCompactionFailed {
        /// Short, one-line category of the failure, such as `invalid
        /// output`; never a provider's response text.
        reason: String,
    },
}

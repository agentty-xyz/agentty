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
    /// The turn's shell commands run without the native sandbox, or inside
    /// it without a grant this host cannot honor. Every turn offering
    /// commands reports it while the condition lasts.
    SandboxUnavailable {
        /// What is missing, written for the user.
        message: String,
        /// How the user can fix it.
        remedy: String,
    },
    /// The turn offered shell commands and the native sandbox ran them, so
    /// a host can tell the user when an earlier warning no longer applies.
    SandboxAvailable,
    /// A sandboxed command failed with an operating-system denial the
    /// sandbox may have caused. The guess is heuristic, since the command's
    /// own output can trigger it; hosts never rerun a command outside the
    /// sandbox because of it.
    LikelySandboxDenial,
    /// The turn asked to share the host's dependency caches, but these stay
    /// per session because sharing them could not be confined: a cache that
    /// is a link, lies outside the home directory or on `PATH`, or whose
    /// lock file has another link.
    HostCachesNotShared {
        /// The caches kept per session, named for the user, such as `Cargo`.
        caches: Vec<String>,
    },
}

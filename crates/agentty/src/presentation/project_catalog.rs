//! Presentation state for explicit project registration and discovery.

use std::path::PathBuf;

use crate::domain::input::InputState;

/// User-facing project catalog dialog, shared by input and rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectCatalogDialog {
    /// One-time startup invitation; skipping is selected initially.
    SuggestHome {
        /// Explicit home scan root shown to the user.
        root: PathBuf,
        /// Index of Discover (0) or Skip (1).
        selected: usize,
    },
    /// User-supplied repository or scan folder.
    PathInput {
        /// Whether submitting starts recursive discovery.
        discover: bool,
        /// Validation failure displayed above the path.
        error: Option<String>,
        /// Editable path; relative paths resolve from the launch project.
        input: InputState,
    },
    /// Background scan that can be cancelled without saving results.
    Scanning {
        /// Selected scan root.
        root: PathBuf,
    },
    /// Repositories awaiting explicit selection and registration.
    Results {
        /// Persistence failure, if any.
        error: Option<String>,
        /// Repositories and their selected flags.
        paths: Vec<(PathBuf, bool)>,
        /// Keyboard cursor within the result rows.
        selected: usize,
    },
    /// Informational outcome, dismissed with Enter or Escape.
    Message {
        /// Outcome or failure message.
        message: String,
    },
}

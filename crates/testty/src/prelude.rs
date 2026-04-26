//! Curated re-exports for the common testty workflow.
//!
//! ```no_run
//! use testty::prelude::*;
//!
//! let scenario = Scenario::new("smoke")
//!     .wait_for_stable_frame(300, 5_000)
//!     .press_key("q");
//! ```

pub use crate::assertion;
pub use crate::assertion::{AssertionFailure, Expected, MatchResult};
pub use crate::frame::{CellColor, CellStyle, TerminalFrame};
pub use crate::journey::Journey;
pub use crate::locator::MatchedSpan;
pub use crate::proof::backend::ProofBackend;
pub use crate::proof::report::{ProofCapture, ProofError, ProofReport};
pub use crate::region::Region;
pub use crate::scenario::Scenario;
pub use crate::session::{PtySession, PtySessionBuilder, PtySessionError};
pub use crate::step::Step;

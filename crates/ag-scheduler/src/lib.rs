//! Shared admission for session turns.
//!
//! Hosts retain durable operation ownership and task claims. A process-local
//! permit bounds concurrent session turns after those operations are accepted.

mod admission;

pub use admission::SessionAdmission;

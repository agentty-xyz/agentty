//! Unit tests for turn reservation, split by behavior.

#[path = "reservation_test/admission_test.rs"]
mod admission;
#[cfg(feature = "sqlite")]
#[path = "reservation_test/journal_test.rs"]
mod journal;
#[path = "reservation_test/lease_test.rs"]
mod lease;
#[path = "reservation_test/recovery_test.rs"]
mod recovery;

//! Shared foreground admission for session workflow mutations.
//!
//! Transport, initiator, and acknowledgement policy are independent. Queries
//! stay on the runtime query path; accepted effects retain their existing
//! session-worker ordering and persistence contracts.

mod admission;
mod command;
mod review;

pub(crate) use command::{CommandDelivery, CommandInitiator, SessionCommand, SessionMutation};
pub(crate) use review::ReviewRequestCommand;

#[cfg(test)]
#[path = "command_gateway_test.rs"]
mod tests;

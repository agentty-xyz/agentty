//! Shared CLI subprocess helpers for agent-backed transports.
//!
//! This parent module intentionally acts as a router only so shared stdin I/O
//! and provider-aware exit guidance can be reused by session turns and one-shot
//! utility prompts without duplicating subprocess details.

pub(crate) mod error;
pub(crate) mod stdin;

//! Shared admission for session turns and pure selection of campaign tasks.
//!
//! Hosts retain durable operation ownership and task claims. A process-local
//! permit bounds concurrent session turns after those operations are accepted.

mod admission;
mod campaign;

pub use admission::SessionAdmission;
pub use campaign::{CampaignCandidate, CampaignDecision, select_campaign_tasks};

//! Full-screen UI page modules.

/// Scrollable session git-diff page.
pub mod diff;
/// Generic saved execution graph inspector.
pub(crate) mod execution_graph;
/// Graph proposal and live execution sidebar beside session chat.
pub(crate) mod graph_chat;
/// Platform sign-in, account, and remote-control page.
pub mod platform;
/// Project selection and activity list page.
pub mod project_list;
/// Remote-control sections of the Platform page: pairing, the relay link,
/// and paired devices.
pub(crate) mod remote_control;
/// Session-linked forge-review panels embedded in Diff mode.
pub mod review_comment;
/// Session transcript and prompt page.
pub mod session_chat;
/// Grouped session list page.
pub mod session_list;
/// Project settings page.
pub mod setting;

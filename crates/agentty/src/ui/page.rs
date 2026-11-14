//! Full-screen UI page modules.

/// Scrollable session git-diff page.
pub mod diff;
/// Generic saved execution graph inspector.
pub(crate) mod execution_graph;
/// Graph proposal and live execution sidebar beside session chat.
pub(crate) mod graph_chat;
/// Project selection and activity list page.
pub mod project_list;
/// Session-linked forge-review panels embedded in Diff mode.
pub mod review_comment;
/// Session transcript and prompt page.
pub mod session_chat;
/// Grouped session list page.
pub mod session_list;
/// Project settings page.
pub mod setting;

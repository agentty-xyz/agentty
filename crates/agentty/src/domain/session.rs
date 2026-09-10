use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use ag_agent::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
pub use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, SessionRole,
    SessionStatus as Status, activity_day_key_with_offset,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::agent::{AgentSelection, ReasoningLevel};
use super::session_message::SessionTranscript;
use crate::domain::question::QuestionItem;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageBody, TransientMessageSlot, TransientMessageStore,
};
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment};

/// Folder name under a project root that stores Agentty session metadata.
pub const SESSION_DATA_DIR: &str = ".agentty";

/// Maximum number of stacked descendants in one root-to-child chain.
pub const MAX_STACK_DEPTH: usize = 5;

/// Full in-progress loader label shown while post-turn commit-message
/// generation and git commit orchestration are running.
pub(crate) const COMMITTING_PROGRESS_LABEL: &str = "Committing...";

/// Lead sentence used when seeding a follow-on prompt from a terminal session.
const TERMINAL_CONTINUATION_PROMPT_INTRO: &str =
    "Continue the work from this previous Agentty session.";

/// Size bucket derived from a session's git diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SessionSize {
    /// At most 10 changed lines.
    #[default]
    Xs,
    /// Between 11 and 30 changed lines.
    S,
    /// Between 31 and 80 changed lines.
    M,
    /// Between 81 and 200 changed lines.
    L,
    /// Between 201 and 500 changed lines.
    Xl,
    /// More than 500 changed lines.
    Xxl,
}

impl SessionSize {
    /// Ordered list of all session size buckets from smallest to largest.
    pub const ALL: [SessionSize; 6] = [
        SessionSize::Xs,
        SessionSize::S,
        SessionSize::M,
        SessionSize::L,
        SessionSize::Xl,
        SessionSize::Xxl,
    ];

    /// Classifies one git diff into a session size bucket.
    pub fn from_diff(diff: &str) -> Self {
        let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);
        let changed_line_count =
            usize::try_from(added_lines.saturating_add(deleted_lines)).unwrap_or(usize::MAX);

        Self::from_changed_line_count(changed_line_count)
    }

    fn from_changed_line_count(changed_line_count: usize) -> Self {
        match changed_line_count {
            0..=10 => SessionSize::Xs,
            11..=30 => SessionSize::S,
            31..=80 => SessionSize::M,
            81..=200 => SessionSize::L,
            201..=500 => SessionSize::Xl,
            _ => SessionSize::Xxl,
        }
    }

    /// Returns a short UI label for this size bucket.
    pub fn label(self) -> &'static str {
        match self {
            SessionSize::Xs => "XS",
            SessionSize::S => "S",
            SessionSize::M => "M",
            SessionSize::L => "L",
            SessionSize::Xl => "XL",
            SessionSize::Xxl => "XXL",
        }
    }
}

/// Result of refreshing diff-derived metadata for one session worktree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDiffStats {
    /// The worktree diff was loaded successfully.
    Known {
        /// Added line count parsed from the diff.
        added_lines: u64,
        /// Deleted line count parsed from the diff.
        deleted_lines: u64,
        /// Whether the diff contains any content, including binary-only or
        /// metadata-only changes.
        has_diff: bool,
        /// Size bucket derived from text line changes.
        session_size: SessionSize,
    },
    /// The worktree diff could not be loaded.
    Unknown,
}

impl SessionDiffStats {
    /// Derives known diff metadata from one successful Git diff response.
    pub fn from_diff(diff: &str) -> Self {
        let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);

        Self::Known {
            added_lines,
            deleted_lines,
            has_diff: !diff.trim().is_empty(),
            session_size: SessionSize::from_diff(diff),
        }
    }

    /// Returns the UI availability state represented by this refresh result.
    pub fn diff_state(self) -> SessionDiffState {
        match self {
            Self::Known { has_diff: true, .. } => SessionDiffState::Present,
            Self::Known {
                has_diff: false, ..
            } => SessionDiffState::Empty,
            Self::Unknown => SessionDiffState::Unknown,
        }
    }
}

impl fmt::Display for SessionSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

impl FromStr for SessionSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "XS" | "Xs" | "xs" => Ok(SessionSize::Xs),
            "S" | "s" => Ok(SessionSize::S),
            "M" | "m" => Ok(SessionSize::M),
            "L" | "l" => Ok(SessionSize::L),
            "XL" | "Xl" | "xl" => Ok(SessionSize::Xl),
            "XXL" | "Xxl" | "xxl" => Ok(SessionSize::Xxl),
            _ => Err(format!("Unknown session size: {s}")),
        }
    }
}

/// Session-view action currently available for manual session-branch
/// publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishBranchAction {
    /// Pushes the session branch to the configured Git remote.
    Push,
    /// Pushes the session branch and creates or refreshes the forge review
    /// request for it.
    PublishPullRequest,
}

/// Launch action currently available for one persisted follow-up task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowUpTaskAction {
    /// Starts a new sibling session from the selected task text.
    Launch,
    /// Opens the already launched sibling session linked to the task.
    Open,
}

/// Auto-push state for one already-published session branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PublishedBranchSyncStatus {
    /// No background sync push is currently active and the last push did not
    /// fail.
    #[default]
    Idle,
    /// A completed turn is currently pushing the published branch upstream.
    InProgress,
    /// The latest automatic push attempt updated the published branch.
    Succeeded,
    /// The latest automatic push attempt failed and left the branch stale.
    Failed,
}

/// Aggregated activity count for one day key.
///
/// `day_key` is the number of days since Unix epoch (`1970-01-01`).
/// App/session loading stores local day keys derived from immutable
/// session-creation activity history so heatmap remains visible after session
/// deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DailyActivity {
    /// Day key measured as whole days since Unix epoch.
    pub day_key: i64,
    /// Number of sessions created on the corresponding day.
    pub session_count: u32,
}

/// Persisted read-only follow-up task rendered alongside one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFollowUpTask {
    /// Stable database identifier for the persisted follow-up task row.
    pub id: i64,
    /// Previously launched sibling session linked to this task, when one has
    /// already been created.
    pub launched_session_id: Option<SessionId>,
    /// Stable display-order position persisted for this follow-up task.
    pub position: usize,
    /// User-visible task text emitted by the agent.
    pub text: String,
}

impl SessionFollowUpTask {
    /// Returns the action the session view should expose for this task.
    pub fn action(&self) -> FollowUpTaskAction {
        if self.launched_session_id.is_some() {
            return FollowUpTaskAction::Open;
        }

        FollowUpTaskAction::Launch
    }
}
/// In-memory snapshot of one persisted session row used by the UI and app
/// orchestration layers.
pub struct Session {
    /// Agent provider and model selected for this session.
    pub agent: AgentSelection,
    /// Base branch used to create the session worktree.
    pub base_branch: String,
    /// Controller session that owns this orchestration child, when present.
    pub controller_session_id: Option<SessionId>,
    /// Session creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Ordered image attachments staged for the draft-session prompt stored in
    /// `prompt` while the session remains `Draft`.
    pub draft_attachments: Vec<TurnPromptAttachment>,
    /// Planned or active worktree folder path for this session.
    pub folder: PathBuf,
    /// Persisted read-only follow-up tasks emitted after the latest turn.
    pub follow_up_tasks: Vec<SessionFollowUpTask>,
    /// Stable session identifier.
    pub id: SessionId,
    /// Unix timestamp when the current active-work interval started, if the
    /// session is presently accumulating `InProgress` time.
    pub in_progress_started_at: Option<i64>,
    /// Cumulative active-work time already completed by this session, in whole
    /// seconds.
    pub in_progress_total_seconds: i64,
    /// Whether the session was created through the explicit draft workflow
    /// from the sessions list.
    pub is_draft: bool,
    /// Derived orchestration progress rendered in place of a lifecycle label.
    pub orchestration_progress: Option<String>,
    /// Parent session this stacked session is based on while its parent branch
    /// remains active.
    pub parent_session_id: Option<SessionId>,
    /// Provider permission mode selected through the prompt shortcut.
    pub permission_mode: crate::domain::permission::PermissionMode,
    /// Workspace personality selected for future turns, when present.
    pub personality_id: Option<String>,
    /// Human-readable project name associated with the session.
    pub project_name: String,
    /// Initial user prompt used to create the session.
    pub prompt: String,
    /// Upstream reference recorded after the latest successful branch publish,
    /// for example `origin/wt/session-id`.
    pub published_upstream_ref: Option<String>,
    /// Model clarification questions emitted by the agent.
    pub questions: Vec<QuestionItem>,
    /// Chat messages queued while the active turn is running, mirrored from
    /// [`SessionHandles::queued_messages`] for render in submission order
    /// alongside queued workflow actions.
    pub queued_messages: Vec<QueuedMessage>,
    /// Session-scoped reasoning override selected through prompt slash
    /// commands.
    pub reasoning_level_override: Option<ReasoningLevel>,
    /// Presentation style requested for future model responses.
    pub response_style: ResponseStyle,
    /// Persisted forge review-request link for this session, when available.
    pub review_request: Option<ReviewRequest>,
    /// Role this session plays in multi-session orchestration.
    pub role: SessionRole,
    /// Derived size bucket computed from diff size.
    pub size: SessionSize,
    /// Response-speed preference selected through `/speed`.
    pub speed_mode: SpeedMode,
    /// Token usage statistics associated with this session.
    pub stats: SessionStats,
    /// Current lifecycle status.
    pub status: Status,
    /// Optional explicit session title.
    pub title: Option<String>,
    /// Typed transcript snapshot used by the UI when available.
    pub transcript: Option<SessionTranscript>,
    /// Last update timestamp (Unix seconds).
    pub updated_at: i64,
    /// Explicit non-durable output slots and their render lifecycle.
    pub(crate) transient_messages: TransientMessageStore,
}

impl Session {
    /// Returns the latest persisted user-prompt position for transient-message
    /// lifecycle binding.
    pub(crate) fn latest_user_prompt_position(&self) -> Option<i64> {
        self.transcript
            .as_ref()?
            .messages()
            .iter()
            .rev()
            .find_map(|message| message.kind.is_prompt().then_some(message.position))
    }

    /// Resolves turn-scoped messages when a snapshot leaves an active turn.
    pub(crate) fn reconcile_status_transition(&mut self, previous_status: Status) {
        if previous_status == Status::InProgress && self.status != Status::InProgress {
            self.transient_messages
                .retract(TransientMessageSlot::ReviewCommentResolution);
        }

        self.reconcile_transient_messages();
    }

    /// Applies turn-bound lifecycle cleanup after reducer-owned snapshot sync.
    pub(crate) fn reconcile_transient_messages(&mut self) {
        if matches!(self.status, Status::InProgress | Status::Queued)
            && let Some(active_turn_position) = self.latest_user_prompt_position()
        {
            self.transient_messages
                .clear_for_new_turn(active_turn_position);
        }
    }

    /// Returns the display title for this session.
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("No title")
    }

    /// Returns whether the session should use staged-draft behavior before
    /// its first live turn starts.
    pub fn is_draft_session(&self) -> bool {
        self.is_draft
    }

    /// Returns whether the session currently has one or more staged draft
    /// prompts waiting for an explicit start action.
    pub fn has_staged_drafts(&self) -> bool {
        self.is_draft_session() && self.status == Status::Draft && !self.prompt.is_empty()
    }

    /// Returns whether this session can parent a stacked draft.
    ///
    /// Unstarted standalone drafts are excluded because their worktree branch
    /// is deferred until start. Unstarted stacked drafts can stage descendants
    /// against their deterministic future branch, though each descendant must
    /// still wait for its immediate parent to reach review before starting.
    /// Terminal sessions no longer provide an active branch to stack on. The
    /// stack-wide depth policy is evaluated separately because it requires the
    /// loaded session graph.
    pub fn allows_stacked_child_creation(&self) -> bool {
        if self.is_draft_session()
            && self.status == Status::Draft
            && self.parent_session_id.is_none()
        {
            return false;
        }

        !matches!(
            self.status,
            Status::Merged | Status::Done | Status::Canceled
        )
    }

    /// Returns whether this session can be moved beneath another session.
    ///
    /// Appending changes the branch base and immediately starts a sync, so the
    /// source must be an independent, review-ready user-owned branch without
    /// a forge review request whose target would become stale.
    pub fn allows_stack_append(&self) -> bool {
        self.accepts_user_turns()
            && self.owns_branch_changes()
            && self.parent_session_id.is_none()
            && self.review_request.is_none()
            && self.status.allows_review_actions()
    }

    /// Returns whether this session can be forked into a new independent
    /// session branch.
    ///
    /// Forks start from the current session branch and snapshot durable
    /// transcript history, so the source must be a root session with a
    /// materialized branch in a review-ready state. Drafts are excluded
    /// because their worktree may not exist yet, stacked children are excluded
    /// because they remain coupled to parent stack workflow, and non-review
    /// statuses are excluded because active branch work or terminal cleanup
    /// could race with the snapshot.
    pub fn allows_fork_action(&self) -> bool {
        self.accepts_user_turns()
            && self.role.owns_branch_changes()
            && self.parent_session_id.is_none()
            && !self.is_draft_session()
            && self.status.allows_review_actions()
    }

    /// Returns whether the session can submit an agent reply for actionable
    /// forge review comments.
    pub fn allows_review_comment_reply(&self) -> bool {
        self.accepts_user_turns()
            && self.role.owns_branch_changes()
            && (self.status.allows_review_actions() || self.status == Status::Question)
    }

    /// Returns whether the session lifecycle and ownership role permit opening
    /// its materialized worktree.
    ///
    /// Managed orchestration workers remain unavailable for direct user turns,
    /// but a settled worker in `Review` may be opened for external inspection.
    pub fn allows_worktree_open_action(&self) -> bool {
        self.status.allows_session_actions()
            && (self.accepts_user_turns()
                || (self.role == SessionRole::OrchestrationWorker && self.status == Status::Review))
    }

    /// Returns whether this session exposes branch diff, merge, and publish
    /// affordances.
    pub fn owns_branch_changes(&self) -> bool {
        self.role.owns_branch_changes()
    }

    /// Returns whether direct user turns and branch mutations are allowed.
    pub fn accepts_user_turns(&self) -> bool {
        self.role.accepts_user_turns()
    }

    /// Returns whether this session is owned by an orchestration campaign.
    pub fn is_managed(&self) -> bool {
        self.role.is_managed()
    }

    /// Returns whether this session is stacked beneath another session branch.
    pub fn is_stacked_child(&self) -> bool {
        self.parent_session_id.is_some()
    }

    /// Returns whether the staged draft bundle can start its first live turn.
    pub fn can_start_staged_session(&self) -> bool {
        self.is_draft_session() && self.status == Status::Draft && self.has_staged_drafts()
    }

    /// Returns whether the session can be canceled by the user.
    ///
    /// Running sessions can be canceled from the list after their active turn
    /// is signaled to stop. Review-oriented sessions remain cancelable, and
    /// unstarted draft sessions can also be canceled before they materialize a
    /// worktree. Draft orchestrators remain cancelable after their controller
    /// worktree is materialized but before their first goal is submitted.
    pub fn allows_cancel_action(&self) -> bool {
        self.accepts_user_turns()
            && (self.status == Status::InProgress
                || self.status.allows_review_actions()
                || (self.status == Status::Draft
                    && (self.is_draft_session()
                        || self.role == SessionRole::Orchestrator
                        || self
                            .transient_messages
                            .get(TransientMessageSlot::WorkspacePreparation)
                            .is_some())))
    }

    /// Returns whether this terminal session can launch a seeded follow-on
    /// session from view mode.
    pub fn allows_terminal_continuation(&self) -> bool {
        self.role == SessionRole::Worker && self.status.allows_terminal_continuation()
    }

    /// Returns one seeded first-prompt body for a follow-on session launched
    /// from a terminal session view.
    pub fn continuation_prompt_seed(&self) -> Option<String> {
        if !self.allows_terminal_continuation() {
            return None;
        }

        let (context_label, context_text) = self.continuation_context()?;

        Some(format!(
            "{TERMINAL_CONTINUATION_PROMPT_INTRO}\n\nPrevious session: {}\nProject: {}\nStatus: \
             {}\n\n{context_label}:\n{context_text}\n",
            self.display_title(),
            self.project_name,
            self.status,
        ))
    }

    /// Returns whether session chat should render the cumulative active-work
    /// timer for this session.
    pub fn has_in_progress_timer(&self) -> bool {
        self.in_progress_total_seconds > 0 || self.in_progress_started_at.is_some()
    }

    /// Returns the session-persisted reasoning level used for the next turn.
    pub fn effective_reasoning_level(&self) -> ReasoningLevel {
        self.reasoning_level_override.unwrap_or_default()
    }

    /// Returns cumulative active-work time including any open `InProgress`
    /// interval measured at `wall_clock_unix_seconds`.
    pub fn in_progress_duration_seconds(&self, wall_clock_unix_seconds: i64) -> i64 {
        let open_interval_seconds = self.in_progress_started_at.map_or(0, |started_at| {
            wall_clock_unix_seconds.saturating_sub(started_at).max(0)
        });

        self.in_progress_total_seconds
            .saturating_add(open_interval_seconds)
    }

    /// Returns a short forge indicator suffix for the session list status
    /// column.
    ///
    /// The indicator reflects the most specific known forge state:
    /// - `↑` when the branch was pushed but no review request is linked.
    /// - `⊙ #N` when a linked review request is open.
    /// - `✓ #N` when a linked review request was merged.
    /// - `✗ #N` when a linked review request was closed without merge.
    /// - Empty when neither published nor linked.
    pub fn forge_indicator(&self) -> String {
        if let Some(review_request) = &self.review_request {
            let display_id = &review_request.summary.display_id;

            return match review_request.summary.state {
                ReviewRequestState::Open => format!("⊙ {display_id}"),
                ReviewRequestState::Merged => format!("✓ {display_id}"),
                ReviewRequestState::Closed => format!("✗ {display_id}"),
            };
        }

        if self.published_upstream_ref.is_some() {
            return "↑".to_string();
        }

        String::new()
    }

    /// Returns whether this session has a linked forge review request.
    pub fn has_review_request(&self) -> bool {
        self.review_request.is_some()
    }

    /// Returns whether this session can trigger a forge review request sync.
    ///
    /// Sync is available when the session has a published branch or a linked
    /// review request and the status allows review actions.
    pub fn can_sync_review_request(&self) -> bool {
        let has_forge_context = self.published_upstream_ref.is_some() || self.has_review_request();

        has_forge_context && matches!(self.status, Status::Review | Status::AgentReview)
    }

    /// Returns the review-request publish action currently available in session
    /// view, including queueing behind active turn or rebase work.
    pub fn publish_pull_request_action(&self) -> Option<PublishBranchAction> {
        let is_publish_active = self
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_some_and(|message| message.body.is_pending_indicator());

        (self.accepts_user_turns()
            && self.owns_branch_changes()
            && (self.status.allows_review_actions()
                || matches!(self.status, Status::InProgress | Status::Rebasing))
            && !is_publish_active)
            .then_some(PublishBranchAction::PublishPullRequest)
    }

    /// Returns the follow-up task at `position`, when present.
    pub fn follow_up_task(&self, position: usize) -> Option<&SessionFollowUpTask> {
        self.follow_up_tasks
            .iter()
            .find(|task| task.position == position)
    }

    /// Returns the best persisted context section for a continuation prompt.
    fn continuation_context(&self) -> Option<(&'static str, String)> {
        self.non_empty_transcript()
            .map(|transcript| ("Previous session transcript", transcript))
            .or_else(|| {
                self.non_empty_prompt()
                    .map(|prompt| ("Previous session prompt", prompt.to_string()))
            })
    }

    /// Returns the formatted transcript text when it is non-empty.
    fn non_empty_transcript(&self) -> Option<String> {
        self.transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .and_then(|transcript| {
                Self::trimmed_non_empty_text(&transcript).map(ToString::to_string)
            })
    }

    /// Returns the trimmed persisted initial prompt when it is non-empty.
    fn non_empty_prompt(&self) -> Option<&str> {
        Self::trimmed_non_empty_text(&self.prompt)
    }

    /// Returns `value` trimmed to a non-empty slice when any content remains.
    fn trimmed_non_empty_text(value: &str) -> Option<&str> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }
}

/// Returns whether `parent_session_id` can parent another stacked draft
/// without exceeding [`MAX_STACK_DEPTH`].
pub(crate) fn can_create_stacked_child(sessions: &[Session], parent_session_id: &str) -> bool {
    let Some(parent_session) = find_session(sessions, parent_session_id) else {
        return false;
    };

    parent_session.allows_stacked_child_creation()
        && session_stack_depth(sessions, parent_session_id)
            .is_some_and(|depth| depth < MAX_STACK_DEPTH)
}

/// Returns whether one review-ready root session can be moved beneath
/// `parent_session_id` and synchronized as a stacked child.
pub(crate) fn can_append_session_to_stack(
    sessions: &[Session],
    session_id: &str,
    parent_session_id: &str,
) -> bool {
    if session_id == parent_session_id {
        return false;
    }
    let Some(session) = find_session(sessions, session_id) else {
        return false;
    };
    let Some(parent_session) = find_session(sessions, parent_session_id) else {
        return false;
    };

    session.allows_stack_append()
        && !sessions.iter().any(|candidate| {
            candidate
                .parent_session_id
                .as_deref()
                .is_some_and(|parent_id| parent_id == session_id)
        })
        && parent_session.accepts_user_turns()
        && parent_session.owns_branch_changes()
        && parent_session.status.allows_review_actions()
        && can_create_stacked_child(sessions, parent_session_id)
        && can_rebase_session_branch_in_stack(sessions, parent_session_id)
}

/// Returns whether the staged draft identified by `session_id` can start
/// under the currently loaded stack.
///
/// Root drafts only need their own staged prompt state. Stacked drafts also
/// require a review-ready immediate parent and no other branch work already
/// running or queued in the same stack.
pub(crate) fn can_start_staged_session_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };
    let session = stack.requested_session();
    if !session.can_start_staged_session() {
        return false;
    }

    if session.parent_session_id.is_none() {
        return true;
    }
    if !stack.parent_allows_stacked_child_start() {
        return false;
    }

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether the session identified by `session_id` can start slash
/// command branch mutation while preserving one active branch worker per
/// stack.
///
/// This blocks parent branch edits once a child branch has materialized, and
/// blocks any stack member from starting branch work while a different member
/// is already running, queued, rebasing, merging, or waiting on a question.
pub(crate) fn can_mutate_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    if stack.has_branch_mutating_member_except(session_id) {
        return false;
    }

    if stack.has_materialized_descendant() {
        return false;
    }

    true
}

/// Returns whether a session can enter the merge queue while preserving stack
/// consistency.
///
/// A linked forge review request disables local merge queueing so the remote
/// review remains the only merge path. Otherwise, merging a parent with idle
/// materialized children is allowed because the successful parent merge
/// retargets and syncs the children afterward. Active stack members still
/// block the request so the stack does not run competing branch work.
pub(crate) fn can_merge_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    stack.requested_session.review_request.is_none()
        && !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a session can start session sync while preserving stack
/// consistency.
///
/// Like merge, syncing a parent with idle materialized children is allowed
/// because the successful parent sync fans out child syncs afterward. Active
/// stack members still block the request so the stack does not run competing
/// branch work.
pub(crate) fn can_rebase_session_branch_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a session can accept a chat reply under stack
/// constraints.
///
/// Replies are allowed when the stack has no other member actively running or
/// reserving branch work. Unlike merge or sync gates, an idle review-ready
/// materialized child does not block parent replies; the child can be synced
/// again after the parent produces its next review state.
pub(crate) fn can_reply_to_session_in_stack(sessions: &[Session], session_id: &str) -> bool {
    let Some(stack) = SessionStack::for_session(sessions, session_id) else {
        return false;
    };

    !stack.has_branch_mutating_member_except(session_id)
}

/// Returns whether a caller-owned branch reservation belongs to this stack.
/// Missing or invalid stacks fail closed, matching the branch-work gates.
pub(crate) fn has_reserved_branch_work_in_stack(
    sessions: &[Session],
    session_id: &str,
    mut is_reserved: impl FnMut(&str) -> bool,
) -> bool {
    SessionStack::for_session(sessions, session_id)
        .is_none_or(|stack| stack.members.iter().any(|member| is_reserved(&member.id)))
}

/// Snapshot of one loaded stack tree for branch-work policy checks.
struct SessionStack<'a> {
    members: Vec<&'a Session>,
    requested_session: &'a Session,
}

impl<'a> SessionStack<'a> {
    /// Builds the stack containing `session_id` from the loaded session list.
    fn for_session(sessions: &'a [Session], session_id: &str) -> Option<Self> {
        let requested_session = find_session(sessions, session_id)?;
        let root_session = stack_root_session(sessions, requested_session)?;
        let members = sessions
            .iter()
            .filter(|session| session_stack_root_id(sessions, session) == Some(&root_session.id))
            .collect();

        Some(Self {
            members,
            requested_session,
        })
    }

    /// Returns the session whose action is being evaluated.
    fn requested_session(&self) -> &'a Session {
        self.requested_session
    }

    /// Returns whether another stack member is currently reserving or
    /// performing branch-mutating work.
    fn has_branch_mutating_member_except(&self, ignored_session_id: &str) -> bool {
        self.members
            .iter()
            .filter(|session| session.id.as_str() != ignored_session_id)
            .any(|session| session.status.is_stack_branch_mutating())
    }

    /// Returns whether the requested session has a non-terminal descendant
    /// branch that has started at least one live turn.
    fn has_materialized_descendant(&self) -> bool {
        self.members.iter().any(|session| {
            session.id != self.requested_session.id
                && session_is_descendant_of(
                    &self.members,
                    session,
                    self.requested_session.id.as_str(),
                )
                && !matches!(
                    session.status,
                    Status::Draft | Status::Merged | Status::Done | Status::Canceled
                )
        })
    }

    /// Returns whether the immediate parent is in a state that lets the
    /// requested stacked draft materialize.
    ///
    /// The caller handles root drafts before invoking this stacked-only gate.
    fn parent_allows_stacked_child_start(&self) -> bool {
        self.requested_session
            .parent_session_id
            .as_ref()
            .is_some_and(|parent_session_id| {
                self.members.iter().any(|session| {
                    session.id == *parent_session_id && session.status.allows_stacked_child_start()
                })
            })
    }
}

/// Returns the zero-based stack depth of a session, rejecting missing or
/// cyclic parent chains. Root sessions have depth zero.
fn session_stack_depth(sessions: &[Session], session_id: &str) -> Option<usize> {
    let mut current_session = find_session(sessions, session_id)?;
    let mut visited_session_ids = Vec::new();
    let mut depth = 0;

    while let Some(parent_session_id) = current_session.parent_session_id.as_ref() {
        if visited_session_ids.contains(&current_session.id) {
            return None;
        }
        visited_session_ids.push(current_session.id.clone());
        current_session = find_session(sessions, parent_session_id.as_str())?;
        depth += 1;
    }

    (!visited_session_ids.contains(&current_session.id)).then_some(depth)
}

/// Returns the root session for a valid loaded parent chain.
fn stack_root_session<'a>(sessions: &'a [Session], session: &'a Session) -> Option<&'a Session> {
    let depth = session_stack_depth(sessions, session.id.as_str())?;
    let mut root_session = session;

    for _ in 0..depth {
        root_session = find_session(sessions, root_session.parent_session_id.as_deref()?)?;
    }

    Some(root_session)
}

/// Returns the root id for a session with a valid loaded parent chain.
fn session_stack_root_id<'a>(
    sessions: &'a [Session],
    session: &'a Session,
) -> Option<&'a SessionId> {
    stack_root_session(sessions, session).map(|root_session| &root_session.id)
}

/// Returns whether `session` descends from `ancestor_session_id` within the
/// already-connected stack member set.
fn session_is_descendant_of(
    members: &[&Session],
    session: &Session,
    ancestor_session_id: &str,
) -> bool {
    let mut current_session = session;

    while let Some(parent_session_id) = current_session.parent_session_id.as_ref() {
        if parent_session_id.as_str() == ancestor_session_id {
            return true;
        }
        let Some(parent_session) = members
            .iter()
            .find(|candidate| candidate.id == *parent_session_id)
        else {
            return false;
        };
        current_session = parent_session;
    }

    false
}

/// Finds one loaded session by id.
fn find_session<'a>(sessions: &'a [Session], session_id: &str) -> Option<&'a Session> {
    sessions
        .iter()
        .find(|session| session.id.as_str() == session_id)
}

/// One chat prompt waiting behind active session work.
///
/// `order` comes from the same session-local sequence as queued workflow
/// actions, allowing the worker and renderer to preserve one FIFO order
/// across both kinds of work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedMessage {
    order: u64,
    prompt: TurnPrompt,
    transcript_text: String,
}

impl QueuedMessage {
    /// Creates one queued chat prompt at its reserved submission order.
    pub(crate) fn new(order: u64, prompt: TurnPrompt) -> Self {
        let transcript_text = prompt.transcript_text();

        Self {
            order,
            prompt,
            transcript_text,
        }
    }

    /// Consumes the queue entry and returns its structured prompt.
    pub(crate) fn into_prompt(self) -> TurnPrompt {
        self.prompt
    }

    /// Returns the session-local submission order shared with queued actions.
    pub(crate) fn order(&self) -> u64 {
        self.order
    }

    /// Returns the structured prompt without consuming the queue entry.
    pub(crate) fn prompt(&self) -> &TurnPrompt {
        &self.prompt
    }

    /// Returns the transcript rendering of the queued prompt.
    pub(crate) fn transcript_text(&self) -> &str {
        &self.transcript_text
    }
}

/// Shared runtime handles for one active session worker.
pub struct SessionHandles {
    /// Serializes branch-publish ownership with queued branch operations.
    ///
    /// The guard is held across async persistence and push work, so this is
    /// intentionally an async mutex rather than [`std::sync::Mutex`].
    pub branch_operation_lock: Arc<AsyncMutex<()>>,
    /// Per-turn cancellation token shared between the UI and the worker.
    ///
    /// The worker swaps in a fresh [`CancellationToken`] at the start of
    /// each turn. The UI calls `cancel()` on the current token to
    /// interrupt the running turn. Because each turn gets its own token,
    /// stale cancellations from previous turns cannot affect new work.
    pub cancel_token: Arc<Mutex<CancellationToken>>,
    /// Child process identifier for the running agent command, when present.
    pub child_pid: Arc<Mutex<Option<u32>>>,
    /// In-memory queue of prompts staged while the current turn is running.
    ///
    /// Pushed by the chat composer when the user submits while the session is
    /// `InProgress`; popped by the session worker between turns. The queue is
    /// session-local and discarded on app restart.
    pub queued_messages: Arc<Mutex<VecDeque<QueuedMessage>>>,
    /// Monotonic submission order shared by queued chat and workflow actions.
    pub queued_work_sequence: Arc<AtomicU64>,
    /// Shared mutable status synchronized with persistence/UI.
    pub status: Arc<Mutex<Status>>,
    /// Shared typed transcript snapshot mirrored to the render layer.
    pub transcript: Arc<Mutex<SessionTranscript>>,
    /// Queued workflow rows that must survive active-project snapshot reloads.
    queued_actions: Arc<Mutex<TransientMessageStore>>,
    /// Whether [`Self::transcript`] contains the complete persisted history.
    ///
    /// Lazy session-list handles start unhydrated so background workflow
    /// notices cannot make a partial transcript look authoritative.
    transcript_is_hydrated: AtomicBool,
}

impl SessionHandles {
    /// Creates handles with a loaded, empty transcript.
    pub fn new(status: Status) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            transcript_is_hydrated: AtomicBool::new(true),
        }
    }

    /// Creates handles whose persisted transcript has not been loaded yet.
    pub(crate) fn new_unloaded(status: Status) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            transcript_is_hydrated: AtomicBool::new(false),
        }
    }

    /// Creates handles initialized with a typed transcript snapshot.
    pub fn new_with_transcript(status: Status, transcript: SessionTranscript) -> Self {
        Self {
            branch_operation_lock: Arc::new(AsyncMutex::new(())),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            child_pid: Arc::new(Mutex::new(None)),
            queued_actions: Arc::new(Mutex::new(TransientMessageStore::default())),
            queued_messages: Arc::new(Mutex::new(VecDeque::new())),
            queued_work_sequence: Arc::new(AtomicU64::new(0)),
            status: Arc::new(Mutex::new(status)),
            transcript: Arc::new(Mutex::new(transcript)),
            transcript_is_hydrated: AtomicBool::new(true),
        }
    }

    /// Returns the live transcript, hydrating an unloaded handle from the
    /// persisted snapshot even when background notices made it non-empty.
    pub(crate) fn transcript_snapshot_with_loaded(
        &self,
        loaded_transcript: Option<&SessionTranscript>,
    ) -> Option<SessionTranscript> {
        let Ok(mut transcript) = self.transcript.lock() else {
            return None;
        };
        if !self.transcript_is_hydrated.load(Ordering::Acquire)
            && let Some(loaded_transcript) = loaded_transcript
        {
            *transcript = Self::merge_unloaded_transcript(loaded_transcript, &transcript);
            self.transcript_is_hydrated.store(true, Ordering::Release);
        }
        if transcript.is_empty() {
            return None;
        }

        Some(transcript.clone())
    }

    /// Reserves the next shared submission order for queued session work.
    pub(crate) fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns queued chat messages in submission order so callers can mirror
    /// queue contents into render snapshots.
    pub fn queued_message_snapshot(&self) -> Vec<QueuedMessage> {
        // Sync critical section (read-only clone, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
        // Selection".
        self.queued_messages
            .lock()
            .map(|guard| guard.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    /// Stores one queued workflow row beside the worker-owned queue state.
    pub(crate) fn upsert_queued_action(&self, message: TransientMessage) {
        debug_assert!(matches!(&message.body, TransientMessageBody::Queued(_)));
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            queued_actions.upsert(message);
        }
    }

    /// Removes one queued workflow row after its command starts or resolves.
    pub(crate) fn resolve_queued_action(&self, slot: TransientMessageSlot) {
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            queued_actions.retract(slot);
        }
    }

    /// Removes all queued workflow rows during terminal cancellation.
    pub(crate) fn clear_queued_actions(&self) {
        if let Ok(mut queued_actions) = self.queued_actions.lock() {
            *queued_actions = TransientMessageStore::default();
        }
    }

    /// Returns queued workflow rows in their stable display order.
    pub(crate) fn queued_action_snapshot(&self) -> Vec<TransientMessage> {
        self.queued_actions
            .lock()
            .map(|queued_actions| queued_actions.messages().to_vec())
            .unwrap_or_default()
    }

    /// Merges messages appended while persistence was in flight into a
    /// database snapshot, deduplicating exact matches and retaining conflicts.
    fn merge_unloaded_transcript(
        loaded_transcript: &SessionTranscript,
        live_transcript: &SessionTranscript,
    ) -> SessionTranscript {
        let mut messages = loaded_transcript.messages().to_vec();
        for live_message in live_transcript.messages() {
            if let Some(loaded_message) = messages
                .iter()
                .find(|message| message.position == live_message.position)
            {
                if loaded_message == live_message {
                    continue;
                }

                let next_position = messages
                    .last()
                    .map_or(0, |message| message.position.saturating_add(1));
                let mut appended_message = live_message.clone();
                appended_message.position = next_position;
                messages.push(appended_message);
            } else {
                messages.push(live_message.clone());
            }
        }

        SessionTranscript::new(messages)
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;

//! In-memory session model used by the UI and app layers, with status-based
//! action gates.

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use ag_contracts::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
pub use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId,
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

/// Full in-progress loader label shown while post-turn commit-message
/// generation and git commit orchestration are running.
pub(crate) const COMMITTING_PROGRESS_LABEL: &str = "Committing...";

/// Status line shown while a session watches its linked review request.
const WATCHING_STATUS_MESSAGE: &str =
    "Watching review request for failing checks, comments, and conflicts...";

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
    Small,
    /// Between 31 and 80 changed lines.
    Medium,
    /// Between 81 and 200 changed lines.
    Large,
    /// Between 201 and 500 changed lines.
    Xl,
    /// More than 500 changed lines.
    Xxl,
}

impl SessionSize {
    /// Ordered list of all session size buckets from smallest to largest.
    pub const ALL: [SessionSize; 6] = [
        SessionSize::Xs,
        SessionSize::Small,
        SessionSize::Medium,
        SessionSize::Large,
        SessionSize::Xl,
        SessionSize::Xxl,
    ];

    /// Classifies added and deleted line totals into a session size bucket.
    pub fn from_line_counts(added_lines: u64, deleted_lines: u64) -> Self {
        let changed_line_count =
            usize::try_from(added_lines.saturating_add(deleted_lines)).unwrap_or(usize::MAX);

        Self::from_changed_line_count(changed_line_count)
    }

    fn from_changed_line_count(changed_line_count: usize) -> Self {
        match changed_line_count {
            0..=10 => SessionSize::Xs,
            11..=30 => SessionSize::Small,
            31..=80 => SessionSize::Medium,
            81..=200 => SessionSize::Large,
            201..=500 => SessionSize::Xl,
            _ => SessionSize::Xxl,
        }
    }

    /// Returns a short UI label for this size bucket.
    pub fn label(self) -> &'static str {
        match self {
            SessionSize::Xs => "XS",
            SessionSize::Small => "S",
            SessionSize::Medium => "M",
            SessionSize::Large => "L",
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
        /// Added line count across the worktree diff.
        added_lines: u64,
        /// Deleted line count across the worktree diff.
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
    /// Derives known diff metadata from successful worktree line totals.
    ///
    /// `has_diff` covers binary-only and metadata-only changes, which add no
    /// text lines.
    pub fn from_line_counts(added_lines: u64, deleted_lines: u64, has_diff: bool) -> Self {
        Self::Known {
            added_lines,
            deleted_lines,
            has_diff,
            session_size: SessionSize::from_line_counts(added_lines, deleted_lines),
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
            "S" | "s" => Ok(SessionSize::Small),
            "M" | "m" => Ok(SessionSize::Medium),
            "L" | "l" => Ok(SessionSize::Large),
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

/// Session-view action currently available for review-request watching.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewWatchToggle {
    /// Asks to watch the open linked review request.
    Start,
    /// Stops watching and returns the session to `Review`.
    Stop,
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
    /// Session creation timestamp (Unix seconds).
    pub created_at: i64,
    /// Ordered image attachments staged for the draft-session prompt stored in
    /// `prompt` while the session remains `Draft`.
    pub draft_attachments: Vec<TurnPromptAttachment>,
    /// Planned or active worktree folder path for this session.
    pub folder: PathBuf,
    /// Persisted read-only follow-up tasks emitted after the latest turn.
    pub follow_up_tasks: Vec<SessionFollowUpTask>,
    /// Whether retained archive storage contains a nonempty diff.
    pub has_archived_diff: bool,
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
    /// Workspace agent selected for future turns, when present.
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
    /// Derived size bucket computed from diff size.
    pub size: SessionSize,
    /// Response-speed preference toggled through `/fast`.
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
                .retract(TransientMessageSlot::GeneratedTurnProgress);
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

    /// Returns whether the session has not received its first user message.
    pub fn awaits_first_message(&self) -> bool {
        self.status == Status::Draft && self.prompt.is_empty()
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

    /// Returns whether this session can be forked into a new independent
    /// session branch.
    ///
    /// Forks start from the current session branch and snapshot durable
    /// transcript history, so the source must have a materialized branch in a
    /// review-ready state. Drafts are excluded because their worktree may not
    /// exist yet, and non-review statuses are excluded because active branch
    /// work or terminal cleanup could race with the snapshot.
    pub fn allows_fork_action(&self) -> bool {
        !self.is_draft_session() && self.status.allows_review_actions()
    }

    /// Returns whether the session can submit an agent reply for actionable
    /// forge review comments.
    pub fn allows_review_comment_reply(&self) -> bool {
        (self.status.allows_review_actions()
            || matches!(
                self.status,
                Status::Question | Status::InProgress | Status::Rebasing
            ))
            && !self.has_queued_generated_turn()
    }

    /// Returns whether one Agentty-generated turn already waits in the shared
    /// worker queue, which holds at most one such turn per session.
    pub(crate) fn has_queued_generated_turn(&self) -> bool {
        self.transient_messages
            .get(TransientMessageSlot::GeneratedTurnQueue)
            .is_some()
    }

    /// Returns whether selected comments must wait for clarification or join
    /// existing ordered worker work.
    pub(crate) fn queues_review_comment_reply(&self) -> bool {
        self.status == Status::Question || self.has_pending_worker_work()
    }

    /// Returns whether chat input queues behind an active turn or rebase.
    ///
    /// The composer then offers only setting slash commands, and model or
    /// speed changes wait until the session worker is idle.
    pub fn queues_chat_messages(&self) -> bool {
        matches!(self.status, Status::InProgress | Status::Rebasing)
    }

    /// Returns whether the session worker still owns accepted work: an active
    /// turn or rebase, queued chat messages, queued worker actions, or a
    /// queued or running branch publish.
    ///
    /// A started review-request publish replaces its queued notice with a
    /// loading one while the session stays in `Review`, so it counts until it
    /// finishes; pausing the worker for a model switch would otherwise block
    /// on it. Queued setting changes are not worker work: they wait for it.
    pub(crate) fn has_pending_worker_work(&self) -> bool {
        self.queues_chat_messages()
            || !self.queued_messages.is_empty()
            || self.transient_messages.messages().iter().any(|message| {
                (matches!(message.body, TransientMessageBody::Queued(_))
                    && message.slot != TransientMessageSlot::SettingsQueue)
                    || (message.slot == TransientMessageSlot::BranchPublish
                        && message.body.is_pending_indicator())
            })
    }

    /// Returns whether the session lifecycle permits opening its worktree.
    pub fn allows_worktree_open_action(&self) -> bool {
        self.status.allows_session_actions()
    }

    /// Returns whether the staged draft bundle can start its first live turn.
    pub fn can_start_staged_session(&self) -> bool {
        self.is_draft_session() && self.status == Status::Draft && self.has_staged_drafts()
    }

    /// Returns whether the oldest queued chat message can join the running
    /// turn now instead of waiting for the next one.
    pub fn can_steer_queued_messages(&self) -> bool {
        self.status == Status::InProgress
            && self
                .queued_messages
                .first()
                .is_some_and(QueuedMessage::is_steerable)
    }

    /// Returns whether the session can be canceled by the user.
    ///
    /// Running sessions can be canceled from the list after their active turn
    /// is signaled to stop. Review-oriented sessions and sessions waiting on
    /// clarification questions remain cancelable, and unstarted draft sessions
    /// can also be canceled before they materialize a worktree.
    pub fn allows_cancel_action(&self) -> bool {
        matches!(self.status, Status::InProgress | Status::Question)
            || self.status.allows_review_actions()
            || (self.status == Status::Draft
                && (self.is_draft_session()
                    || self
                        .transient_messages
                        .get(TransientMessageSlot::WorkspacePreparation)
                        .is_some()))
    }

    /// Returns whether this terminal session can launch a seeded follow-on
    /// session from view mode.
    pub fn allows_terminal_continuation(&self) -> bool {
        self.status.allows_terminal_continuation()
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

        has_forge_context && self.status.allows_review_actions()
    }

    /// Returns the review-request publish action currently available in session
    /// view, including queueing behind active turn or rebase work.
    pub fn publish_pull_request_action(&self) -> Option<PublishBranchAction> {
        let is_publish_active = self
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_some_and(|message| message.body.is_pending_indicator());

        ((self.status.allows_review_actions()
            || matches!(self.status, Status::InProgress | Status::Rebasing))
            && !is_publish_active)
            .then_some(PublishBranchAction::PublishPullRequest)
    }

    /// Returns the review-request watch action currently available in session
    /// view.
    ///
    /// Watching starts from idle `Review` with an open linked review request
    /// and stops from `Watching`.
    pub fn review_watch_toggle(&self) -> Option<ReviewWatchToggle> {
        match self.status {
            Status::Watching => Some(ReviewWatchToggle::Stop),
            Status::Review => self
                .review_request
                .as_ref()
                .is_some_and(|review_request| {
                    review_request.summary.state == ReviewRequestState::Open
                })
                .then_some(ReviewWatchToggle::Start),
            _ => None,
        }
    }

    /// Returns the follow-up task at `position`, when present.
    pub fn follow_up_task(&self, position: usize) -> Option<&SessionFollowUpTask> {
        self.follow_up_tasks
            .iter()
            .find(|task| task.position == position)
    }

    /// Returns the line that ends the transcript while the session works or
    /// waits, such as `Working... Reading files`, or `None` when its status
    /// shows none.
    ///
    /// `active_progress` is the running turn's latest compact thought.
    pub(crate) fn status_line(&self, active_progress: Option<&str>) -> Option<String> {
        let message = status_message(
            self.status,
            active_progress,
            self.review_loading_message(),
            self.generated_turn_progress_message(),
        );
        let message = message.trim();

        (!message.is_empty()).then(|| message.to_string())
    }

    /// Returns the labels of the workflow actions queued behind the active
    /// turn, such as a sync or a publish, in the order they run.
    pub(crate) fn queued_action_labels(&self) -> Vec<String> {
        let mut queued_actions = self
            .transient_messages
            .messages()
            .iter()
            .filter_map(|message| {
                if let TransientMessageBody::Queued(queued_action) = &message.body {
                    Some(queued_action)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        queued_actions.sort_by_key(|queued_action| queued_action.order);

        queued_actions
            .into_iter()
            .map(|queued_action| queued_action.text.clone())
            .collect()
    }

    /// Returns the loading text of a running review, when one runs.
    pub(crate) fn review_loading_message(&self) -> Option<&str> {
        self.loading_message(TransientMessageSlot::Review)
    }

    /// Returns the loading text of a generated turn, such as a review-comment
    /// resolution, when one runs.
    pub(crate) fn generated_turn_progress_message(&self) -> Option<&str> {
        self.loading_message(TransientMessageSlot::GeneratedTurnProgress)
    }

    /// Returns the loading text held in `slot`, when it holds one.
    fn loading_message(&self, slot: TransientMessageSlot) -> Option<&str> {
        self.transient_messages
            .get(slot)
            .and_then(|message| match &message.body {
                TransientMessageBody::Loading(message) => Some(message.as_str()),
                TransientMessageBody::Markdown(_)
                | TransientMessageBody::Plain(_)
                | TransientMessageBody::Queued(_) => None,
            })
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

/// Returns the status text for active session states, or an empty string for
/// states that show none.
///
/// Most in-progress details are agent thinking snippets appended to the
/// generic working label. Post-turn auto-commit sends a complete loader label
/// so commit-message generation and git commit work render as committing.
pub(crate) fn status_message(
    status: Status,
    active_progress: Option<&str>,
    review_status_message: Option<&str>,
    review_comment_resolution_message: Option<&str>,
) -> String {
    match status {
        Status::InProgress => review_comment_resolution_message
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map_or_else(
                || {
                    active_progress
                        .map(str::trim)
                        .filter(|progress| !progress.is_empty())
                        .map_or_else(
                            || "Working...".to_string(),
                            |progress| {
                                if progress == COMMITTING_PROGRESS_LABEL {
                                    progress.to_string()
                                } else {
                                    format!("Working... {progress}")
                                }
                            },
                        )
                },
                ToString::to_string,
            ),
        Status::AgentReview => review_status_message
            .map(str::trim)
            .filter(|status_message| !status_message.is_empty())
            .unwrap_or("Reviewing changes...")
            .to_string(),
        Status::Queued => "Waiting in merge queue...".to_string(),
        Status::Rebasing => "Rebasing...".to_string(),
        Status::Merging => "Merging...".to_string(),
        Status::Merged => "Waiting for manual local target sync...".to_string(),
        Status::Watching => WATCHING_STATUS_MESSAGE.to_string(),
        Status::Draft | Status::Review | Status::Question | Status::Done | Status::Canceled => {
            String::new()
        }
    }
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
    steerable: bool,
    transcript_text: String,
}

impl QueuedMessage {
    /// Creates one queued chat prompt at its reserved submission order.
    pub(crate) fn new(order: u64, prompt: TurnPrompt) -> Self {
        let transcript_text = prompt.transcript_text();

        Self {
            order,
            prompt,
            steerable: false,
            transcript_text,
        }
    }

    /// Marks whether the prompt can join the running turn instead of
    /// waiting for the next one.
    ///
    /// Prompts with image attachments always wait: their temporary files
    /// are released when a turn that owns them ends.
    #[must_use]
    pub(crate) fn with_steering(mut self, harness_steers: bool) -> Self {
        self.steerable = harness_steers && !self.prompt.has_attachments();

        self
    }

    /// Returns whether the prompt can be sent into the running turn now.
    pub(crate) fn is_steerable(&self) -> bool {
        self.steerable
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
    /// Whether the worker is running a chat turn on the session's own
    /// conversation.
    ///
    /// Queued prompts can join only that turn. Agent-task and graph turns
    /// also hold the session `InProgress`, but run in separate conversations.
    pub steerable_turn: Arc<AtomicBool>,
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
            steerable_turn: Arc::default(),
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
            steerable_turn: Arc::default(),
            transcript: Arc::new(Mutex::new(SessionTranscript::default())),
            transcript_is_hydrated: AtomicBool::new(false),
        }
    }

    /// Reports whether persisted history still needs to be loaded before a
    /// turn.
    pub(crate) fn needs_transcript_hydration(&self) -> bool {
        !self.transcript_is_hydrated.load(Ordering::Acquire)
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
            steerable_turn: Arc::default(),
            transcript: Arc::new(Mutex::new(transcript)),
            transcript_is_hydrated: AtomicBool::new(true),
        }
    }

    /// Returns the live transcript, hydrating an unloaded handle from the
    /// persisted snapshot even when background notices made it non-empty.
    ///
    /// `previous_snapshot` is a render copy taken from the snapshot being
    /// replaced. It is returned as-is when it still equals the live
    /// transcript, so a reload does not deep-clone unchanged history.
    pub(crate) fn transcript_snapshot_with_loaded(
        &self,
        loaded_transcript: Option<&SessionTranscript>,
        previous_snapshot: Option<SessionTranscript>,
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

        Some(
            previous_snapshot
                .filter(|previous_snapshot| *previous_snapshot == *transcript)
                .unwrap_or_else(|| transcript.clone()),
        )
    }

    /// Reserves the next shared submission order for queued session work.
    pub(crate) fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns queued chat messages in submission order so callers can mirror
    /// queue contents into render snapshots.
    ///
    /// The copies are steerable only while [`Self::steerable_turn`] is set,
    /// so the snapshot never offers to send a prompt into a turn that cannot
    /// receive it.
    pub fn queued_message_snapshot(&self) -> Vec<QueuedMessage> {
        let has_steerable_turn = self.has_steerable_turn();

        // Sync critical section (read-only clone, no `.await`);
        // `std::sync::Mutex` is the correct choice per CLAUDE.md §"Mutex
        // Selection".
        self.queued_messages.lock().map_or_default(|guard| {
            guard
                .iter()
                .cloned()
                .map(|message| {
                    let is_steerable = has_steerable_turn && message.is_steerable();

                    message.with_steering(is_steerable)
                })
                .collect::<Vec<_>>()
        })
    }

    /// Returns whether the worker is running a chat turn that queued prompts
    /// can join.
    pub(crate) fn has_steerable_turn(&self) -> bool {
        self.steerable_turn.load(Ordering::Acquire)
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
            .map_or_default(|queued_actions| queued_actions.messages().to_vec())
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

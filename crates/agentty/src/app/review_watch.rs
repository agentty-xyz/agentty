//! Review-request watching: the opt-in offer, `Watching` lifecycle
//! reconciliation, and automatic follow-up for merge conflicts, review
//! comments, and failing checks.
//!
//! The background sync orchestrator reports forge facts for sessions resting
//! in `Watching`. The foreground reducer selects at most one follow-up per
//! report and starts it through the ordinary session workflows, so every
//! model turn still runs on the session worker.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use ag_forge::{
    ReviewCheckFailure, ReviewCommentSnapshot, ReviewCommentThread, ReviewRequestAttention,
};
use tracing::warn;

use crate::app::prompt_intent::{PromptSessionMode, ReviewCommentResolutionOutcome};
use crate::app::session::StatusTransition;
use crate::app::{App, Tab};
use crate::domain::session::{ReviewRequest, ReviewRequestState, SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, SessionReviewWatchRow};
use crate::presentation::app_mode::{
    AppMode, ConfirmationIntent, ConfirmationViewMode, ReviewCommentSelection,
};

/// Checked-in prompt template submitted when watched checks fail.
const FIX_FAILED_CHECKS_PROMPT_TEMPLATE: &str =
    include_str!("template/fix_failed_checks_prompt.md");
/// Body of the confirmation that offers or enables watching. The overlay
/// shows one truncated line, so the question stays short.
const WATCH_CONFIRMATION_MESSAGE: &str = "Auto-resolve checks, comments, conflicts?";
/// Index of the `No` option, selected when the watch confirmation opens.
const WATCH_CONFIRMATION_DEFAULT_INDEX: usize = 1;

/// Review-watch state owned by the foreground reducer.
#[derive(Debug, Default)]
pub(crate) struct ReviewWatchState {
    /// Watched sessions whose latest forge lookup failed and was reported.
    failed_lookups: HashSet<SessionId>,
    /// Handled keys of the follow-up whose latest start failed and was
    /// reported, per watched session.
    failed_starts: HashMap<SessionId, Vec<String>>,
    /// Follow-up keys already acted on that still describe the latest forge
    /// report, per watched session.
    handled_keys: HashMap<SessionId, Vec<String>>,
    /// Sessions whose newly created review request awaits the watch offer.
    pending_offers: VecDeque<SessionId>,
}

impl ReviewWatchState {
    /// Restores watched sessions and their handled follow-up keys.
    pub(crate) fn from_rows(rows: Vec<SessionReviewWatchRow>) -> Self {
        Self {
            failed_lookups: HashSet::new(),
            failed_starts: HashMap::new(),
            handled_keys: rows
                .into_iter()
                .map(|row| (SessionId::from(row.session_id), row.handled_keys))
                .collect(),
            pending_offers: VecDeque::new(),
        }
    }

    /// Loads persisted watch state, falling back to no watched sessions when
    /// the rows cannot be read.
    pub(crate) async fn load(db: &AppRepositories) -> Self {
        match db.reviews().load_session_review_watches().await {
            Ok(rows) => Self::from_rows(rows),
            Err(error) => {
                warn!(%error, "failed to load review-request watches");

                Self::default()
            }
        }
    }

    /// Returns whether automatic follow-up is enabled for `session_id`.
    pub(crate) fn is_watched(&self, session_id: &str) -> bool {
        self.handled_keys.contains_key(session_id)
    }
}

/// One automatic follow-up selected from a watched review request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReviewWatchAction {
    /// Evaluates trusted reviewer threads in one agent turn.
    AddressComments {
        /// Unresolved threads awaiting a response.
        threads: Vec<ReviewCommentThread>,
    },
    /// Fixes completed failing checks in one agent turn.
    FixChecks {
        /// Checks that failed for `head_commit`.
        failed_checks: Vec<ReviewCheckFailure>,
        /// Commit the checks ran against.
        head_commit: String,
    },
    /// Syncs the session branch to resolve conflicts with its target.
    ResolveConflicts {
        /// Conflicting source-branch head commit.
        head_commit: String,
    },
}

impl ReviewWatchAction {
    /// Selects the highest-priority follow-up not yet attempted.
    ///
    /// Conflicts come first because syncing replaces the head commit and
    /// reruns checks. Checks wait until none is pending so one turn sees every
    /// failure. Each conflict or check result is attempted once per head
    /// commit and each thread once per comment count, so an unsuccessful
    /// attempt is not repeated until the forge state changes.
    pub(crate) fn plan(
        attention: &ReviewRequestAttention,
        handled_keys: &[String],
    ) -> Option<Self> {
        let is_unhandled = |key: &String| !handled_keys.contains(key);
        if attention.has_merge_conflict && is_unhandled(&conflict_key(&attention.head_commit)) {
            return Some(Self::ResolveConflicts {
                head_commit: attention.head_commit.clone(),
            });
        }

        let threads = attention
            .review_threads
            .iter()
            .filter(|thread| is_unhandled(&thread_key(thread)))
            .cloned()
            .collect::<Vec<_>>();
        if !threads.is_empty() {
            return Some(Self::AddressComments { threads });
        }

        (!attention.has_pending_checks
            && !attention.failed_checks.is_empty()
            && is_unhandled(&checks_key(&attention.head_commit)))
        .then(|| Self::FixChecks {
            failed_checks: attention.failed_checks.clone(),
            head_commit: attention.head_commit.clone(),
        })
    }

    /// Returns the keys that mark this follow-up as attempted.
    fn handled_keys(&self) -> Vec<String> {
        match self {
            Self::AddressComments { threads } => threads.iter().map(thread_key).collect(),
            Self::FixChecks { head_commit, .. } => vec![checks_key(head_commit)],
            Self::ResolveConflicts { head_commit } => vec![conflict_key(head_commit)],
        }
    }

    /// Returns the transcript notice announcing this follow-up.
    fn notice(&self, review_request: &ReviewRequest) -> String {
        match self {
            Self::AddressComments { threads } => {
                format!("Addressing {}", counted(threads.len(), "review comment"))
            }
            Self::FixChecks { failed_checks, .. } => format!(
                "Fixing {}: {}",
                counted(failed_checks.len(), "failing check"),
                failed_checks
                    .iter()
                    .map(|check| check.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::ResolveConflicts { .. } => format!(
                "Resolving merge conflicts with `{}`",
                review_request.summary.target_branch
            ),
        }
    }
}

impl App {
    /// Queues the watch offer for a session whose review request was just
    /// created.
    pub(crate) fn offer_review_watch(&mut self, session_id: SessionId) {
        if !self.review_watch.pending_offers.contains(&session_id) {
            self.review_watch.pending_offers.push_back(session_id);
        }
    }

    /// Opens the next pending watch offer while the session list or a session
    /// view is showing, so the popup never interrupts another dialog, editor,
    /// or composer.
    pub(crate) fn present_review_watch_offer(&mut self) {
        let restore_view = match &self.mode {
            AppMode::List if self.tabs.current() == Tab::Sessions => None,
            AppMode::View {
                scroll_offset,
                session_id,
            } => Some(ConfirmationViewMode {
                scroll_offset: *scroll_offset,
                session_id: session_id.clone(),
            }),
            _ => return,
        };
        while let Some(session_id) = self.review_watch.pending_offers.pop_front() {
            if self.open_review_watch_confirmation(&session_id, restore_view.clone()) {
                self.mark_dirty();

                return;
            }
        }
    }

    /// Opens the watch confirmation for one session with an open review
    /// request that is not watched yet. Returns whether the popup opened.
    pub(crate) fn open_review_watch_confirmation(
        &mut self,
        session_id: &SessionId,
        restore_view: Option<ConfirmationViewMode>,
    ) -> bool {
        if self.review_watch.is_watched(session_id) {
            return false;
        }
        let Some(review_request) = self.open_review_request(session_id) else {
            return false;
        };

        let summary = &review_request.summary;
        self.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::WatchReviewRequest,
            confirmation_message: WATCH_CONFIRMATION_MESSAGE.to_string(),
            confirmation_title: format!(
                "Watch {} {}",
                summary.forge_kind.review_request_short_name(),
                summary.display_id
            ),
            restore_view,
            session_id: Some(session_id.clone()),
            selected_confirmation_index: WATCH_CONFIRMATION_DEFAULT_INDEX,
        };

        true
    }

    /// Enables watching for one session with an open review request and
    /// requests an immediate forge refresh.
    pub(crate) async fn enable_review_watch(&mut self, session_id: &SessionId) {
        let Some(review_request) = self.open_review_request(session_id) else {
            return;
        };
        if let Err(error) = self
            .services
            .db()
            .reviews()
            .enable_session_review_watch(session_id)
            .await
        {
            self.append_review_watch_notice(
                session_id,
                format!("Could not start watching: {error}"),
            )
            .await;

            return;
        }

        self.review_watch
            .handled_keys
            .entry(session_id.clone())
            .or_default();
        self.append_review_watch_notice(
            session_id,
            format!(
                "Watching {} for failing checks, review comments, and merge conflicts",
                review_request_label(&review_request)
            ),
        )
        .await;
        self.reconcile_review_watch_statuses().await;
        self.restart_git_status_task();
    }

    /// Disables watching for one session and returns it to `Review`.
    pub(crate) async fn disable_review_watch(&mut self, session_id: &SessionId) {
        if !self.forget_review_watch(session_id).await {
            return;
        }

        self.append_review_watch_notice(session_id, "Stopped watching the review request")
            .await;
        self.reconcile_review_watch_statuses().await;
    }

    /// Drops watch state for one session without a transcript notice, for
    /// review requests that merged or closed. Returns whether it was watched.
    pub(crate) async fn forget_review_watch(&mut self, session_id: &SessionId) -> bool {
        if self.review_watch.handled_keys.remove(session_id).is_none() {
            return false;
        }
        self.review_watch.failed_lookups.remove(session_id);
        self.review_watch.failed_starts.remove(session_id);
        if let Err(error) = self
            .services
            .db()
            .reviews()
            .disable_session_review_watch(session_id)
            .await
        {
            warn!(%session_id, %error, "failed to remove review-request watch");
        }

        true
    }

    /// Moves idle sessions between `Review` and `Watching` to match their
    /// watch state.
    ///
    /// A watched session with an open review request rests in `Watching`.
    /// Every other `Watching` session returns to `Review`. Only these two idle
    /// states are touched, so running turns, syncs, and focused reviews
    /// finish first.
    pub(crate) async fn reconcile_review_watch_statuses(&mut self) {
        let transitions = self
            .sessions
            .sessions()
            .iter()
            .filter_map(|session| {
                let should_watch = self.review_watch.is_watched(&session.id)
                    && session
                        .review_request
                        .as_ref()
                        .is_some_and(|linked| linked.summary.state == ReviewRequestState::Open);
                let next_status = match (session.status, should_watch) {
                    (Status::Review, true) => Status::Watching,
                    (Status::Watching, false) => Status::Review,
                    _ => return None,
                };

                Some((session.id.clone(), session.status, next_status))
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for (session_id, current_status, next_status) in transitions {
            let Ok(handles) = self.sessions.session_handles_or_err(&session_id) else {
                continue;
            };
            if !handles
                .status
                .lock()
                .is_ok_and(|status| *status == current_status)
            {
                continue;
            }

            let status_transition =
                StatusTransition::from_services(&self.services, handles, session_id.clone());
            changed |= status_transition.apply(next_status).await;
            self.sessions.sync_session_from_handle(&session_id);
        }
        if changed {
            self.publish_sync_context();
            self.mark_dirty();
        }
    }

    /// Starts at most one automatic follow-up for forge facts reported about
    /// a session resting in `Watching`.
    ///
    /// A follow-up counts as handled only once it starts. One that cannot
    /// start, for example while a project sync is running, is retried on the
    /// next report, and its notices are written once per run of failed starts.
    pub(crate) async fn apply_review_request_attention(
        &mut self,
        session_id: &SessionId,
        attention: ReviewRequestAttention,
    ) {
        self.review_watch.failed_lookups.remove(session_id);
        let Some(handled_keys) = self.review_watch.handled_keys.get(session_id) else {
            return;
        };
        let mut current_keys = retain_current_keys(handled_keys, &attention);
        let is_watching = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.status == Status::Watching);
        let follow_up = ReviewWatchAction::plan(&attention, &current_keys)
            .filter(|_| is_watching)
            .zip(self.open_review_request(session_id));
        let Some((action, review_request)) = follow_up else {
            self.review_watch.failed_starts.remove(session_id);
            self.store_review_watch_keys(session_id, current_keys).await;

            return;
        };

        let action_keys = action.handled_keys();
        let is_retry = self.review_watch.failed_starts.get(session_id) == Some(&action_keys);
        if !is_retry {
            self.append_review_watch_notice(session_id, action.notice(&review_request))
                .await;
        }
        match self
            .start_review_watch_action(session_id, &review_request, action)
            .await
        {
            Ok(()) => {
                self.review_watch.failed_starts.remove(session_id);
                current_keys.extend(action_keys);
            }
            Err(error) => {
                if !is_retry {
                    self.append_review_watch_notice(
                        session_id,
                        format!("Could not start: {error}; retrying on the next check"),
                    )
                    .await;
                }
                self.review_watch
                    .failed_starts
                    .insert(session_id.clone(), action_keys);
            }
        }
        self.store_review_watch_keys(session_id, current_keys).await;
    }

    /// Reports that the forge facts of a watched session could not be loaded.
    ///
    /// The notice is written once per run of consecutive failures, so a
    /// persistent forge error does not repeat it on every refresh.
    pub(crate) async fn report_review_request_attention_failure(
        &mut self,
        session_id: &SessionId,
        error: &str,
    ) {
        if !self.review_watch.is_watched(session_id)
            || !self.review_watch.failed_lookups.insert(session_id.clone())
        {
            return;
        }

        self.append_review_watch_notice(
            session_id,
            format!("Could not check the review request: {error}"),
        )
        .await;
    }

    /// Starts one planned follow-up through the ordinary session workflows.
    async fn start_review_watch_action(
        &mut self,
        session_id: &SessionId,
        review_request: &ReviewRequest,
        action: ReviewWatchAction,
    ) -> Result<(), String> {
        match action {
            ReviewWatchAction::AddressComments { threads } => {
                let selections = threads
                    .iter()
                    .map(|thread| ReviewCommentSelection {
                        thread_id: thread.id.clone(),
                    })
                    .collect::<Vec<_>>();
                let snapshot = ReviewCommentSnapshot {
                    pr_level_comments: Vec::new(),
                    threads,
                };
                match self
                    .resolve_session_review_comments(session_id, &snapshot, &selections)
                    .await
                {
                    ReviewCommentResolutionOutcome::ShowSession { .. } => Ok(()),
                    ReviewCommentResolutionOutcome::KeepReviewComments => {
                        Err("the session did not accept the review comments".to_string())
                    }
                }
            }
            ReviewWatchAction::FixChecks {
                failed_checks,
                head_commit,
            } => {
                let prompt =
                    build_fix_failed_checks_prompt(review_request, &head_commit, &failed_checks);

                self.submit_turn_prompt(session_id.clone(), PromptSessionMode::Existing, prompt)
                    .await
                    .map_err(|error| error.to_string())
            }
            ReviewWatchAction::ResolveConflicts { .. } => {
                // Session sync rebases onto the branch the session would
                // publish against. A review request retargeted on the forge
                // conflicts with another branch, so syncing cannot resolve it.
                let target_branch = &review_request.summary.target_branch;
                if let Some(sync_branch) = self
                    .sessions
                    .session_for_id(session_id)
                    .map(|session| self.review_target_branch_for_session(session))
                    .filter(|sync_branch| sync_branch != target_branch)
                {
                    return Err(format!(
                        "the review request targets `{target_branch}`, but this session syncs \
                         with `{sync_branch}`"
                    ));
                }

                self.rebase_session(session_id)
                    .await
                    .map_err(|error| error.to_string())
            }
        }
    }

    /// Replaces the remembered follow-up keys of one watched session in
    /// memory, and in durable storage when they changed.
    async fn store_review_watch_keys(&mut self, session_id: &SessionId, keys: Vec<String>) {
        let previous_keys = self
            .review_watch
            .handled_keys
            .insert(session_id.clone(), keys.clone());
        if previous_keys.as_ref() == Some(&keys) {
            return;
        }

        if let Err(error) = self
            .services
            .db()
            .reviews()
            .update_session_review_watch_handled_keys(session_id, keys)
            .await
        {
            warn!(%session_id, %error, "failed to persist review-request watch keys");
        }
    }

    /// Returns the open linked review request of one loaded session.
    fn open_review_request(&self, session_id: &str) -> Option<ReviewRequest> {
        self.sessions
            .session_for_id(session_id)?
            .review_request
            .clone()
            .filter(|review_request| review_request.summary.state == ReviewRequestState::Open)
    }

    /// Appends one durable `[Review Watch]` transcript notice.
    async fn append_review_watch_notice(&self, session_id: &str, detail: impl std::fmt::Display) {
        self.append_output_for_session(session_id, &TranscriptNotice::ReviewWatch.format(detail))
            .await;
    }
}

/// Builds the agent-facing prompt for one batch of failed checks.
///
/// Forge-reported values stay inside one collision-safe fence so check names
/// and URLs cannot read as instructions.
pub(crate) fn build_fix_failed_checks_prompt(
    review_request: &ReviewRequest,
    head_commit: &str,
    failed_checks: &[ReviewCheckFailure],
) -> TurnPrompt {
    let summary = &review_request.summary;
    let mut check_data = format!(
        "Review request: {}\nCommit: {head_commit}\n",
        summary.web_url
    );
    for check in failed_checks {
        let _ = write!(
            check_data,
            "\nCheck: {}\nOutcome: {}\n",
            check.name, check.outcome
        );
        if let Some(web_url) = check.web_url.as_deref() {
            let _ = writeln!(check_data, "Details: {web_url}");
        }
    }
    let check_data = check_data.trim_end();
    let fence = ag_protocol::diff_fence(check_data);
    let prompt = FIX_FAILED_CHECKS_PROMPT_TEMPLATE
        .trim_end()
        .replace(
            "{{ review_request_name }}",
            summary.forge_kind.review_request_name(),
        )
        .replace("{{ forge_cli }}", summary.forge_kind.cli_name())
        .replace(
            "{{ fenced_failed_checks }}",
            &format!("{fence}text\n{check_data}\n{fence}"),
        );

    TurnPrompt::from_agent_data(prompt)
}

/// Returns the user-facing name of one linked review request, such as
/// `pull request #42`.
fn review_request_label(review_request: &ReviewRequest) -> String {
    format!(
        "{} {}",
        review_request.summary.forge_kind.review_request_name(),
        review_request.summary.display_id
    )
}

/// Formats a count with a singular or plural noun.
fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Returns the handled keys that still describe `attention`: attempts at its
/// head commit and at reported threads in their current conversation state.
///
/// Keys for replaced commits and for threads that moved on or left the report
/// are dropped, so the remembered keys stay bounded by the forge report and a
/// key that still applies is never evicted.
fn retain_current_keys(handled_keys: &[String], attention: &ReviewRequestAttention) -> Vec<String> {
    let current_keys = [
        conflict_key(&attention.head_commit),
        checks_key(&attention.head_commit),
    ]
    .into_iter()
    .chain(attention.review_threads.iter().map(thread_key))
    .collect::<HashSet<_>>();

    handled_keys
        .iter()
        .filter(|key| current_keys.contains(*key))
        .cloned()
        .collect()
}

/// Key for one conflict-resolution attempt at `head_commit`.
fn conflict_key(head_commit: &str) -> String {
    format!("conflict:{head_commit}")
}

/// Key for one failed-check attempt at `head_commit`.
fn checks_key(head_commit: &str) -> String {
    format!("checks:{head_commit}")
}

/// Key for one attempt at a thread in its current conversation state.
fn thread_key(thread: &ReviewCommentThread) -> String {
    format!("thread:{}:{}", thread.id, thread.comments.len())
}

#[cfg(test)]
#[path = "review_watch_test.rs"]
mod tests;

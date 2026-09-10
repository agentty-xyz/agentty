use std::io;

use ag_orchestration::OrchestrationApprovalOutcome;
use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use tracing::warn;

use crate::app::session::{SessionTaskService, remote_branch_name_from_upstream_ref};
use crate::app::{self, App, AppEvent, ReviewCacheEntry};
use crate::domain::input::InputState;
use crate::domain::session::{FollowUpTaskAction, PublishBranchAction, SessionId, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::TransientMessageSlot;
use crate::presentation::app_mode::{
    AppMode, ChatFocus, ConfirmationIntent, ConfirmationViewMode, DiffSidebarFocus, HelpContext,
};
use crate::presentation::help_action::{self, ViewSessionState};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState};
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::confirmation::DEFAULT_OPTION_INDEX;
use crate::runtime::mode::input_key::is_insertable_char_key;
use crate::runtime::mode::prompt;
use crate::ui::RenderCacheStore;

#[derive(Clone)]
struct ViewContext {
    scroll_offset: Option<u16>,
    session_id: SessionId,
    session_index: usize,
}

/// Pending review and scroll updates produced by one key event in session-view
/// mode.
struct ViewPendingUpdate {
    scroll_offset: Option<u16>,
}

impl ViewPendingUpdate {
    /// Builds update state seeded from the current view scroll.
    fn from_context(view_context: &ViewContext) -> Self {
        Self {
            scroll_offset: view_context.scroll_offset,
        }
    }
}

/// Borrowed per-key context used while processing one session-view key event.
struct ViewKeyContext<'a> {
    context: &'a ViewContext,
    session_snapshot: &'a ViewSessionSnapshot,
}

/// Two-state action availability used in session-view snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewActionState {
    Disabled,
    Enabled,
}

impl ViewActionState {
    /// Returns the action state that corresponds to `is_enabled`.
    fn from_bool(is_enabled: bool) -> Self {
        if is_enabled {
            return Self::Enabled;
        }

        Self::Disabled
    }

    /// Returns whether the action is currently enabled.
    fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

/// Snapshot of session-derived state used by view-mode key handling.
struct ViewSessionSnapshot {
    branch_actions: ViewActionState,
    continue_terminal_session: ViewActionState,
    follow_up_task_action: Option<FollowUpTaskAction>,
    fork_session: ViewActionState,
    inspect_diff: ViewActionState,
    is_managed: bool,
    is_orchestrator: bool,
    merge_session_branch: ViewActionState,
    mutate_session_branch: ViewActionState,
    open_worktree: ViewActionState,
    publish_pull_request_action: Option<PublishBranchAction>,
    rebase_session_branch: ViewActionState,
    reply_to_session: ViewActionState,
    review_comments: ViewActionState,
    session_state: ViewSessionState,
    session_status: Status,
    start_staged_session: ViewActionState,
}

impl ViewSessionSnapshot {
    /// Returns whether the active session can enter the merge queue from view
    /// mode.
    fn can_merge_session(&self) -> bool {
        self.branch_actions.is_enabled()
            && self.session_status.allows_session_actions()
            && self.can_merge_session_branch()
            && self.session_state != ViewSessionState::StackedDraft
    }

    /// Returns whether the active session can start the session sync action
    /// from view mode.
    fn can_rebase_session(&self) -> bool {
        self.branch_actions.is_enabled()
            && self.session_status.allows_rebase_action()
            && self.can_rebase_session_branch()
            && self.session_state != ViewSessionState::StackedDraft
    }

    /// Returns whether a terminal session can launch a continuation draft.
    fn can_continue_terminal_session(&self) -> bool {
        self.continue_terminal_session.is_enabled()
    }

    /// Returns whether this session can be forked from view mode.
    fn can_fork_session(&self) -> bool {
        self.fork_session.is_enabled()
    }

    /// Returns whether this session can start branch-mutating stack work.
    fn can_mutate_session_branch(&self) -> bool {
        self.mutate_session_branch.is_enabled()
    }

    /// Returns whether managed-worker-only keys may be handled.
    fn accepts_managed_keys(&self) -> bool {
        self.is_managed && self.session_state != ViewSessionState::ManagedResearch
    }

    /// Returns whether this session can enter the merge queue under stack
    /// rules.
    fn can_merge_session_branch(&self) -> bool {
        self.merge_session_branch.is_enabled()
    }

    /// Returns whether this session's local worktree can be opened.
    fn can_open_worktree(&self) -> bool {
        self.open_worktree.is_enabled()
    }

    /// Returns whether this session can start sync work under stack rules.
    fn can_rebase_session_branch(&self) -> bool {
        self.rebase_session_branch.is_enabled()
    }

    /// Returns whether this session can accept a reply under stack rules.
    fn can_reply_to_session(&self) -> bool {
        self.reply_to_session.is_enabled()
    }

    /// Returns whether the session has a linked forge review request whose
    /// comments can be opened read-only.
    fn can_open_review_comments(&self) -> bool {
        self.review_comments.is_enabled()
    }

    /// Returns whether this staged draft can start its first live turn.
    fn can_start_staged_session(&self) -> bool {
        self.start_staged_session.is_enabled()
    }

    /// Returns whether `Enter` may open a prompt composer from view mode.
    fn can_open_prompt_composer(&self) -> bool {
        if !self.session_status.allows_chat_composer() {
            return false;
        }

        self.can_edit_without_branch_work() || self.can_reply_to_session()
    }

    /// Returns whether `/` may open the slash-command composer from view mode.
    fn can_launch_configuration_composer(&self) -> bool {
        if !self.session_status.allows_session_actions() {
            return false;
        }

        self.can_edit_without_branch_work()
            || self.can_mutate_session_branch()
            || self.can_reply_to_session()
    }

    /// Returns whether image paste can open a draft prompt composer directly
    /// from view mode.
    fn can_paste_image_into_draft_composer(&self) -> bool {
        self.can_open_prompt_composer() && self.can_edit_without_branch_work()
    }

    /// Returns whether editing the viewed session only stages local draft
    /// content and therefore does not mutate a session branch.
    fn can_edit_without_branch_work(&self) -> bool {
        matches!(
            self.session_state,
            ViewSessionState::NewSession | ViewSessionState::StackedDraft
        )
    }
}

/// Processes view-mode key presses and keeps shortcut availability aligned with
/// session status (`o` disabled outside editable/review-ready local
/// worktrees, and diff/review available for review-ready statuses).
pub(crate) async fn handle_with_cache<B: Backend>(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(view_context) = view_context(app) else {
        return Ok(EventResult::Continue);
    };
    let mut pending_update = ViewPendingUpdate::from_context(&view_context);
    if chat_scroll::is_scroll_key(key) {
        let metrics = view_metrics(app, render_cache_store, terminal, &view_context)?;
        chat_scroll::apply_scroll_key(&mut pending_update.scroll_offset, metrics, key);
        apply_view_scroll_and_output_mode(app, pending_update.scroll_offset);

        return Ok(EventResult::Continue);
    }

    let Some(view_session_snapshot) = view_session_snapshot(app, &view_context) else {
        return Ok(EventResult::Continue);
    };
    let view_key_context = ViewKeyContext {
        context: &view_context,
        session_snapshot: &view_session_snapshot,
    };

    if !handle_view_key(app, key, view_key_context, &mut pending_update).await {
        return Ok(EventResult::Continue);
    }

    apply_view_scroll_and_output_mode(app, pending_update.scroll_offset);

    Ok(EventResult::Continue)
}

/// Applies one view-mode key press and updates pending output/scroll state.
///
/// Returns `false` when key handling already transitioned mode and should skip
/// applying pending view updates.
async fn handle_view_key(
    app: &mut App,
    key: KeyEvent,
    view_key_context: ViewKeyContext<'_>,
    pending_update: &mut ViewPendingUpdate,
) -> bool {
    let view_context = view_key_context.context;
    let view_session_snapshot = view_key_context.session_snapshot;

    if let Some(should_apply_pending_update) = handle_primary_view_key(
        app,
        key,
        view_context,
        view_session_snapshot,
        pending_update,
    )
    .await
    {
        return should_apply_pending_update;
    }

    if let Some(should_apply_pending_update) = handle_workflow_view_key(
        app,
        key,
        view_context,
        view_session_snapshot,
        pending_update,
    )
    .await
    {
        return should_apply_pending_update;
    }

    true
}

/// Handles primary session-view actions that do not need diff/review routing.
async fn handle_primary_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    pending_update: &ViewPendingUpdate,
) -> Option<bool> {
    if view_session_snapshot.is_orchestrator
        && handle_orchestration_view_key(app, key, view_context).await
    {
        return Some(true);
    }
    let accepts_managed_keys = view_session_snapshot.accepts_managed_keys();
    if accepts_managed_keys && handle_managed_view_key(app, key, view_context) {
        return Some(false);
    }

    match key.code {
        KeyCode::Char('q') => {
            app.mode = AppMode::List;
        }
        KeyCode::Char('o') if view_session_snapshot.can_open_worktree() => {
            return Some(handle_open_worktree_key(app, view_context, view_session_snapshot).await);
        }
        KeyCode::Char('l') if view_session_snapshot.follow_up_task_action.is_some() => {
            if let Err(error) = app
                .launch_or_open_selected_follow_up_task(&view_context.session_id)
                .await
            {
                app.append_output_for_session(
                    &view_context.session_id,
                    &TranscriptNotice::FollowUpTaskError.format(error),
                )
                .await;
            }

            return Some(false);
        }
        KeyCode::Char('s') if view_session_snapshot.can_start_staged_session() => {
            if let Err(error) = app.start_staged_session(&view_context.session_id).await {
                app.append_output_for_session(
                    &view_context.session_id,
                    &TranscriptNotice::StartError.format(error),
                )
                .await;
            }

            return Some(false);
        }
        KeyCode::Char('v' | 'V')
            if prompt::is_prompt_image_paste_key(key)
                && view_session_snapshot.can_paste_image_into_draft_composer() =>
        {
            open_draft_prompt_with_pasted_image(app, view_context, pending_update.scroll_offset)
                .await;

            return Some(false);
        }
        KeyCode::Char('c')
            if key.modifiers == event::KeyModifiers::NONE
                && view_session_snapshot.can_open_review_comments() =>
        {
            open_review_comments_in_diff(app, view_context);

            return Some(false);
        }
        KeyCode::Char('c')
            if key.modifiers == event::KeyModifiers::NONE
                && view_session_snapshot.can_continue_terminal_session() =>
        {
            open_continue_confirmation(app, view_context);

            return Some(false);
        }
        KeyCode::Char('[') if app.has_multiple_follow_up_tasks(&view_context.session_id) => {
            app.select_previous_follow_up_task(&view_context.session_id);
        }
        KeyCode::Char(']') if app.has_multiple_follow_up_tasks(&view_context.session_id) => {
            app.select_next_follow_up_task(&view_context.session_id);
        }
        KeyCode::Enter if view_session_snapshot.can_open_prompt_composer() => {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(session_prompt_history_entries(
                    app.sessions.session_at(view_context.session_index)?,
                )),
                InputState::default(),
                pending_update.scroll_offset,
            )
            .await;
        }
        KeyCode::Char('/')
            if view_session_snapshot.can_launch_configuration_composer()
                && is_insertable_char_key(key) =>
        {
            switch_view_to_prompt(
                app,
                view_context,
                PromptHistoryState::new(session_prompt_history_entries(
                    app.sessions.session_at(view_context.session_index)?,
                )),
                InputState::with_text("/".to_string()),
                pending_update.scroll_offset,
            )
            .await;
        }
        _ => return None,
    }

    Some(true)
}

fn open_review_comments_in_diff(app: &mut App, view_context: &ViewContext) {
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return;
    }

    app.start_diff_view_load(
        &view_context.session_id,
        None,
        DiffSidebarFocus::Comments,
        true,
    );
}

/// Opens a regular worktree immediately or warns for a managed worker.
async fn handle_open_worktree_key(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
) -> bool {
    let restore_view = confirmation_view_mode(view_context);
    if view_session_snapshot.is_managed {
        open_managed_worktree_confirmation(app, restore_view);

        return false;
    }
    open_worktree_for_view_session(app, restore_view).await;

    true
}

/// Applies campaign-board controls owned by an orchestrator session.
async fn handle_orchestration_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
) -> bool {
    match key.code {
        KeyCode::Char('a') => {
            let outcome = app
                .approve_orchestration(&view_context.session_id, None)
                .await;
            if outcome == OrchestrationApprovalOutcome::IntegrationApproachRequired {
                app.mode = AppMode::Confirmation {
                    confirmation_intent: ConfirmationIntent::ChooseIntegrationApproach,
                    confirmation_message: "Choose how to integrate verified task branches."
                        .to_string(),
                    confirmation_title: "Integration Approach".to_string(),
                    restore_view: Some(confirmation_view_mode(view_context)),
                    session_id: Some(view_context.session_id.clone()),
                    selected_confirmation_index: 0,
                };
            }
        }
        _ => return false,
    }

    true
}

/// Opens the one-way ownership-transfer confirmation for a managed worker.
fn handle_managed_view_key(app: &mut App, key: KeyEvent, view_context: &ViewContext) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL) {
        return true;
    }
    if key.code != KeyCode::Char('D') {
        return false;
    }
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::DetachManagedSession,
        confirmation_message: "Detach this worker from its campaign and take permanent ownership?"
            .to_string(),
        confirmation_title: "Confirm Detach".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };

    true
}

/// Handles workflow actions in session view such as diff, publish, review,
/// merge, session sync, cancellation, and help.
async fn handle_workflow_view_key(
    app: &mut App,
    key: KeyEvent,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
    pending_update: &mut ViewPendingUpdate,
) -> Option<bool> {
    match key.code {
        KeyCode::Char('d')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.inspect_diff.is_enabled() =>
        {
            show_diff_for_view_session(app, view_context);
        }
        KeyCode::Char(character)
            if character.eq_ignore_ascii_case(&'p')
                && !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.publish_pull_request_action.is_some() =>
        {
            let Some(publish_pull_request_action) =
                view_session_snapshot.publish_pull_request_action
            else {
                return Some(true);
            };
            open_publish_branch_input(app, view_context, publish_pull_request_action);

            return Some(false);
        }
        KeyCode::Char('F')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.can_fork_session() =>
        {
            open_fork_confirmation(app, view_context);

            return Some(false);
        }
        KeyCode::Char('f')
            if !key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.branch_actions.is_enabled()
                && view_session_snapshot.session_status.allows_review_actions() =>
        {
            open_or_regenerate_review(app, view_context, pending_update);
        }
        KeyCode::Char('m') if view_session_snapshot.can_merge_session() => {
            open_merge_confirmation(app, view_context);
        }
        KeyCode::Char('r') if view_session_snapshot.can_rebase_session() => {
            rebase_view_session(app, &view_context.session_id).await;
        }
        KeyCode::Char('c')
            if key.modifiers.contains(event::KeyModifiers::CONTROL)
                && view_session_snapshot.session_status == Status::InProgress =>
        {
            end_in_progress_turn(app, &view_context.session_id).await;

            return Some(false);
        }
        KeyCode::Char('?') => {
            open_view_help_overlay(app, view_context, view_session_snapshot);
            return Some(false);
        }
        _ => return None,
    }

    Some(true)
}

/// Opens a fork confirmation overlay for the active view session.
///
/// The body explains that the new session keeps the current transcript history
/// while starting on a fresh session branch.
fn open_fork_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ForkSession,
        confirmation_message: "Fork this session into a new session with the current transcript \
                               history?"
            .to_string(),
        confirmation_title: "Confirm Fork".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens a merge confirmation overlay for the active view session.
///
/// The body text asks whether the current session should be added to the
/// merge queue.
fn open_merge_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::MergeSession,
        confirmation_message: "Add this session to merge queue?".to_string(),
        confirmation_title: "Confirm Merge".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens a continuation confirmation overlay for one terminal session.
///
/// The confirmation explains that Agentty will create a new draft session
/// seeded with initial context so the user can add more notes before starting
/// it.
fn open_continue_confirmation(app: &mut App, view_context: &ViewContext) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::ContinueSession,
        confirmation_message: "Create a new draft session with initial context from this session?"
            .to_string(),
        confirmation_title: "Confirm Continue".to_string(),
        restore_view: Some(confirmation_view_mode(view_context)),
        session_id: Some(view_context.session_id.clone()),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Warns before opening a controller-managed worker's writable worktree.
fn open_managed_worktree_confirmation(app: &mut App, restore_view: ConfirmationViewMode) {
    app.mode = AppMode::Confirmation {
        confirmation_intent: ConfirmationIntent::OpenManagedWorktree,
        confirmation_message: "This opens a writable shell in a controller-managed worktree. \
                               Edits can invalidate orchestration verification. Open anyway?"
            .to_string(),
        confirmation_title: "Open Managed Worktree".to_string(),
        session_id: Some(restore_view.session_id.clone()),
        restore_view: Some(restore_view),
        selected_confirmation_index: DEFAULT_OPTION_INDEX,
    };
}

/// Opens the viewed session worktree directly or shows a command selector when
/// multiple launch configurations are configured.
pub(crate) async fn open_worktree_for_view_session(
    app: &mut App,
    restore_view: ConfirmationViewMode,
) {
    let launch_configurations = app.configured_launch_configurations();
    if launch_configurations.len() > 1 {
        app.mode = AppMode::LaunchConfigurationSelector {
            commands: launch_configurations,
            restore_view,
            selected_command_index: 0,
        };

        return;
    }

    let selected_launch_configuration = launch_configurations.first().map(String::as_str);
    app.mode = restore_view.into_view_mode();

    app.open_session_worktree_in_tmux_with_command(selected_launch_configuration)
        .await;
}

/// Builds the view-mode snapshot used to restore chat context when a merge
/// confirmation is dismissed.
fn confirmation_view_mode(view_context: &ViewContext) -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: view_context.scroll_offset,
        session_id: view_context.session_id.clone(),
    }
}

/// Opens focused review or shows a regeneration confirmation popup.
///
/// When a review result (or error) is already present, shows a confirmation
/// popup before regenerating. If a generation is already in flight (loading),
/// the press is ignored to avoid spawning duplicate background tasks.
/// Otherwise, loads or starts focused review output and resets scroll to
/// bottom-aligned mode.
fn open_or_regenerate_review(
    app: &mut App,
    view_context: &ViewContext,
    pending_update: &mut ViewPendingUpdate,
) {
    let (review_status_message, review_text) = app.review_view_state(&view_context.session_id);
    if app.review_is_loading(&view_context.session_id) {
        return;
    }

    if review_text.is_some() || review_status_message.is_some() {
        app.mode = AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::RegenerateReview,
            confirmation_message: "Regenerate focused review?".to_string(),
            confirmation_title: "Confirm Regenerate".to_string(),
            restore_view: Some(confirmation_view_mode(view_context)),
            session_id: Some(view_context.session_id.clone()),
            selected_confirmation_index: DEFAULT_OPTION_INDEX,
        };

        return;
    }

    open_review_output_mode(app, view_context);

    pending_update.scroll_offset = None;
}

/// Collects session-specific values used by `handle()` from the active view
/// row.
fn view_session_snapshot(app: &App, view_context: &ViewContext) -> Option<ViewSessionSnapshot> {
    let session = app.sessions.session_at(view_context.session_index)?;
    let session_status = session.status;
    let can_open_worktree = app.is_tmux_session()
        && session.allows_worktree_open_action()
        && *app
            .sessions
            .session_worktree_availability()
            .get(view_context.session_id.as_str())
            .unwrap_or(&false);

    Some(ViewSessionSnapshot {
        branch_actions: ViewActionState::from_bool(
            session.owns_branch_changes() && session.accepts_user_turns(),
        ),
        continue_terminal_session: ViewActionState::from_bool(
            session.allows_terminal_continuation(),
        ),
        fork_session: ViewActionState::from_bool(session.allows_fork_action()),
        follow_up_task_action: app.selected_follow_up_task_action(&view_context.session_id),
        inspect_diff: ViewActionState::from_bool(
            session.stats.should_show_diff()
                && (session.is_managed()
                    || (session.owns_branch_changes() && session.status.allows_diff_view())),
        ),
        is_managed: session.is_managed(),
        is_orchestrator: session.role == crate::domain::session::SessionRole::Orchestrator,
        merge_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_merge_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        mutate_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_mutate_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        open_worktree: ViewActionState::from_bool(can_open_worktree),
        publish_pull_request_action: session.publish_pull_request_action(),
        rebase_session_branch: ViewActionState::from_bool(
            session.owns_branch_changes()
                && app
                    .sessions
                    .can_rebase_session_branch_in_stack(view_context.session_id.as_str()),
        ),
        reply_to_session: ViewActionState::from_bool(
            session.accepts_user_turns()
                && app
                    .sessions
                    .can_reply_to_session_in_stack(view_context.session_id.as_str()),
        ),
        review_comments: ViewActionState::from_bool(
            session.has_review_request() && session.allows_review_comment_reply(),
        ),
        session_state: help_action::session_view_state(session),
        session_status,
        start_staged_session: ViewActionState::from_bool(
            app.sessions
                .can_start_staged_session(view_context.session_id.as_str())
                || (session.accepts_user_turns()
                    && session
                        .transient_messages
                        .get(TransientMessageSlot::WorkspacePreparation)
                        .is_some()),
        ),
    })
}

/// Applies in-place updates for active view review status/text and scroll
/// position.
fn apply_view_scroll_and_output_mode(app: &mut App, scroll_offset: Option<u16>) {
    if let AppMode::View {
        scroll_offset: view_scroll_offset,
        ..
    } = &mut app.mode
    {
        *view_scroll_offset = scroll_offset;
    }
}

/// Handles `Ctrl+C` while a session is `InProgress` with a per-press policy.
///
/// Each press first tries to retract the most recently queued chat message on
/// [`SessionHandles::queued_messages`] (LIFO `pop_back`) so the user can undo
/// queue entries one-by-one in the reverse order they were added, without
/// interrupting the running turn. The running turn keeps streaming, status
/// stays `InProgress`, and no cancellation token, database status update, or
/// auto-review suppression runs while a queued message is being dropped.
/// When the queue is already empty, the press falls through to
/// [`cancel_in_progress_turn`] which performs the existing
/// cancel-and-return-to-`Review` flow.
async fn end_in_progress_turn(app: &mut App, session_id: &str) {
    if pop_last_queued_chat_message_if_any(app, session_id).await {
        return;
    }

    cancel_in_progress_turn(app, session_id).await;
}

/// Pops the most recently queued chat message (LIFO) from the session's
/// handles and re-syncs the snapshot from the post-pop handle state,
/// returning `true` when one queued message was retracted.
///
/// Pops the entry from the shared [`SessionHandles::queued_messages`] deque
/// via `pop_back`. The handle is the source of truth: the worker may have
/// already drained the oldest entry via `pop_front` between snapshot
/// refreshes, so a position-based snapshot pop could remove the wrong
/// transcript row and leave a phantom queued message visible. The snapshot
/// is then re-projected from the handle through
/// [`SessionState::sync_session_from_handle`], so no additional manual
/// `queued_messages` mutation is needed here. Releases any local image
/// attachments owned by the popped prompt through
/// [`App::cleanup_prompt_attachment_files`] so retracted messages do not
/// leak temp files under `AGENTTY_ROOT/tmp/`, then emits
/// [`AppEvent::SessionUpdated`] so list and chat views redraw without paying
/// for a full DB-backed `RefreshSessions` reload. Leaves the cancellation
/// token, persisted status, and auto-review suppression untouched so the
/// running turn can keep streaming.
async fn pop_last_queued_chat_message_if_any(app: &mut App, session_id: &str) -> bool {
    let popped_message = app
        .sessions
        .session_handles()
        .get(session_id)
        .and_then(|handles| handles.queued_messages.lock().ok()?.pop_back());

    let Some(popped_message) = popped_message else {
        return false;
    };

    app.sessions.sync_session_from_handle(session_id);

    app.cleanup_prompt_attachment_files(popped_message.prompt())
        .await;

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id,
        ),
    });

    true
}

/// Interrupts the active turn of a running `InProgress` session and returns it
/// to `Review`.
///
/// Cancels queued operations in the database, then fires the per-turn
/// [`CancellationToken`] so the worker's `select!` branch triggers
/// graceful channel shutdown. The worker owns process termination:
/// CLI channels receive `SIGTERM` inside the cancellation branch
/// (where the child is guaranteed alive because `run_turn` has not
/// returned yet), and app-server channels shut down through
/// `shutdown_session`. Both paths converge on the worker returning a
/// `[Stopped]` error. After signalling, the persisted status is updated to
/// `Review`, the in-memory snapshot and shared handle are refreshed, and UI
/// events are emitted so the user can inspect or continue the session instead
/// of treating it as canceled.
async fn cancel_in_progress_turn(app: &mut App, session_id: &str) {
    let timestamp_seconds =
        app::session::unix_timestamp_from_system_time(app.services.clock().now_system_time());

    if let Err(error) = app
        .services
        .db()
        .operations()
        .request_cancel_for_session_operations(session_id)
        .await
    {
        warn!(
            session_id = session_id,
            error = %error,
            "failed to request cancellation for queued session operations"
        );
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id) {
        // Cancel the current turn's token so the worker's `select!`
        // branch fires and triggers graceful channel shutdown. The
        // worker sends SIGTERM to CLI child processes inside the
        // cancellation path where the PID is guaranteed valid.
        match handles.cancel_token.lock() {
            Ok(cancel_token) => cancel_token.cancel(),
            Err(error) => {
                warn!(
                    session_id = session_id,
                    error = %error,
                    "failed to lock session cancel token"
                );
            }
        }
    }

    if let Err(error) = app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(
            session_id,
            &Status::Review.to_string(),
            timestamp_seconds,
        )
        .await
    {
        warn!(
            session_id = session_id,
            error = %error,
            "failed to persist review status after interrupting session turn"
        );

        return;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        let previous_status = session.status;
        session.status = Status::Review;
        session.reconcile_status_transition(previous_status);
    }

    suppress_auto_review_for_stopped_turn(app, session_id);

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.into(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id,
        ),
    });
    app.services.emit_session_and_project_refresh_events();
}

/// Marks automatic focused review as suppressed after a user stops one active
/// turn.
///
/// The session remains review-ready, but the reducer's automatic focused
/// review pass should not immediately start an agent review for the partially
/// stopped turn. The marker is intentionally inserted without loading a diff so
/// `Ctrl+C` returns to the event loop quickly; the next submitted turn clears
/// the cache, and pressing `f` still starts manual focused review because
/// view-mode review handling replaces suppressed entries.
fn suppress_auto_review_for_stopped_turn(app: &mut App, session_id: &str) {
    app.suppress_review_output(session_id);
}

/// Switches the TUI mode from session view to the prompt input.
///
/// Focused-review output/status is copied into prompt mode so canceling the
/// composer returns to the same session transcript state. The caller supplies
/// the initial composer buffer so session-view shortcuts like `/` can open the
/// prompt with prefilled slash-command text. A non-empty initial buffer
/// intentionally replaces any saved composer for the session.
async fn switch_view_to_prompt(
    app: &mut App,
    view_context: &ViewContext,
    history_state: PromptHistoryState,
    input: InputState,
    scroll_offset: Option<u16>,
) {
    if input.is_empty() && app.restore_prompt_progress(&view_context.session_id).await {
        return;
    }

    app.discard_prompt_progress(&view_context.session_id).await;

    app.mode = AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state,
        slash_state: app.prompt_slash_state(),
        session_id: view_context.session_id.clone(),
        input,
        scroll_offset,
    };
}

/// Opens a draft composer from view mode and immediately applies the existing
/// prompt image-paste intent.
async fn open_draft_prompt_with_pasted_image(
    app: &mut App,
    view_context: &ViewContext,
    scroll_offset: Option<u16>,
) {
    let Some(session) = app.sessions.session_at(view_context.session_index) else {
        return;
    };
    let history_state = PromptHistoryState::new(session_prompt_history_entries(session));

    switch_view_to_prompt(
        app,
        view_context,
        history_state,
        InputState::default(),
        scroll_offset,
    )
    .await;

    prompt::paste_image_into_active_prompt(app, &view_context.session_id).await;
}

/// Opens the help overlay while preserving the currently viewed session state.
fn open_view_help_overlay(
    app: &mut App,
    view_context: &ViewContext,
    view_session_snapshot: &ViewSessionSnapshot,
) {
    app.mode = AppMode::Help {
        context: HelpContext::View {
            can_fork_session: view_session_snapshot.can_fork_session(),
            can_merge_session_branch: view_session_snapshot.can_merge_session_branch(),
            can_mutate_session_branch: view_session_snapshot.can_mutate_session_branch(),
            can_open_worktree: view_session_snapshot.can_open_worktree(),
            can_rebase_session_branch: view_session_snapshot.can_rebase_session_branch(),
            can_reply_to_session: view_session_snapshot.can_reply_to_session(),
            can_show_diff: view_session_snapshot.inspect_diff.is_enabled(),
            can_start_staged_session: view_session_snapshot.can_start_staged_session(),
            can_view_review_comments: view_session_snapshot.can_open_review_comments(),
            publish_pull_request_action: view_session_snapshot.publish_pull_request_action,
            session_id: view_context.session_id.clone(),
            session_state: view_session_snapshot.session_state,
            scroll_offset: view_context.scroll_offset,
        },
        scroll_offset: 0,
    };
}

/// Opens the session-view publish popup and preserves the current view state
/// for cancel or submit.
fn open_publish_branch_input(
    app: &mut App,
    view_context: &ViewContext,
    publish_branch_action: PublishBranchAction,
) {
    let Some(session) = app.sessions.session_at(view_context.session_index) else {
        return;
    };
    let default_branch_name = crate::app::session::session_branch(&session.id);
    let locked_upstream_ref = session.published_upstream_ref.clone();
    let input = locked_upstream_ref
        .as_deref()
        .map(remote_branch_name_from_upstream_ref)
        .map(InputState::with_text)
        .unwrap_or_default();

    app.mode = AppMode::PublishBranchInput {
        default_branch_name,
        input,
        locked_upstream_ref,
        publish_branch_action,
        restore_view: confirmation_view_mode(view_context),
    };
}

fn view_context(app: &mut App) -> Option<ViewContext> {
    let (session_id, scroll_offset) = match &app.mode {
        AppMode::View {
            session_id,
            scroll_offset,
        } => (session_id.clone(), *scroll_offset),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    Some(ViewContext {
        scroll_offset,
        session_id,
        session_index,
    })
}

fn view_metrics<B: Backend>(
    app: &App,
    render_cache_store: &RenderCacheStore,
    terminal: &Terminal<B>,
    view_context: &ViewContext,
) -> io::Result<ChatScrollMetrics>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let terminal_size = terminal.size().map_err(crate::runtime::backend_err)?;
    let terminal_rect = Rect::new(0, 0, terminal_size.width, terminal_size.height);

    Ok(ChatScrollMetrics::new(
        app,
        render_cache_store,
        &view_context.session_id,
        view_context.session_index,
        terminal_rect,
    ))
}

/// Returns prompt-history entries for the session-view prompt composer.
///
/// Draft sessions use the staged prompt stored in `prompt` directly because
/// they have not yet written user prompts into the persisted transcript.
/// Started sessions read typed user rows so generated agent prompts remain
/// available for provider replay without entering user-facing history.
pub(super) fn session_prompt_history_entries(
    session: &crate::domain::session::Session,
) -> Vec<String> {
    if session.status == Status::Draft && session.is_draft_session() {
        return vec![session.prompt.clone()];
    }

    session
        .transcript
        .as_ref()
        .map(|transcript| {
            transcript
                .messages()
                .iter()
                .filter(|message| message.kind == SessionMessageKind::UserPrompt)
                .map(|message| message.content.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Opens review mode and serves cached review or loading status.
///
/// Reviews are auto-generated when sessions transition to `Review`. When the
/// user presses `f` and no cached review exists yet, Agentty requests the
/// current diff in the background, starts generation, and shows a loading
/// message immediately. The resulting review is appended into the normal
/// session output panel instead of replacing it, and successful review text is
/// persisted for restart hydration.
fn open_review_output_mode(app: &mut App, view_context: &ViewContext) {
    if let Some(cached) = app.review_cache.get(view_context.session_id.as_str())
        && !matches!(cached, ReviewCacheEntry::Suppressed)
    {
        return;
    }
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return;
    }

    app.start_manual_review_diff_load(&view_context.session_id);
}

/// Opens diff mode only when the viewed session has actual worktree changes.
///
/// Returns `true` when diff mode was opened and `false` when the session diff
/// is empty, which keeps the view page in place so the `d` shortcut behaves as
/// unavailable for unchanged review sessions.
fn show_diff_for_view_session(app: &mut App, view_context: &ViewContext) -> bool {
    if app
        .sessions
        .session_at(view_context.session_index)
        .is_none_or(|session| session.id != view_context.session_id)
    {
        return false;
    }

    app.start_diff_view_load(
        &view_context.session_id,
        None,
        DiffSidebarFocus::Files,
        false,
    )
}

/// Starts session sync and reports whether the rebase command was accepted.
async fn rebase_view_session(app: &mut App, session_id: &str) -> bool {
    if let Err(error) = app.rebase_session(session_id).await {
        app.append_output_for_session(session_id, &TranscriptNotice::RebaseError.format(error))
            .await;

        return false;
    }

    true
}

#[cfg(test)]
#[path = "session_view_test.rs"]
mod tests;

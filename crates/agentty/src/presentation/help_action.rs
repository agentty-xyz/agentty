use crate::domain::session::{PublishBranchAction, Session, Status};
use crate::presentation::app_mode::{DiffFocus, DiffSidebarFocus};

/// Footer shortcut label for prompt image paste.
///
/// Keeps prompt-mode and draft-view image paste shortcut labels aligned.
pub(crate) const PROMPT_IMAGE_PASTE_SHORTCUT_LABEL: &str = "Ctrl+V/Alt+V";

/// One user-visible shortcut entry that can be rendered in the footer and
/// in the help popup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpAction {
    pub(crate) footer_label: &'static str,
    pub(crate) key: &'static str,
    pub(crate) popup_label: &'static str,
}

impl HelpAction {
    /// Creates one help action descriptor.
    pub(crate) const fn new(
        footer_label: &'static str,
        key: &'static str,
        popup_label: &'static str,
    ) -> Self {
        Self {
            footer_label,
            key,
            popup_label,
        }
    }
}

/// Shared list-mode shortcuts available before tab-specific actions.
const LIST_BASE_ACTIONS: [HelpAction; 2] = [
    HelpAction::new("quit", "q", "Quit"),
    HelpAction::new("sync", "s", "Sync"),
];

/// Full session-view scroll shortcuts shown in the help overlay.
const VIEW_OUTPUT_SCROLL_ACTIONS: [HelpAction; 5] = [
    HelpAction::new("scroll", "j/k", "Scroll output"),
    HelpAction::new("top", "g", "Scroll to top"),
    HelpAction::new("bottom", "G", "Scroll to bottom"),
    HelpAction::new("half down", "Ctrl+d", "Half page down"),
    HelpAction::new("half up", "Ctrl+u", "Half page up"),
];

/// Compact trailing session-view footer shortcuts.
const VIEW_FOOTER_TRAILING_ACTIONS: [HelpAction; 2] = [
    HelpAction::new("scroll", "j/k", "Scroll output"),
    HelpAction::new("help", "?", "Help"),
];

/// Prompt image paste shortcut shared by full help and compact draft footers.
const PROMPT_IMAGE_PASTE_ACTION: HelpAction = HelpAction::new(
    "paste image",
    PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
    "Paste image",
);

/// Command-menu shortcut shared by full help and compact editable footers.
const COMMANDS_MENU_ACTION: HelpAction =
    HelpAction::new("commands menu", "/", "Open commands menu");

/// Encodes which shortcut family is available for the viewed session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewSessionState {
    /// Coordinator-owned worker is available for transcript and diff
    /// inspection only.
    Managed,
    /// Temporary research child exposes transcript and discarded-diff
    /// evidence but no ownership-transfer or worktree-open actions.
    ManagedResearch,
    /// Campaign controller keeps chat plus deterministic plan and integration
    /// board actions while owning no branch changes.
    Orchestrator,
    /// Session is completed; a seeded continuation prompt can be opened.
    Done,
    /// Session was canceled locally; view mode stays read-only.
    Canceled,
    /// Review request merged remotely; only read-only evidence and
    /// navigation actions remain while local target sync is pending.
    Merged,
    /// Session is currently running; queued replies, queued sync, and stop
    /// remain available while worktree-open and diff shortcuts are hidden.
    InProgress,
    /// Session is syncing through the rebase workflow; queued replies remain
    /// available while worktree-open and diff shortcuts are hidden.
    Rebasing,
    /// Session is in merge-queue processing; only read-only navigation
    /// shortcuts are available.
    MergeQueue,
    /// Session is still collecting staged draft messages before its first
    /// live turn starts.
    NewSession,
    /// Stacked draft is staged beneath a parent branch; draft editing remains
    /// available, start appears only when the parent stack is ready, and merge
    /// and sync are hidden until launch.
    StackedDraft,
    /// Session is ready for review; reply, worktree-open, merge, sync,
    /// review, and diff shortcuts are available.
    Review,
    /// Session is generating focused review output; reply, worktree-open,
    /// merge, sync, review, and diff stay available.
    AgentReview,
    /// Session allows reply and merge actions but is not in review mode, so
    /// diff and sync remain hidden.
    Interactive,
}

/// Two-state availability for view-mode actions that need stronger typing
/// than another raw boolean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewActionAvailability {
    Disabled,
    Enabled,
}

impl ViewActionAvailability {
    /// Returns an availability value for the provided boolean.
    pub(crate) fn from_bool(is_enabled: bool) -> Self {
        if is_enabled {
            return Self::Enabled;
        }

        Self::Disabled
    }

    /// Returns whether the action is available.
    fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

/// Action availability snapshot for view-mode help projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ViewHelpState {
    /// Whether this session can fork into a new independent session.
    pub(crate) can_fork_session: ViewActionAvailability,
    /// Whether this session can enter the merge queue under the current
    /// stack consistency rules.
    pub(crate) can_merge_session_branch: ViewActionAvailability,
    /// Whether this session can start branch-mutating work under the current
    /// stack consistency rules.
    pub(crate) can_mutate_session_branch: ViewActionAvailability,
    /// Whether the current session currently has a local worktree directory
    /// available to open.
    pub(crate) can_open_worktree: ViewActionAvailability,
    /// Whether this session can start sync work under the current stack
    /// consistency rules.
    pub(crate) can_rebase_session_branch: ViewActionAvailability,
    /// Whether the current session has a diff available to inspect.
    pub(crate) can_show_diff: ViewActionAvailability,
    /// Whether the current draft session has staged prompts and can launch.
    pub(crate) can_start_staged_session: ViewActionAvailability,
    /// Pull-request publish action available for the current session, when
    /// any.
    pub(crate) publish_pull_request_action: Option<PublishBranchAction>,
    /// Whether the current session can open the reply composer under stack
    /// rules.
    pub(crate) reply_to_session: ViewActionAvailability,
    /// High-level view-mode state that gates the rest of the shortcut set.
    pub(crate) session_state: ViewSessionState,
}

/// Derived view-mode action gates shared by full help and compact footer rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ViewActionSet {
    continue_terminal_session: ViewActionAvailability,
    fork_session: ViewActionAvailability,
    launch_configuration: ViewActionAvailability,
    merge_session: ViewActionAvailability,
    open_prompt: ViewActionAvailability,
    open_worktree: ViewActionAvailability,
    rebase_session: ViewActionAvailability,
    show_diff: ViewActionAvailability,
    show_review: ViewActionAvailability,
    stop_session: ViewActionAvailability,
}

impl ViewActionSet {
    /// Projects a view help state into concrete shortcut availability.
    fn from_state(state: ViewHelpState) -> Self {
        let can_open_worktree = state.can_open_worktree.is_enabled()
            && matches!(
                state.session_state,
                ViewSessionState::Managed
                    | ViewSessionState::Interactive
                    | ViewSessionState::NewSession
                    | ViewSessionState::StackedDraft
                    | ViewSessionState::Review
                    | ViewSessionState::AgentReview
            );
        let can_open_prompt = can_open_view_prompt(state.session_state, state.reply_to_session);
        let can_launch_configuration = can_open_view_command(
            state.session_state,
            state.can_mutate_session_branch,
            state.reply_to_session,
        );
        let can_merge_session =
            can_merge_view_session(state.session_state, state.can_merge_session_branch);
        let can_rebase_session =
            can_rebase_view_session(state.session_state, state.can_rebase_session_branch);
        let can_show_review = matches!(
            state.session_state,
            ViewSessionState::Review | ViewSessionState::AgentReview
        );
        let can_show_diff = state.can_show_diff.is_enabled()
            && (can_show_review
                || matches!(
                    state.session_state,
                    ViewSessionState::Merged
                        | ViewSessionState::Managed
                        | ViewSessionState::ManagedResearch
                ));

        Self {
            continue_terminal_session: ViewActionAvailability::from_bool(matches!(
                state.session_state,
                ViewSessionState::Done | ViewSessionState::Canceled
            )),
            fork_session: ViewActionAvailability::from_bool(
                state.can_fork_session.is_enabled() && can_show_review,
            ),
            launch_configuration: ViewActionAvailability::from_bool(can_launch_configuration),
            merge_session: ViewActionAvailability::from_bool(can_merge_session),
            open_prompt: ViewActionAvailability::from_bool(can_open_prompt),
            open_worktree: ViewActionAvailability::from_bool(can_open_worktree),
            rebase_session: ViewActionAvailability::from_bool(can_rebase_session),
            show_diff: ViewActionAvailability::from_bool(can_show_diff),
            show_review: ViewActionAvailability::from_bool(can_show_review),
            stop_session: ViewActionAvailability::from_bool(
                state.session_state == ViewSessionState::InProgress,
            ),
        }
    }
}

/// Maps one session snapshot into the shared view-mode shortcut state used by
/// both runtime handlers and footer rendering.
pub(crate) fn session_view_state(session: &Session) -> ViewSessionState {
    if session.role == crate::domain::session::SessionRole::OrchestrationResearcher {
        return ViewSessionState::ManagedResearch;
    }
    if session.is_managed() {
        return ViewSessionState::Managed;
    }
    if session.role == crate::domain::session::SessionRole::Orchestrator {
        return ViewSessionState::Orchestrator;
    }
    if !session.owns_branch_changes()
        && !matches!(
            session.status,
            Status::InProgress | Status::Done | Status::Canceled
        )
    {
        return ViewSessionState::Interactive;
    }

    match session.status {
        Status::Done => ViewSessionState::Done,
        Status::Canceled => ViewSessionState::Canceled,
        Status::InProgress => ViewSessionState::InProgress,
        Status::Draft if session.is_draft_session() && session.is_stacked_child() => {
            ViewSessionState::StackedDraft
        }
        Status::Draft if session.is_draft_session() => ViewSessionState::NewSession,
        Status::Draft | Status::Question => ViewSessionState::Interactive,
        Status::Rebasing => ViewSessionState::Rebasing,
        Status::Merging | Status::Queued => ViewSessionState::MergeQueue,
        Status::Merged => ViewSessionState::Merged,
        Status::Review => ViewSessionState::Review,
        Status::AgentReview => ViewSessionState::AgentReview,
    }
}

/// Returns help actions for the sessions page in list mode.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn session_list_actions(
    can_cancel_selected_session: bool,
    can_open_selected_session: bool,
) -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    append_session_list_selection_actions(
        &mut actions,
        can_cancel_selected_session,
        can_open_selected_session,
    );
    actions.push(HelpAction::new("project", "p", "Switch project"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help actions for the projects page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn project_list_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("select", "Enter", "Select active project"));
    actions.push(HelpAction::new("nav", "j/k", "Navigate projects"));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact projects footer actions for the page-level hint line.
pub(crate) fn project_list_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("select", "Enter", "Select active project"),
        HelpAction::new("nav", "j/k", "Navigate projects"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns compact session list footer actions for the page-level hint line,
/// including `c` only when the selected session can be canceled.
pub(crate) fn session_list_footer_actions(
    can_cancel_selected_session: bool,
    can_open_selected_session: bool,
) -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    append_session_list_selection_actions(
        &mut actions,
        can_cancel_selected_session,
        can_open_selected_session,
    );
    actions.push(HelpAction::new("projects", "p", "Switch project"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Appends actions that operate on the currently selected session row.
fn append_session_list_selection_actions(
    actions: &mut Vec<HelpAction>,
    can_cancel_selected_session: bool,
    can_open_selected_session: bool,
) {
    actions.push(HelpAction::new("new session", "a", "Choose session type"));

    if can_cancel_selected_session {
        actions.push(HelpAction::new("cancel", "c", "Cancel session"));
    }

    if can_open_selected_session {
        actions.push(HelpAction::new("open session", "Enter", "Open session"));
    }

    actions.push(HelpAction::new("nav", "j/k", "Navigate sessions"));
}

/// Returns help actions for the settings page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn settings_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("nav", "j/k", "Navigate settings"));
    actions.push(HelpAction::new(
        "open/edit",
        "Enter",
        "Open selector or command editor",
    ));
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact settings footer actions for the page-level hint line.
pub(crate) fn settings_footer_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("nav", "j/k", "Navigate settings"),
        HelpAction::new("open/edit", "Enter", "Open selector or command editor"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Projects currently available view-mode actions into help entries.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn view_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let action_set = ViewActionSet::from_state(state);
    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    append_view_prompt_actions(
        &mut actions,
        state.session_state,
        action_set.open_prompt.is_enabled(),
        action_set.launch_configuration.is_enabled(),
    );

    append_view_stop_action(&mut actions, action_set);

    if state.can_start_staged_session.is_enabled()
        && matches!(
            state.session_state,
            ViewSessionState::NewSession | ViewSessionState::StackedDraft
        )
    {
        actions.push(HelpAction::new("start", "s", "Start staged session"));
    }

    append_view_open_action(&mut actions, action_set);

    if action_set.show_diff.is_enabled() {
        actions.push(HelpAction::new("diff", "d", "Show diff"));
    }

    append_orchestration_actions(&mut actions, state.session_state);

    append_view_review_actions(&mut actions, state, action_set);

    if action_set.merge_session.is_enabled() {
        actions.push(HelpAction::new(
            "add to merge queue",
            "m",
            "Add to merge queue",
        ));
    }

    if action_set.rebase_session.is_enabled() {
        actions.push(HelpAction::new("sync", "r", "Sync"));
    }

    append_view_continue_action(&mut actions, action_set);
    actions.extend(VIEW_OUTPUT_SCROLL_ACTIONS);
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns full session-view help actions with linked review comments when
/// available outside sessions that support terminal continuation.
pub(crate) fn view_actions_with_review_comments(
    state: ViewHelpState,
    can_view_review_comments: bool,
) -> Vec<HelpAction> {
    let mut actions = view_actions(state);
    append_review_comment_action(
        &mut actions,
        can_append_review_comments(state.session_state, can_view_review_comments),
    );

    actions
}

/// Returns compact session-view footer actions for the page-level hint line.
///
/// Review-oriented and running sessions keep sync controls discoverable in
/// the footer.
pub(crate) fn view_footer_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let action_set = ViewActionSet::from_state(state);
    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];

    append_view_footer_edit_actions(
        &mut actions,
        state.session_state,
        state.can_start_staged_session,
        action_set.open_prompt,
        action_set.launch_configuration,
        action_set.merge_session,
        action_set.rebase_session,
    );
    append_view_stop_action(&mut actions, action_set);
    append_view_open_action(&mut actions, action_set);
    append_orchestration_actions(&mut actions, state.session_state);
    append_view_review_actions(&mut actions, state, action_set);
    append_view_continue_action(&mut actions, action_set);
    actions.extend(VIEW_FOOTER_TRAILING_ACTIONS);

    actions
}

/// Returns whether linked review comments should be added to session-view
/// actions without competing with terminal continuation on `c`.
fn can_append_review_comments(
    session_state: ViewSessionState,
    can_view_review_comments: bool,
) -> bool {
    match session_state {
        ViewSessionState::Done | ViewSessionState::Canceled => false,
        _ => can_view_review_comments,
    }
}

/// Adds the comments shortcut before trailing navigation actions.
fn append_review_comment_action(actions: &mut Vec<HelpAction>, is_available: bool) {
    if !is_available {
        return;
    }

    let insertion_index = 1.min(actions.len());
    actions.insert(
        insertion_index,
        HelpAction::new("comments", "c", "Show review comments"),
    );
}

/// Appends the stop action shared by full help and compact footer rows.
fn append_view_stop_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.stop_session.is_enabled() {
        actions.push(HelpAction::new(
            "stop",
            "Ctrl+c",
            "Stop current turn (pops one queued chat message at a time first)",
        ));
    }
}

/// Appends the worktree-open action shared by full help and compact footer
/// rows.
fn append_view_open_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.open_worktree.is_enabled() {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }
}

/// Appends deterministic campaign and ownership-transfer actions.
fn append_orchestration_actions(actions: &mut Vec<HelpAction>, session_state: ViewSessionState) {
    match session_state {
        ViewSessionState::Managed => {
            actions.push(HelpAction::new("detach", "D", "Detach managed worker"));
        }
        ViewSessionState::Orchestrator => {
            actions.push(HelpAction::new("approve", "a", "Approve campaign step"));
        }
        _ => {}
    }
}

/// Appends review and publish actions shared by full help and compact footer
/// rows.
fn append_view_review_actions(
    actions: &mut Vec<HelpAction>,
    state: ViewHelpState,
    action_set: ViewActionSet,
) {
    if action_set.show_review.is_enabled() {
        actions.push(HelpAction::new("review", "f", "Focused review"));
    }

    if action_set.fork_session.is_enabled() {
        actions.push(HelpAction::new("fork", "F", "Fork session"));
    }

    if state.session_state != ViewSessionState::Merged
        && let Some(publish_pull_request_action) = state.publish_pull_request_action
    {
        actions.push(publish_pull_request_help_action(
            publish_pull_request_action,
        ));
    }
}

/// Appends the terminal continuation action shared by full help and compact
/// footer rows.
fn append_view_continue_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.continue_terminal_session.is_enabled() {
        actions.push(HelpAction::new("continue", "c", "Continue in new session"));
    }
}

/// Returns whether a session state can open the reply composer in view mode.
fn can_open_view_prompt(
    session_state: ViewSessionState,
    reply_to_session: ViewActionAvailability,
) -> bool {
    if matches!(
        session_state,
        ViewSessionState::NewSession | ViewSessionState::StackedDraft
    ) {
        return true;
    }

    reply_to_session.is_enabled()
        && matches!(
            session_state,
            ViewSessionState::Rebasing
                | ViewSessionState::Interactive
                | ViewSessionState::Orchestrator
                | ViewSessionState::Review
                | ViewSessionState::AgentReview
        )
}

/// Returns whether a session state can open slash commands in view mode under
/// stack branch-mutation and reply constraints.
fn can_open_view_command(
    session_state: ViewSessionState,
    can_mutate_session_branch: ViewActionAvailability,
    reply_to_session: ViewActionAvailability,
) -> bool {
    if matches!(
        session_state,
        ViewSessionState::NewSession | ViewSessionState::StackedDraft
    ) {
        return true;
    }

    (can_mutate_session_branch.is_enabled() || reply_to_session.is_enabled())
        && matches!(
            session_state,
            ViewSessionState::Interactive
                | ViewSessionState::Orchestrator
                | ViewSessionState::Review
                | ViewSessionState::AgentReview
        )
}

/// Returns whether a session state can start sync in view mode under stack
/// sync constraints.
///
/// Running and review-ready sessions expose sync when the stack is idle.
fn can_rebase_view_session(
    session_state: ViewSessionState,
    can_rebase_session_branch: ViewActionAvailability,
) -> bool {
    can_rebase_session_branch.is_enabled()
        && matches!(
            session_state,
            ViewSessionState::InProgress | ViewSessionState::Review | ViewSessionState::AgentReview
        )
}

/// Returns whether a session state can enter the merge queue in view mode
/// under stack merge constraints.
fn can_merge_view_session(
    session_state: ViewSessionState,
    can_merge_session_branch: ViewActionAvailability,
) -> bool {
    can_merge_session_branch.is_enabled()
        && matches!(
            session_state,
            ViewSessionState::NewSession
                | ViewSessionState::Interactive
                | ViewSessionState::Review
                | ViewSessionState::AgentReview
        )
}

/// Appends footer actions that operate on an editable session in their
/// canonical order.
///
/// The explicit draft-session start action stays immediately after the draft
/// edit action so it remains visible in standard-width terminals. Stacked
/// drafts keep only draft-editing and start actions until their first turn
/// launches.
fn append_view_footer_edit_actions(
    actions: &mut Vec<HelpAction>,
    session_state: ViewSessionState,
    can_start_staged_session: ViewActionAvailability,
    can_open_prompt: ViewActionAvailability,
    can_launch_configuration: ViewActionAvailability,
    can_merge_session: ViewActionAvailability,
    can_rebase_session: ViewActionAvailability,
) {
    if !can_open_prompt.is_enabled()
        && !can_launch_configuration.is_enabled()
        && !can_merge_session.is_enabled()
        && !can_rebase_session.is_enabled()
        && !can_start_staged_session.is_enabled()
    {
        return;
    }

    if can_open_prompt.is_enabled() {
        actions.push(prompt_action_help_action(session_state));
    }

    if can_start_staged_session.is_enabled() {
        actions.push(HelpAction::new("start", "s", "Start staged session"));
    }

    if can_open_prompt.is_enabled() {
        append_prompt_image_paste_action(actions, session_state);
    }

    if can_launch_configuration.is_enabled() {
        actions.push(COMMANDS_MENU_ACTION);
    }

    if can_merge_session.is_enabled() {
        actions.push(HelpAction::new(
            "add to merge queue",
            "m",
            "Add to merge queue",
        ));
    }

    if can_rebase_session.is_enabled() {
        actions.push(HelpAction::new("sync", "r", "Sync"));
    }
}

/// Appends the session-view shortcuts that open the prompt composer.
///
/// Editable sessions expose both `Enter` for a blank composer and `/` for the
/// commands menu with a prefilled leading slash.
fn append_view_prompt_actions(
    actions: &mut Vec<HelpAction>,
    session_state: ViewSessionState,
    can_open_prompt: bool,
    can_launch_configuration: bool,
) {
    if can_open_prompt {
        actions.push(prompt_action_help_action(session_state));
        append_prompt_image_paste_action(actions, session_state);
    }
    if can_launch_configuration {
        actions.push(COMMANDS_MENU_ACTION);
    }
}

/// Appends image paste help when the current session state supports draft
/// attachments from view mode.
fn append_prompt_image_paste_action(
    actions: &mut Vec<HelpAction>,
    session_state: ViewSessionState,
) {
    if prompt_image_paste_allowed(session_state) {
        actions.push(PROMPT_IMAGE_PASTE_ACTION);
    }
}

/// Returns whether view-mode prompt entry can paste images before launch.
fn prompt_image_paste_allowed(session_state: ViewSessionState) -> bool {
    matches!(
        session_state,
        ViewSessionState::NewSession | ViewSessionState::StackedDraft
    )
}

/// Returns the `Enter` prompt-entry action label appropriate for the current
/// session state.
fn prompt_action_help_action(session_state: ViewSessionState) -> HelpAction {
    if matches!(
        session_state,
        ViewSessionState::NewSession | ViewSessionState::StackedDraft
    ) {
        return HelpAction::new("add draft", "Enter", "Add draft");
    }
    if matches!(session_state, ViewSessionState::Rebasing) {
        return HelpAction::new("queue message", "Enter", "Queue message");
    }

    HelpAction::new("reply", "Enter", "Reply")
}

/// Returns help entries for diff-mode actions.
///
/// The help overlay is a complete cross-focus reference; the compact footer
/// below filters actions to those usable in the current sidebar state.
pub(crate) fn diff_actions(can_comment: bool) -> Vec<HelpAction> {
    let mut actions = vec![
        HelpAction::new("back", "q", "Back to session"),
        HelpAction::new("select item", "j/k", "Select file or comment"),
        HelpAction::new(
            if can_comment { "open/comment" } else { "open" },
            "Enter/l",
            if can_comment {
                "Open selected file with Enter/l or edit selected line/comment with Enter"
            } else {
                "Open selected file"
            },
        ),
        HelpAction::new("files", "f/Esc/Left", "Focus changed files"),
        HelpAction::new("comments", "c", "Focus review comments"),
        HelpAction::new("preview", "p", "Toggle selected markdown preview"),
        HelpAction::new(
            "scroll/select row",
            "J/K/Up/Down",
            "Scroll a file or select a changed line/comment",
        ),
    ];
    if can_comment {
        actions.push(HelpAction::new(
            "comment file",
            "Shift+C",
            "Comment on the selected file",
        ));
        actions.push(HelpAction::new(
            "select rows",
            "Shift+V",
            "Start visual changed-row selection",
        ));
        actions.push(HelpAction::new(
            "comment newline",
            "Alt/Shift+Enter",
            "Insert a newline in the diff comment",
        ));
        actions.push(HelpAction::new(
            "save comment",
            "Enter/Esc",
            "Finish the diff comment",
        ));
        actions.push(HelpAction::new(
            "submit comments",
            "s",
            "Submit all diff comments",
        ));
    }
    actions.extend([
        HelpAction::new("select", "Space", "Toggle comment selection"),
        HelpAction::new("submit", "Enter", "Submit selected comments to the agent"),
        HelpAction::new("help", "?", "Help"),
    ]);

    actions
}

/// Inputs that select compact actions for the current Diff footer state.
#[derive(Clone, Copy)]
pub(crate) struct DiffFooterContext {
    /// Whether the selected forge comment can be marked for handling.
    pub(crate) can_mark_selected: bool,
    /// Whether marked forge comments can be submitted.
    pub(crate) can_submit: bool,
    /// Whole-file comment availability for the selected tree row.
    pub(crate) file_comment: DiffFileCommentAvailability,
    /// Changed-file panel that currently receives navigation input.
    pub(crate) focus: DiffFocus,
    /// Whether the diff includes a linked forge-comment sidebar.
    pub(crate) has_review_comments: bool,
    /// Diff comment state for footer editing actions.
    pub(crate) line_comment_state: DiffLineCommentFooterState,
    /// Sidebar section that currently controls the right pane.
    pub(crate) sidebar_focus: DiffSidebarFocus,
}

impl DiffFooterContext {
    /// Appends actions for the sidebar section that currently controls the Diff
    /// pane.
    fn add_sidebar_actions(self, actions: &mut Vec<HelpAction>, can_submit_line_comments: bool) {
        match self.sidebar_focus {
            DiffSidebarFocus::Files if self.focus == DiffFocus::Files => {
                actions.push(HelpAction::new("select file", "j/k", "Select file"));
                actions.push(HelpAction::new(
                    "open",
                    "Enter/l",
                    "Focus the selected file's changed lines",
                ));
                actions.push(HelpAction::new("preview", "p", "Toggle markdown preview"));
                actions.extend(self.file_comment.help_action("comment"));
                if self.has_review_comments {
                    actions.push(HelpAction::new("comments", "c", "Focus review comments"));
                }
            }
            DiffSidebarFocus::Files => {
                actions[0] = HelpAction::new("back", "q", "Back to session");
                actions.push(HelpAction::new("files", "Esc/Left", "Focus changed files"));
                actions.push(HelpAction::new(
                    "select row",
                    "j/k",
                    "Select changed line or inline comment",
                ));
                if matches!(
                    self.line_comment_state,
                    DiffLineCommentFooterState::Ready { .. }
                ) {
                    actions.push(HelpAction::new(
                        "comment",
                        "Enter",
                        "Edit the selected line's or inline comment's feedback",
                    ));
                }
                if can_submit_line_comments {
                    actions.push(diff_comment_submit_action());
                }
            }
            DiffSidebarFocus::Comments => {
                actions[0] = HelpAction::new("back", "q", "Back to session");
                actions.push(HelpAction::new("select comment", "j/k", "Select comment"));
                if self.can_mark_selected {
                    actions.push(HelpAction::new(
                        "select",
                        "Space",
                        "Toggle selected comment for agent evaluation",
                    ));
                }
                if self.can_submit {
                    actions.push(HelpAction::new(
                        "submit",
                        "Enter",
                        "Submit selected comments to the agent",
                    ));
                }
            }
        }
        if self.sidebar_focus == DiffSidebarFocus::Comments {
            actions.push(HelpAction::new("files", "f/Esc", "Focus changed files"));
            actions.push(HelpAction::new(
                "scroll pane",
                "Up/Down",
                "Scroll comment details",
            ));
        }
    }
}

/// Whether the selected diff-tree row supports whole-file comments.
#[derive(Clone, Copy)]
pub(crate) enum DiffFileCommentAvailability {
    /// The selected row is editable and identifies one file.
    Available,
    /// The selected row is read-only or identifies a folder.
    Unavailable,
}

impl DiffFileCommentAvailability {
    /// Maps one computed availability predicate into the footer state.
    pub(crate) fn from_bool(is_available: bool) -> Self {
        if is_available {
            return Self::Available;
        }

        Self::Unavailable
    }

    /// Returns whether whole-file commenting is available.
    fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Returns the whole-file comment action when the selected row supports it.
    fn help_action(self, footer_label: &'static str) -> Option<HelpAction> {
        self.is_available()
            .then(|| HelpAction::new(footer_label, "Shift+C", "Comment on the selected file"))
    }
}

/// Inline-comment state that changes the compact Diff footer actions.
#[derive(Clone, Copy)]
pub(crate) enum DiffLineCommentFooterState {
    /// One diff comment currently receives keyboard input.
    Editing,
    /// The visible session cannot accept a diff-comment reply.
    ReadOnly,
    /// Visual changed-row selection is active for one range comment.
    Selecting,
    /// Diff navigation is active with this many completed comments.
    Ready {
        /// Number of diff comments ready for batch submission.
        comment_count: usize,
    },
}

impl DiffLineCommentFooterState {
    /// Returns whether completed diff comments are ready for submission.
    fn can_submit(self) -> bool {
        matches!(self, Self::Ready { comment_count } if comment_count > 0)
    }
}

/// Returns compact diff footer actions for the page-level hint line.
pub(crate) fn diff_footer_actions(context: DiffFooterContext) -> Vec<HelpAction> {
    if matches!(
        context.line_comment_state,
        DiffLineCommentFooterState::Editing
    ) {
        return diff_comment_editing_actions();
    }
    if matches!(
        context.line_comment_state,
        DiffLineCommentFooterState::Selecting
    ) {
        let mut actions = vec![
            HelpAction::new("back", "q", "Back to session"),
            HelpAction::new("cancel", "Esc", "Cancel row selection"),
            HelpAction::new("extend", "j/k", "Extend row selection"),
            HelpAction::new("comment", "Enter", "Comment on selected rows"),
        ];
        actions.extend(context.file_comment.help_action("comment file"));

        return actions;
    }

    let can_submit_line_comments = context.line_comment_state.can_submit();
    let mut actions = vec![HelpAction::new("back", "q/Esc", "Back to session")];
    if can_submit_line_comments
        && !(context.sidebar_focus == DiffSidebarFocus::Files
            && context.focus == DiffFocus::Content)
    {
        actions.push(diff_comment_submit_action());
    }
    context.add_sidebar_actions(&mut actions, can_submit_line_comments);
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns compact actions for the active multiline diff comment editor.
fn diff_comment_editing_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new(
            "newline",
            "Alt/Shift+Enter",
            "Insert a newline in the diff comment",
        ),
        HelpAction::new("save comment", "Enter/Esc", "Finish the diff comment"),
    ]
}

/// Builds the shared diff-comment submission action for every Diff pane.
fn diff_comment_submit_action() -> HelpAction {
    HelpAction::new("submit comments", "s", "Submit all diff comments")
}

/// Returns list-mode actions shared by all tabs.
fn list_base_actions() -> Vec<HelpAction> {
    Vec::from(LIST_BASE_ACTIONS)
}

/// Returns the view-mode shortcut entry for the current pull-request publish
/// action.
fn publish_pull_request_help_action(action: PublishBranchAction) -> HelpAction {
    match action {
        PublishBranchAction::PublishPullRequest | PublishBranchAction::Push => {
            HelpAction::new("PR", "p", "Create or refresh forge review request")
        }
    }
}

#[cfg(test)]
#[path = "help_action_test.rs"]
mod tests;

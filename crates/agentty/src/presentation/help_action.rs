use crate::domain::session::{PublishBranchAction, Session, Status};

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
    /// Session is completed; a seeded continuation prompt can be opened.
    Done,
    /// Session was canceled locally; view mode stays read-only.
    Canceled,
    /// Session is currently running; queued replies, queued sync, and stop
    /// remain available while worktree-open and diff shortcuts are hidden.
    InProgress,
    /// Session is syncing through the rebase workflow; worktree-open, reply,
    /// and diff shortcuts are hidden until sync finishes.
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
    /// one-level stack consistency rules.
    pub(crate) can_merge_session_branch: ViewActionAvailability,
    /// Whether this session can start branch-mutating work under the current
    /// one-level stack consistency rules.
    pub(crate) can_mutate_session_branch: ViewActionAvailability,
    /// Whether the current session currently has a local worktree directory
    /// available to open.
    pub(crate) can_open_worktree: ViewActionAvailability,
    /// Whether this session can start sync work under the current one-level
    /// stack consistency rules.
    pub(crate) can_rebase_session_branch: ViewActionAvailability,
    /// Whether the current session can open the reply composer under stack
    /// rules.
    pub(crate) reply_to_session: ViewActionAvailability,
    /// Whether the current draft session has staged prompts and can launch.
    pub(crate) can_start_staged_session: ViewActionAvailability,
    /// Pull-request publish action available for the current session, when
    /// any.
    pub(crate) publish_pull_request_action: Option<PublishBranchAction>,
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
                ViewSessionState::Interactive
                    | ViewSessionState::NewSession
                    | ViewSessionState::StackedDraft
                    | ViewSessionState::Review
                    | ViewSessionState::AgentReview
            );
        let can_open_prompt = can_open_view_prompt(state.session_state, state.reply_to_session);
        let can_launch_configuration =
            can_open_view_command(state.session_state, state.can_mutate_session_branch);
        let can_merge_session =
            can_merge_view_session(state.session_state, state.can_merge_session_branch);
        let can_rebase_session =
            can_rebase_view_session(state.session_state, state.can_rebase_session_branch);
        let can_show_review = matches!(
            state.session_state,
            ViewSessionState::Review | ViewSessionState::AgentReview
        );

        Self {
            continue_terminal_session: ViewActionAvailability::from_bool(matches!(
                state.session_state,
                ViewSessionState::Done
            )),
            fork_session: ViewActionAvailability::from_bool(
                state.can_fork_session.is_enabled() && can_show_review,
            ),
            launch_configuration: ViewActionAvailability::from_bool(can_launch_configuration),
            merge_session: ViewActionAvailability::from_bool(can_merge_session),
            open_prompt: ViewActionAvailability::from_bool(can_open_prompt),
            open_worktree: ViewActionAvailability::from_bool(can_open_worktree),
            rebase_session: ViewActionAvailability::from_bool(can_rebase_session),
            show_diff: ViewActionAvailability::from_bool(can_show_review),
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

/// Returns help actions for the requested-review page.
pub(crate) fn review_actions() -> Vec<HelpAction> {
    let mut actions = vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("nav", "j/k", "Navigate reviews"),
        HelpAction::new("open", "Enter", "Open review details"),
        HelpAction::new("refresh", "s", "Refresh requested reviews"),
    ];
    actions.push(HelpAction::new("next tab", "Tab", "Switch tab"));
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help actions for the assigned-issue list page.
pub(crate) fn issue_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("quit", "q", "Quit"),
        HelpAction::new("nav", "j/k", "Navigate issues"),
        HelpAction::new("open", "Enter", "Open issue details"),
        HelpAction::new("refresh", "s", "Refresh assigned issues"),
        HelpAction::new("next tab", "Tab", "Switch tab"),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns help actions for one assigned-issue detail page.
pub(crate) fn issue_detail_actions() -> Vec<HelpAction> {
    vec![
        HelpAction::new("back", "q/Esc", "Back to issues"),
        HelpAction::new("scroll", "j/k", "Scroll issue details"),
        HelpAction::new("page", "Ctrl+d/u", "Scroll half page"),
        HelpAction::new("top/bottom", "g/G", "Jump to top or bottom"),
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

    if state.can_start_staged_session.is_enabled() {
        actions.push(HelpAction::new("start", "s", "Start staged session"));
    }

    append_view_open_action(&mut actions, action_set);

    if action_set.show_diff.is_enabled() {
        actions.push(HelpAction::new("diff", "d", "Show diff"));
    }

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
    append_view_review_actions(&mut actions, state, action_set);
    append_view_continue_action(&mut actions, action_set);
    actions.extend(VIEW_FOOTER_TRAILING_ACTIONS);

    actions
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

    if let Some(publish_pull_request_action) = state.publish_pull_request_action {
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
            ViewSessionState::Interactive
                | ViewSessionState::Review
                | ViewSessionState::AgentReview
        )
}

/// Returns whether a session state can open slash commands in view mode under
/// stack branch-mutation constraints.
fn can_open_view_command(
    session_state: ViewSessionState,
    can_mutate_session_branch: ViewActionAvailability,
) -> bool {
    if matches!(
        session_state,
        ViewSessionState::NewSession | ViewSessionState::StackedDraft
    ) {
        return true;
    }

    can_mutate_session_branch.is_enabled()
        && matches!(
            session_state,
            ViewSessionState::Interactive
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

    HelpAction::new("reply", "Enter", "Reply")
}

/// Returns help entries for diff-mode actions.
///
/// These entries are used by the help overlay and include all available
/// actions. The `c` shortcut label flips based on `right_panel` so users see
/// which panel they'd switch to.
pub(crate) fn diff_actions(right_panel: super::app_mode::DiffRightPanel) -> Vec<HelpAction> {
    vec![
        HelpAction::new("back", "q/Esc", "Back to session"),
        HelpAction::new("select file", "j/k", "Select file"),
        HelpAction::new("scroll pane", "Up/Down", "Scroll right panel"),
        HelpAction::new(
            diff_toggle_comments_label(right_panel),
            "c",
            diff_toggle_comments_label(right_panel),
        ),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns compact diff footer actions for the page-level hint line.
pub(crate) fn diff_footer_actions(right_panel: super::app_mode::DiffRightPanel) -> Vec<HelpAction> {
    vec![
        HelpAction::new("back", "q/Esc", "Back to session"),
        HelpAction::new("select file", "j/k", "Select file"),
        HelpAction::new(
            diff_toggle_comments_label(right_panel),
            "c",
            diff_toggle_comments_label(right_panel),
        ),
        HelpAction::new("help", "?", "Help"),
    ]
}

/// Returns the user-facing label for the `c` shortcut based on which right
/// panel is currently active.
fn diff_toggle_comments_label(right_panel: super::app_mode::DiffRightPanel) -> &'static str {
    match right_panel {
        super::app_mode::DiffRightPanel::Diff => "Show comments",
        super::app_mode::DiffRightPanel::Comments => "Show diff",
    }
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
mod tests {
    use super::*;
    use crate::domain::session::PublishBranchAction;

    #[test]
    fn test_project_list_actions_exclude_new_session_shortcut() {
        // Arrange
        // Act
        let actions = project_list_actions();

        // Assert
        assert!(!actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_settings_actions_exclude_new_session_shortcut() {
        // Arrange
        // Act
        let actions = settings_actions();

        // Assert
        assert!(!actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_session_list_actions_include_new_session_shortcut() {
        // Arrange
        // Act
        let actions = session_list_actions(false, false);

        // Assert
        assert!(actions.iter().any(|action| action.key == "a"));
    }

    #[test]
    fn test_session_list_actions_include_switch_project_shortcut() {
        // Arrange
        // Act
        let actions = session_list_actions(false, false);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "p" && action.popup_label == "Switch project")
        );
    }

    #[test]
    fn test_session_list_actions_hide_enter_without_openable_session() {
        // Arrange
        // Act
        let actions = session_list_actions(false, false);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "j/k"));
    }

    #[test]
    fn test_session_list_footer_actions_hides_non_critical_session_commands() {
        // Arrange

        // Act
        let actions = session_list_footer_actions(false, true);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "a"));
        assert!(
            actions
                .iter()
                .any(|action| action.key == "p" && action.footer_label == "projects")
        );
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "c"));
        assert!(!actions.iter().any(|action| action.key == "Tab"));
    }

    #[test]
    fn test_session_list_footer_actions_includes_cancel_when_session_is_cancelable() {
        // Arrange

        // Act
        let actions = session_list_footer_actions(true, true);

        // Assert
        assert!(actions.iter().any(|action| action.key == "c"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "Tab"));
    }

    #[test]
    fn test_view_actions_in_progress_shows_stop_and_sync_and_hides_open_and_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::InProgress,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(actions.iter().any(|action| action.key == "r"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_hide_open_when_worktree_is_unavailable() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Disabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "o"));
    }

    #[test]
    fn test_view_actions_rebasing_hides_open_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Rebasing,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::MergeQueue,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_review_shows_diff() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(
            actions
                .iter()
                .any(|action| action.key == "p" && action.footer_label == "PR")
        );
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "F"));
        assert!(actions.iter().any(|action| {
            action.key == "/"
                && action.footer_label == "commands menu"
                && action.popup_label == "Open commands menu"
        }));
        assert!(!actions.iter().any(|action| action.key == "S-Tab"));
        assert!(
            actions
                .iter()
                .any(|action| action.key == "f" && action.popup_label == "Focused review")
        );
    }

    #[test]
    fn test_view_actions_review_hides_fork_when_unavailable() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Disabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "F"));
    }

    #[test]
    fn test_view_actions_review_keeps_reply_merge_and_sync_when_stack_blocks_commands() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Disabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_review_uses_merge_gate_separately_from_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Disabled,
            can_rebase_session_branch: ViewActionAvailability::Disabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
        assert!(!actions.iter().any(|action| action.key == "/"));
    }

    #[test]
    fn test_view_actions_agent_review_shows_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::AgentReview,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "d"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_interactive_hides_diff() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Interactive,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "f"));
        assert!(!actions.iter().any(|action| action.key == "r"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
    }

    #[test]
    fn test_view_actions_new_session_shows_add_draft_and_start() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Enabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::NewSession,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Enter" && action.footer_label == "add draft")
        );
        assert!(actions.iter().any(|action| {
            action.key == PROMPT_IMAGE_PASTE_SHORTCUT_LABEL && action.footer_label == "paste image"
        }));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "s"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_stacked_draft_shows_start_and_hides_merge_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Enabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::StackedDraft,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Enter" && action.footer_label == "add draft")
        );
        assert!(actions.iter().any(|action| {
            action.key == PROMPT_IMAGE_PASTE_SHORTCUT_LABEL && action.footer_label == "paste image"
        }));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "s"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_stacked_draft_hides_start_without_staged_drafts() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::StackedDraft,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(
            actions
                .iter()
                .any(|action| action.key == "Enter" && action.footer_label == "add draft")
        );
        assert!(!actions.iter().any(|action| action.key == "s"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_actions_done_shows_continue_and_hides_edit_actions() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Done,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "c"));
        assert!(!actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "d"));
        assert!(!actions.iter().any(|action| action.key == "f"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_review_shows_advanced_actions() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "F"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_review_hides_fork_when_unavailable() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Disabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "F"));
    }

    #[test]
    fn test_view_footer_actions_review_keeps_reply_merge_and_sync_when_stack_blocks_commands() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Disabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_review_uses_merge_gate_separately_from_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Disabled,
            can_rebase_session_branch: ViewActionAvailability::Disabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
        assert!(!actions.iter().any(|action| action.key == "/"));
    }

    #[test]
    fn test_view_footer_actions_agent_review_shows_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_state: ViewSessionState::AgentReview,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "o"));
        assert!(actions.iter().any(|action| action.key == "f"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "p"));
        assert!(actions.iter().any(|action| action.key == "m"));
        assert!(actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_rebasing_hides_open_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Rebasing,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
    }

    #[test]
    fn test_view_footer_actions_new_session_keeps_edit_actions_grouped() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Enabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::NewSession,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            &ordered_keys[..6],
            [
                "q",
                "Enter",
                "s",
                PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
                "/",
                "m"
            ]
        );
    }

    #[test]
    fn test_view_footer_actions_stacked_draft_shows_start_and_hides_merge_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Enabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::StackedDraft,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            &ordered_keys[..5],
            ["q", "Enter", "s", PROMPT_IMAGE_PASTE_SHORTCUT_LABEL, "/"]
        );
        assert!(actions.iter().any(|action| action.key == "Enter"));
        assert!(actions.iter().any(|action| action.key == "/"));
        assert!(actions.iter().any(|action| action.key == "s"));
        assert!(!actions.iter().any(|action| action.key == "m"));
        assert!(!actions.iter().any(|action| action.key == "r"));
    }

    #[test]
    fn test_view_footer_actions_merge_queue_hides_worktree_shortcuts_and_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::MergeQueue,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(actions.iter().any(|action| action.key == "q"));
        assert!(actions.iter().any(|action| action.key == "j/k"));
    }

    #[test]
    fn test_view_footer_actions_in_progress_shows_stop_and_sync() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::InProgress,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(&ordered_keys[..4], ["q", "r", "Ctrl+c", "j/k"]);
    }

    #[test]
    fn test_view_actions_canceled_without_branch_publish_action() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Canceled,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "c"));
        assert!(!actions.iter().any(|action| action.key == "p"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "t"));
    }

    #[test]
    fn test_view_footer_actions_done_shows_continue_before_scroll() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Done,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(&ordered_keys[..4], ["q", "c", "j/k", "?"]);
    }

    #[test]
    fn test_view_footer_actions_canceled_hides_continue() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Canceled,
        };

        // Act
        let actions = view_footer_actions(state);
        let ordered_keys = actions.iter().map(|action| action.key).collect::<Vec<_>>();

        // Assert
        assert_eq!(&ordered_keys[..3], ["q", "j/k", "?"]);
    }

    #[test]
    fn test_view_footer_actions_in_progress_shows_stop_and_hides_open() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::InProgress,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(actions.iter().any(|action| action.key == "Ctrl+c"));
        assert!(!actions.iter().any(|action| action.key == "o"));
        assert!(!actions.iter().any(|action| action.key == "Enter"));
        assert!(!actions.iter().any(|action| action.key == "d"));
    }

    #[test]
    fn test_view_actions_review_hides_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    }

    #[test]
    fn test_view_footer_actions_review_hides_stop() {
        // Arrange
        let state = ViewHelpState {
            can_fork_session: ViewActionAvailability::Enabled,
            can_merge_session_branch: ViewActionAvailability::Enabled,
            can_mutate_session_branch: ViewActionAvailability::Enabled,
            can_rebase_session_branch: ViewActionAvailability::Enabled,
            can_open_worktree: ViewActionAvailability::Enabled,
            reply_to_session: ViewActionAvailability::Enabled,
            can_start_staged_session: ViewActionAvailability::Disabled,
            publish_pull_request_action: None,
            session_state: ViewSessionState::Review,
        };

        // Act
        let actions = view_footer_actions(state);

        // Assert
        assert!(!actions.iter().any(|action| action.key == "Ctrl+c"));
    }
}

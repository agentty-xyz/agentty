//! Footer and help-popup shortcut entries, with the action sets available in
//! list, settings, remote, session view, and diff contexts.

use crate::domain::platform_sync::PlatformTaskChoice;
use crate::domain::session::{
    PublishBranchAction, ReviewRequestMergeAction, ReviewWatchToggle, Session, Status,
};
use crate::domain::session_order::ArchiveAction;
use crate::presentation::platform::{PlatformAccess, PlatformState, PlatformTabView};
use crate::presentation::remote_control::RemoteControlView;

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

/// Tab-cycling shortcuts shared by every list-mode tab.
const SWITCH_TAB_ACTIONS: [HelpAction; 2] = [
    HelpAction::new("next tab", "Tab", "Next tab"),
    HelpAction::new("previous tab", "Shift+Tab", "Previous tab"),
];

/// Full session-view scroll shortcuts shown in the help overlay.
const VIEW_OUTPUT_SCROLL_ACTIONS: [HelpAction; 5] = [
    HelpAction::new("scroll", "j/k", "Scroll output"),
    HelpAction::new("top", "g", "Scroll to top"),
    HelpAction::new("bottom", "G", "Scroll to bottom"),
    HelpAction::new("half down", "Ctrl+d", "Half page down"),
    HelpAction::new("half up", "Ctrl+u", "Half page up"),
];

/// Prompt image paste shortcut shown in session-view help.
const PROMPT_IMAGE_PASTE_ACTION: HelpAction = HelpAction::new(
    "paste image",
    PROMPT_IMAGE_PASTE_SHORTCUT_LABEL,
    "Paste image",
);

/// Command-menu shortcut shown in session-view help.
const COMMANDS_MENU_ACTION: HelpAction =
    HelpAction::new("commands menu", "/", "Open commands menu");

/// Encodes which shortcut family is available for the viewed session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewSessionState {
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
    /// Whether this session can enter the local merge queue.
    pub(crate) can_merge_session_branch: ViewActionAvailability,
    /// Whether the current session currently has a local worktree directory
    /// available to open.
    pub(crate) can_open_worktree: ViewActionAvailability,
    /// Whether the current draft session has staged prompts and can launch.
    pub(crate) can_start_staged_session: ViewActionAvailability,
    /// Whether queued chat messages can join the running turn now.
    pub(crate) can_steer_queued_messages: ViewActionAvailability,
    /// Choice offered while the session's task on the platform's Sync
    /// holds it back, when it does.
    pub(crate) platform_task_choice: Option<PlatformTaskChoice>,
    /// Pull-request publish action available for the current session, when
    /// any.
    pub(crate) publish_pull_request_action: Option<PublishBranchAction>,
    /// Pull-request merge available on the forge for the current session,
    /// when its latest refresh reported it ready or ready with bypass.
    pub(crate) review_request_merge_action: Option<ReviewRequestMergeAction>,
    /// High-level view-mode state that gates the rest of the shortcut set.
    pub(crate) session_state: ViewSessionState,
}

/// Derived view-mode action gates used by session-view help.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ViewActionSet {
    continue_terminal_session: ViewActionAvailability,
    fork_session: ViewActionAvailability,
    launch_configuration: ViewActionAvailability,
    merge_session: ViewActionAvailability,
    open_prompt: ViewActionAvailability,
    open_worktree: ViewActionAvailability,
    rebase_session: ViewActionAvailability,
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
                    | ViewSessionState::Review
                    | ViewSessionState::AgentReview
            );
        let can_open_prompt = can_open_view_prompt(state.session_state);
        let can_launch_configuration = can_open_view_command(state.session_state);
        let can_merge_session =
            can_merge_view_session(state.session_state, state.can_merge_session_branch);
        let can_rebase_session = can_rebase_view_session(state.session_state);
        let can_show_review = matches!(
            state.session_state,
            ViewSessionState::Review | ViewSessionState::AgentReview
        );

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
        Status::Draft if session.is_draft_session() => ViewSessionState::NewSession,
        Status::Draft | Status::Question => ViewSessionState::Interactive,
        Status::Rebasing => ViewSessionState::Rebasing,
        Status::Merging | Status::Queued => ViewSessionState::MergeQueue,
        Status::Merged => ViewSessionState::Merged,
        Status::Review | Status::Watching => ViewSessionState::Review,
        Status::AgentReview => ViewSessionState::AgentReview,
    }
}

/// Returns the `Enter` action for one selected archive pagination row.
pub(crate) fn archive_action(action: ArchiveAction) -> HelpAction {
    match action {
        ArchiveAction::LoadMore => {
            HelpAction::new("load more", "Enter", "Load next 10 archived sessions")
        }
        ArchiveAction::ShowLess => HelpAction::new(
            "show less",
            "Enter",
            "Hide archived sessions beyond the first 10",
        ),
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
    actions.extend(SWITCH_TAB_ACTIONS);
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns help actions for the projects page.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn project_list_actions() -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.push(HelpAction::new("select", "Enter", "Select active project"));
    actions.push(HelpAction::new("add", "a", "Add project by path"));
    actions.push(HelpAction::new(
        "discover",
        "d",
        "Discover projects in a folder",
    ));
    actions.push(HelpAction::new("nav", "j/k", "Navigate projects"));
    actions.extend(SWITCH_TAB_ACTIONS);
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
    actions.extend(SWITCH_TAB_ACTIONS);
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns the remote-control actions of a registered Platform tab in the
/// state `view` shows: pairing approval while a device waits, code and link
/// actions while remote control runs, and device actions once a device is
/// paired.
fn remote_control_actions(view: &RemoteControlView) -> Vec<HelpAction> {
    let mut actions = Vec::new();
    if !view.pairing_requests.is_empty() {
        actions.push(HelpAction::new("approve", "y", "Approve pairing request"));
        actions.push(HelpAction::new("deny", "x", "Deny pairing request"));
    }
    if view.relay.is_some() {
        actions.push(HelpAction::new("new code", "n", "New pairing code"));
        actions.push(HelpAction::new("open link", "o", "Open pairing link"));
        actions.push(HelpAction::new("copy link", "c", "Copy pairing link"));
        actions.push(HelpAction::new("stop", "Enter", "Stop remote control"));
    } else {
        actions.push(HelpAction::new("start", "Enter", "Start remote control"));
    }
    if !view.devices.is_empty() {
        actions.push(HelpAction::new("move", "j/k", "Navigate paired devices"));
        actions.push(HelpAction::new("remove", "d", "Remove paired device"));
    }

    actions
}

/// Returns help actions for the Platform tab in the state `view` shows.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn platform_actions(view: PlatformTabView<'_>) -> Vec<HelpAction> {
    let mut actions = list_base_actions();
    actions.extend(platform_tab_actions(view));
    actions.extend(SWITCH_TAB_ACTIONS);
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns the Platform tab's own actions in the state `view` shows:
/// sign-in actions while signed out or signing in, and, once signed in,
/// sign-out and either the remote-control actions of a registered Agentty
/// or another account check, with the keep-on toggle once the check
/// answered.
pub(crate) fn platform_tab_actions(view: PlatformTabView<'_>) -> Vec<HelpAction> {
    match &view.platform.state {
        PlatformState::SignedOut { .. } => {
            vec![HelpAction::new(
                "sign in and connect",
                "Enter",
                "Sign in and start remote control through the platform's relay",
            )]
        }
        PlatformState::SigningOut { .. } => Vec::new(),
        PlatformState::Requesting => vec![HelpAction::new("cancel", "Enter", "Cancel sign-in")],
        PlatformState::SigningIn(_) => vec![
            HelpAction::new("open page", "o", "Open the sign-in page"),
            HelpAction::new("copy link", "c", "Copy the sign-in page link"),
            HelpAction::new("cancel", "Enter", "Cancel sign-in"),
        ],
        PlatformState::SignedIn(account) => {
            let mut actions = match account.access {
                PlatformAccess::Checking => Vec::new(),
                PlatformAccess::Registered { .. } => remote_control_actions(view.remote_control),
                PlatformAccess::NoRelay
                | PlatformAccess::Unavailable { .. }
                | PlatformAccess::Waitlisted => {
                    vec![HelpAction::new(
                        "check again",
                        "Enter",
                        "Check the account again",
                    )]
                }
            };
            if account.offers_keep_on() {
                actions.push(HelpAction::new(
                    "keep on",
                    "a",
                    "Toggle starting remote control whenever Agentty starts",
                ));
            }
            actions.push(HelpAction::new("sign out", "l", "Sign out of the platform"));

            actions
        }
    }
}

/// Projects currently available view-mode actions into help entries.
/// These entries are used by the help overlay and include all available
/// actions.
pub(crate) fn view_actions(state: ViewHelpState) -> Vec<HelpAction> {
    let action_set = ViewActionSet::from_state(state);
    let mut actions = vec![HelpAction::new("back", "q", "Back to list")];
    actions.push(HelpAction::new(
        "graphs",
        "w",
        "Inspect session execution graphs",
    ));
    actions.push(HelpAction::new(
        "usage",
        "u",
        "Show token usage and estimated cost",
    ));

    append_view_prompt_actions(
        &mut actions,
        state.session_state,
        action_set.open_prompt.is_enabled(),
        action_set.launch_configuration.is_enabled(),
    );

    append_view_stop_action(&mut actions, action_set);

    if action_set.stop_session.is_enabled() && state.can_steer_queued_messages.is_enabled() {
        actions.push(HelpAction::new(
            "send now",
            "Ctrl+s",
            "Send queued chat messages into the running turn",
        ));
    }

    match state.platform_task_choice {
        Some(PlatformTaskChoice::Conflict) => {
            actions.push(HelpAction::new(
                "take over",
                "t",
                "Take the platform's Sync task over from the session holding it",
            ));
            actions.push(HelpAction::new(
                "separate work",
                "n",
                "Start as separate work on the platform's Sync",
            ));
        }
        Some(PlatformTaskChoice::Interrupted) => {
            actions.push(HelpAction::new(
                "ask again",
                "s",
                "Ask the platform's Sync again whether another session works on this",
            ));
        }
        None if state.can_start_staged_session.is_enabled()
            && state.session_state == ViewSessionState::NewSession =>
        {
            actions.push(HelpAction::new("start", "s", "Start staged session"));
        }
        None => {}
    }

    append_view_open_action(&mut actions, action_set);

    actions.push(HelpAction::new("diff", "d", "Show diff"));

    append_view_review_actions(&mut actions, state, action_set);

    if action_set.merge_session.is_enabled() {
        actions.push(HelpAction::new(
            "add to merge queue",
            "m",
            "Add to merge queue",
        ));
    }

    if let Some(review_request_merge_action) = state.review_request_merge_action {
        actions.push(review_request_merge_help_action(
            review_request_merge_action,
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

/// Returns full session-view help actions with linked review-request actions:
/// review comments when available outside sessions that support terminal
/// continuation, and the review-request watch toggle.
pub(crate) fn view_actions_with_review_request(
    state: ViewHelpState,
    can_view_review_comments: bool,
    review_watch_toggle: Option<ReviewWatchToggle>,
) -> Vec<HelpAction> {
    let mut actions = view_actions(state);
    append_review_watch_action(&mut actions, review_watch_toggle);
    append_review_comment_action(
        &mut actions,
        can_append_review_comments(state.session_state, can_view_review_comments),
    );

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

/// Adds the review-request watch toggle after the publish action, before the
/// trailing scroll and help actions.
fn append_review_watch_action(
    actions: &mut Vec<HelpAction>,
    review_watch_toggle: Option<ReviewWatchToggle>,
) {
    let action = match review_watch_toggle {
        Some(ReviewWatchToggle::Start) => HelpAction::new(
            "watch",
            "W",
            "Watch review request and resolve checks, comments, and conflicts",
        ),
        Some(ReviewWatchToggle::Stop) => {
            HelpAction::new("stop watching", "W", "Stop watching review request")
        }
        None => return,
    };
    let trailing_action_count = VIEW_OUTPUT_SCROLL_ACTIONS.len() + 1;
    let insertion_index = actions.len().saturating_sub(trailing_action_count);
    actions.insert(insertion_index, action);
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

/// Appends the stop action for running sessions.
fn append_view_stop_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.stop_session.is_enabled() {
        actions.push(HelpAction::new(
            "stop",
            "Ctrl+c",
            "Stop current turn (pops one queued chat message at a time first)",
        ));
    }
}

/// Appends the worktree-open action when the worktree can be opened.
fn append_view_open_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.open_worktree.is_enabled() {
        actions.push(HelpAction::new("open", "o", "Open worktree"));
    }
}

/// Appends review, fork, and publish actions.
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

/// Appends the terminal continuation action for finished sessions.
fn append_view_continue_action(actions: &mut Vec<HelpAction>, action_set: ViewActionSet) {
    if action_set.continue_terminal_session.is_enabled() {
        actions.push(HelpAction::new("continue", "c", "Continue in new session"));
    }
}

/// Returns whether a session state can open the reply composer in view mode.
fn can_open_view_prompt(session_state: ViewSessionState) -> bool {
    matches!(
        session_state,
        ViewSessionState::NewSession
            | ViewSessionState::InProgress
            | ViewSessionState::Rebasing
            | ViewSessionState::Interactive
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    )
}

/// Returns whether a session state can open slash commands in view mode.
fn can_open_view_command(session_state: ViewSessionState) -> bool {
    matches!(
        session_state,
        ViewSessionState::NewSession
            | ViewSessionState::Interactive
            | ViewSessionState::Review
            | ViewSessionState::AgentReview
    )
}

/// Returns whether a session state can start sync in view mode.
///
/// Running and review-ready sessions expose sync.
fn can_rebase_view_session(session_state: ViewSessionState) -> bool {
    matches!(
        session_state,
        ViewSessionState::InProgress | ViewSessionState::Review | ViewSessionState::AgentReview
    )
}

/// Returns whether a session state can enter the merge queue in view mode
/// while no linked review request owns merging.
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
    session_state == ViewSessionState::NewSession
}

/// Returns the `Enter` prompt-entry action label appropriate for the current
/// session state.
fn prompt_action_help_action(session_state: ViewSessionState) -> HelpAction {
    if session_state == ViewSessionState::NewSession {
        return HelpAction::new("add draft", "Enter", "Add draft");
    }
    if matches!(
        session_state,
        ViewSessionState::InProgress | ViewSessionState::Rebasing
    ) {
        return HelpAction::new("queue message", "Enter", "Queue message");
    }

    HelpAction::new("reply", "Enter", "Reply")
}

/// Linked review-comment actions available in the diff help overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffReviewCommentHelp {
    /// No review request is linked, so the comments pane is absent.
    Hidden,
    /// Review comments can be browsed but not sent to the agent.
    ReadOnly,
    /// Actionable review comments can be selected and sent to the agent.
    Reply,
}

impl DiffReviewCommentHelp {
    /// Projects linked-comment presence and reply availability into the
    /// review-comment shortcuts the diff page accepts.
    pub(crate) fn new(has_review_comments: bool, can_reply_review_comments: bool) -> Self {
        match (has_review_comments, can_reply_review_comments) {
            (false, _) => Self::Hidden,
            (true, false) => Self::ReadOnly,
            (true, true) => Self::Reply,
        }
    }
}

/// Returns help entries for diff-mode actions.
///
/// The help overlay is a complete cross-focus reference. Review-comment
/// shortcuts appear only when a review request is linked, and their reply
/// actions only when the session can send comments to the agent.
pub(crate) fn diff_actions(
    can_comment: bool,
    review_comment_help: DiffReviewCommentHelp,
) -> Vec<HelpAction> {
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
        HelpAction::new("files", "f/h/Esc/Left", "Focus changed files"),
    ];
    if review_comment_help != DiffReviewCommentHelp::Hidden {
        actions.push(HelpAction::new("comments", "c", "Focus review comments"));
    }
    actions.extend([
        HelpAction::new("preview", "p", "Toggle selected markdown preview"),
        HelpAction::new(
            "scroll/select row",
            "J/K/Up/Down",
            "Scroll a file or select a changed line/comment",
        ),
    ]);
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
    if review_comment_help == DiffReviewCommentHelp::Reply {
        actions.extend([
            HelpAction::new("select", "Space", "Toggle comment selection"),
            HelpAction::new("submit", "Enter", "Submit selected comments to the agent"),
        ]);
    }
    actions.push(HelpAction::new("help", "?", "Help"));

    actions
}

/// Returns list-mode actions shared by all tabs.
fn list_base_actions() -> Vec<HelpAction> {
    Vec::from(LIST_BASE_ACTIONS)
}

/// Returns the view-mode shortcut entry for merging the session's pull
/// request on its forge.
fn review_request_merge_help_action(action: ReviewRequestMergeAction) -> HelpAction {
    match action {
        ReviewRequestMergeAction::Merge => HelpAction::new(
            "merge PR",
            "m",
            "Merge pull request, or add it to its merge queue",
        ),
        ReviewRequestMergeAction::BypassMerge => HelpAction::new(
            "merge PR (bypass)",
            "m",
            "Merge pull request by bypassing branch protections",
        ),
    }
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

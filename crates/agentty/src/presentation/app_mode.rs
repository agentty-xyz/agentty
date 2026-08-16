use ag_forge::ReviewCommentSnapshot;

use super::help_action::{
    self, HelpAction, ViewActionAvailability, ViewHelpState, ViewSessionState,
};
use super::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    PublishBranchAction, Session, SessionId, Status, can_reply_to_session_in_stack,
};

/// Side of a unified diff that owns one selected changed line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineSide {
    /// Added line in the post-change file.
    New,
    /// Deleted line in the pre-change file.
    Old,
}

impl DiffLineSide {
    /// Returns the stable agent-facing label for this side.
    pub(crate) fn prompt_label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Old => "old",
        }
    }
}

/// Repository location and source text for one selected changed line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLineCommentAnchor {
    /// Changed line text without its diff marker.
    pub(crate) content: String,
    /// One-based line number on the owning diff side.
    pub(crate) line: u32,
    /// Repository-relative changed file path.
    pub(crate) path: String,
    /// Pre-change or post-change side that owns `line`.
    pub(crate) side: DiffLineSide,
}

impl DiffLineCommentAnchor {
    /// Builds one compact next-turn prompt line, retaining deleted source text.
    pub(crate) fn prompt_line(&self, comment: &str) -> String {
        match self.side {
            DiffLineSide::New => format!(
                "- {}:{} [{}]: {}",
                self.path,
                self.line,
                self.side.prompt_label(),
                comment.trim(),
            ),
            DiffLineSide::Old => format!(
                "- {}:{} [{}, source={:?}]: {}",
                self.path,
                self.line,
                self.side.prompt_label(),
                self.content,
                comment.trim(),
            ),
        }
    }
}

/// One editable comment attached to a changed diff line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLineComment {
    /// Changed source line that owns this comment.
    pub(crate) anchor: DiffLineCommentAnchor,
    /// User-authored inline comment text.
    pub(crate) input: InputState,
}

/// Inline comments accumulated while the unified diff remains open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiffLineComments {
    /// Index of the comment currently receiving text input.
    pub(crate) editing_index: Option<usize>,
    /// Comments retained in the order in which their lines were selected.
    pub(crate) comments: Vec<DiffLineComment>,
}

impl DiffLineComments {
    /// Starts editing the existing comment for `anchor` or inserts a new one.
    pub(crate) fn start_editing(&mut self, anchor: DiffLineCommentAnchor) -> usize {
        let editing_index = self
            .comments
            .iter()
            .position(|comment| comment.anchor == anchor)
            .unwrap_or_else(|| {
                self.comments.push(DiffLineComment {
                    anchor,
                    input: InputState::default(),
                });

                self.comments.len().saturating_sub(1)
            });
        self.editing_index = Some(editing_index);

        editing_index
    }

    /// Returns the input currently receiving inline comment keystrokes.
    pub(crate) fn editing_input_mut(&mut self) -> Option<&mut InputState> {
        self.editing_index
            .and_then(|editing_index| self.comments.get_mut(editing_index))
            .map(|comment| &mut comment.input)
    }

    /// Finishes editing and removes a comment whose text is blank.
    pub(crate) fn finish_editing(&mut self) {
        let Some(editing_index) = self.editing_index.take() else {
            return;
        };
        if self
            .comments
            .get(editing_index)
            .is_some_and(|comment| comment.input.text().trim().is_empty())
        {
            self.comments.remove(editing_index);
        }
    }

    /// Returns whether an inline comment currently owns keyboard focus.
    pub(crate) fn is_editing(&self) -> bool {
        self.editing_index.is_some()
    }

    /// Builds one next-turn prompt containing every completed comment.
    pub(crate) fn prompt_text(&self) -> String {
        let comments = self
            .comments
            .iter()
            .filter(|comment| !comment.input.text().trim().is_empty())
            .map(|comment| comment.anchor.prompt_line(comment.input.text()))
            .collect::<Vec<_>>()
            .join("\n");
        if comments.is_empty() {
            return comments;
        }

        format!("Line comments:\n{comments}")
    }
}

/// User-selected handling for one actionable forge review thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewCommentAction {
    /// Ask the session agent to implement the reviewer's requested change.
    Address,
    /// Ask the session agent to rebut the reviewer's requested change.
    Deny,
}

impl ReviewCommentAction {
    /// Returns the prompt label used to communicate the selected handling.
    pub(crate) fn prompt_label(self) -> &'static str {
        match self {
            Self::Address => "Address",
            Self::Deny => "Deny",
        }
    }
}

/// One actionable forge thread selected for batched agent handling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCommentActionSelection {
    /// Handling requested for the selected thread.
    pub(crate) action: ReviewCommentAction,
    /// Forge-native thread identifier.
    pub(crate) thread_id: String,
}

/// Review-comment data attached to the unified diff workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffReviewComments {
    /// Actionable threads marked for batched address or deny handling.
    pub comment_actions: Vec<ReviewCommentActionSelection>,
    /// User-facing failure returned while loading review comments.
    pub comment_error: Option<String>,
    /// Loaded review-request comment snapshot.
    pub comment_snapshot: Option<ReviewCommentSnapshot>,
    /// Whether the linked review request's comments are loading.
    pub is_loading_comments: bool,
    /// Request generation used to reject stale background completions.
    pub request_id: u64,
    /// Selected general comment or inline review thread.
    pub selected_comment_index: usize,
    /// Sidebar section currently controlling the unified diff workspace.
    pub sidebar_focus: DiffSidebarFocus,
}

impl DiffReviewComments {
    /// Creates the initial loading state for one linked review request.
    pub fn loading(request_id: u64) -> Self {
        Self {
            comment_actions: Vec::new(),
            comment_error: None,
            comment_snapshot: None,
            is_loading_comments: true,
            request_id,
            selected_comment_index: 0,
            sidebar_focus: DiffSidebarFocus::Files,
        }
    }
}

/// Sidebar section currently controlling the unified diff workspace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffSidebarFocus {
    /// Changed-file explorer navigation and diff or preview content.
    #[default]
    Files,
    /// Linked forge review-comment navigation and detail content.
    Comments,
}

/// Keyboard focus within the changed-files diff workspace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffFocus {
    /// The changed-file tree receives navigation input.
    #[default]
    Files,
    /// The right-hand diff panel receives line navigation input.
    Content,
}

/// Semantic intent for a `Confirmation` overlay interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationIntent {
    /// Confirms quitting the application.
    Quit,
    /// Confirms canceling a selected review session.
    CancelSession,
    /// Confirms creating a continuation draft from one terminal session.
    ContinueSession,
    /// Confirms forking a root review-ready session into a new session.
    ForkSession,
    /// Confirms queueing merge for the active view session.
    MergeSession,
    /// Confirms regenerating the focused review for the active view session.
    RegenerateReview,
    /// Confirms permanently detaching a coordinator-owned worker.
    DetachManagedSession,
    /// Acknowledges that a managed worker worktree opens with normal write
    /// access.
    OpenManagedWorktree,
    /// Chooses between local merges and forge review requests for a campaign.
    ChooseIntegrationApproach,
}

/// Stored view-mode values used to restore session view after session-scoped
/// confirmations and overlays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationViewMode {
    /// Scroll position to restore in session view.
    pub scroll_offset: Option<u16>,
    /// Session to reopen when the overlay closes.
    pub session_id: SessionId,
}

impl ConfirmationViewMode {
    /// Restores this snapshot as `AppMode::View`.
    #[must_use]
    pub fn into_view_mode(self) -> AppMode {
        AppMode::View {
            session_id: self.session_id,
            scroll_offset: self.scroll_offset,
        }
    }
}

/// Cached scroll bounds for the current diff selection and content area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffScrollCache {
    /// Diff content rectangle used to compute the cached bound.
    pub content_area: ViewportRect,
    /// File-tree selection for which the bound was computed.
    pub file_explorer_selected_index: usize,
    /// Largest valid vertical scroll offset.
    pub max_scroll_offset: u16,
}

/// Rendered markdown preview state for the active diff-tree selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffPreview {
    /// Preview mode is disabled while retaining the last request generation.
    Off {
        /// Last request generation, retained to invalidate older completions.
        request_id: u64,
    },
    /// Preview mode is enabled, but the current tree item is not markdown.
    Unsupported {
        /// Request generation that invalidated the prior selection load.
        request_id: u64,
    },
    /// Markdown content is loading from the session worktree.
    Loading {
        /// Repository-relative markdown path being loaded.
        path: String,
        /// Generation assigned to this background read.
        request_id: u64,
    },
    /// Renderable post-change markdown content is ready.
    Ready {
        /// Complete post-change markdown text.
        content: String,
        /// Repository-relative markdown path represented by `content`.
        path: String,
        /// Generation completed by this content.
        request_id: u64,
    },
    /// The selected markdown file cannot be previewed.
    Unavailable {
        /// Repository-relative markdown path that could not be previewed.
        path: String,
        /// Classified user-facing reason the preview is unavailable.
        reason: DiffPreviewUnavailableReason,
        /// Generation completed by this unavailable result.
        request_id: u64,
    },
}

impl DiffPreview {
    /// Returns whether preview remains enabled across tree selection changes.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Off { .. })
    }

    /// Returns the request generation retained by this preview state.
    #[must_use]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Off { request_id }
            | Self::Unsupported { request_id }
            | Self::Loading { request_id, .. }
            | Self::Ready { request_id, .. }
            | Self::Unavailable { request_id, .. } => *request_id,
        }
    }

    /// Returns the repository-relative path represented by an active load or
    /// result.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Loading { path, .. }
            | Self::Ready { path, .. }
            | Self::Unavailable { path, .. } => Some(path),
            Self::Off { .. } | Self::Unsupported { .. } => None,
        }
    }

    /// Returns a fresh non-zero request generation for a new selection load.
    #[must_use]
    pub fn next_request_id(&self) -> u64 {
        let next_request_id = self.request_id().wrapping_add(1);

        next_request_id.max(1)
    }

    /// Disables preview while invalidating any outstanding request.
    #[must_use]
    pub fn disabled(&self) -> Self {
        Self::Off {
            request_id: self.next_request_id(),
        }
    }
}

impl Default for DiffPreview {
    fn default() -> Self {
        Self::Off { request_id: 0 }
    }
}

/// User-facing reason a selected markdown file has no rendered preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffPreviewUnavailableReason {
    /// The selected file was deleted from the post-change worktree.
    Deleted,
    /// The selected file is not valid UTF-8 text.
    Binary,
    /// The selected file exceeds the bounded preview read size.
    TooLarge,
    /// The worktree read failed for another reason.
    LoadFailed(String),
}

/// Frontend-neutral rectangular viewport coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewportRect {
    /// Rectangle height in terminal cells.
    pub height: u16,
    /// Rectangle width in terminal cells.
    pub width: u16,
    /// Horizontal origin in terminal cells.
    pub x: u16,
    /// Vertical origin in terminal cells.
    pub y: u16,
}

/// Captured question-mode state for restoring after diff preview.
///
/// When the user opens diff preview from question mode (`d` key while chat
/// is focused), the full question state is snapshotted here so it can be
/// restored when leaving the diff view.
pub struct QuestionModeSnapshot {
    /// Restorable file and directory mention dropdown state.
    pub at_mention_state: Option<PromptAtMentionState>,
    /// Active question index.
    pub current_index: usize,
    /// Editable response input and cursor state.
    pub input: InputState,
    /// Ordered clarification questions.
    pub questions: Vec<QuestionItem>,
    /// Collected responses aligned with `questions`.
    pub responses: Vec<String>,
    /// Transcript scroll position to restore.
    pub scroll_offset: Option<u16>,
    /// Highlighted predefined option, when any.
    pub selected_option_index: Option<usize>,
    /// Session receiving the clarification response.
    pub session_id: SessionId,
}

impl QuestionModeSnapshot {
    /// Restores this snapshot as `AppMode::Question` with `Input` focus.
    #[must_use]
    pub fn into_question_mode(self) -> AppMode {
        AppMode::Question {
            at_mention_state: self.at_mention_state,
            current_index: self.current_index,
            focus: ChatFocus::Input,
            input: self.input,
            questions: self.questions,
            responses: self.responses,
            scroll_offset: self.scroll_offset,
            selected_option_index: self.selected_option_index,
            session_id: self.session_id,
        }
    }
}

/// Captured prompt-composer state for restoring after diff preview.
///
/// When the user opens diff preview from prompt mode (`d` key while the chat
/// transcript is focused), the composer state is snapshotted here so it can be
/// restored when leaving the diff view.
#[derive(Clone)]
pub struct PromptModeSnapshot {
    /// Restorable file and directory mention dropdown state.
    pub at_mention_state: Option<PromptAtMentionState>,
    /// Ordered local image attachments and their prompt placeholders.
    pub attachment_state: PromptAttachmentState,
    /// Prompt-history navigation state, including any saved draft.
    pub history_state: PromptHistoryState,
    /// Editable prompt input and cursor state.
    pub input: InputState,
    /// Transcript scroll position to restore.
    pub scroll_offset: Option<u16>,
    /// Session receiving the restored prompt.
    pub session_id: SessionId,
    /// Slash-command selection state for the current input.
    pub slash_state: PromptSlashState,
}

impl PromptModeSnapshot {
    /// Restores this snapshot as `AppMode::Prompt` with `Input` focus.
    #[must_use]
    pub fn into_prompt_mode(self) -> AppMode {
        AppMode::Prompt {
            at_mention_state: self.at_mention_state,
            attachment_state: self.attachment_state,
            focus: ChatFocus::Input,
            history_state: self.history_state,
            slash_state: self.slash_state,
            session_id: self.session_id,
            input: self.input,
            scroll_offset: self.scroll_offset,
        }
    }
}

/// Originating mode restored when leaving a diff preview.
///
/// A diff preview replaces the whole page, so the composer or question state it
/// was opened from is captured here and restored on exit. `None` on the diff
/// mode restores session view instead.
pub enum DiffRestoreTarget {
    /// Restore the prompt composer opened from prompt chat focus.
    Prompt(PromptModeSnapshot),
    /// Restore the question flow opened from question chat focus.
    Question(QuestionModeSnapshot),
}

impl DiffRestoreTarget {
    /// Reconstructs the originating `AppMode` for this restore target.
    #[must_use]
    pub fn into_mode(self) -> AppMode {
        match self {
            DiffRestoreTarget::Prompt(snapshot) => snapshot.into_prompt_mode(),
            DiffRestoreTarget::Question(snapshot) => snapshot.into_question_mode(),
        }
    }
}

/// Returns whether the visible diff may collect and submit inline comments.
pub(crate) fn allows_diff_line_comment_reply(
    session: &Session,
    sessions: &[Session],
    restore: Option<&DiffRestoreTarget>,
) -> bool {
    !matches!(restore, Some(DiffRestoreTarget::Question(_)))
        && session.status.allows_chat_composer()
        && session.accepts_user_turns()
        && (session.status == Status::Draft
            || can_reply_to_session_in_stack(sessions, session.id.as_str()))
}

/// Tracks which panel has input focus on the session chat page.
///
/// Both the prompt composer and the question panel share this focus model:
/// `Tab` moves focus to the transcript for scrolling and back to the bottom
/// input panel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChatFocus {
    /// Bottom input panel is focused for typing, option navigation, or
    /// submission.
    #[default]
    Input,
    /// Chat output area is focused for scrolling.
    Chat,
}

/// Represents the active UI mode for the application.
pub enum AppMode {
    /// Displays the active top-level list tab.
    List,
    /// Displays the session creation selector above the sessions list.
    SessionCreation {
        /// Highlighted session creation option.
        selected_option_index: usize,
    },
    /// Displays an advisory before opening the session creation selector.
    PreCommitHookWarning {
        /// Full warning text, installation commands, and future-enforcement
        /// guidance.
        message: String,
    },
    /// Displays the MRU-ordered project switcher popup above the sessions
    /// list.
    ProjectSwitcher {
        /// Highlighted project row in most-recently-opened order.
        selected_option_index: usize,
    },
    /// Displays a generic binary-choice confirmation overlay.
    Confirmation {
        /// Semantic action represented by the two visible choices.
        confirmation_intent: ConfirmationIntent,
        /// Body text explaining the decision.
        confirmation_message: String,
        /// Short title for the confirmation popup.
        confirmation_title: String,
        /// View state to restore when dismissing a session-scoped
        /// confirmation.
        restore_view: Option<ConfirmationViewMode>,
        /// Session affected by the action, when session-scoped.
        session_id: Option<SessionId>,
        /// Highlighted first or second option index.
        selected_confirmation_index: usize,
    },
    /// Informational popup displayed above the list for sync outcomes,
    /// including success and blocked/failed states, and for other list-level
    /// action failures such as a failed project switch.
    SyncBlockedPopup {
        /// Project name the reported action applies to, when the action was
        /// scoped to one project.
        project_name: Option<String>,
        /// Repository default branch used as sync target. Stays `None` for
        /// actions that have no branch target, such as a project switch.
        default_branch: Option<String>,
        /// Whether the reported action is still running in the background.
        is_loading: bool,
        /// Body text describing the current action state or final outcome.
        message: String,
        /// Popup title describing the reported action.
        title: String,
    },
    /// Informational popup rendered above session view for review-request
    /// workflows.
    ViewInfoPopup {
        /// Whether the background review-request workflow is still running.
        is_loading: bool,
        /// Spinner label rendered while the popup remains in the loading
        /// state.
        loading_label: String,
        /// Body text describing the current review-request outcome.
        message: String,
        /// View state restored after the popup is dismissed.
        restore_view: ConfirmationViewMode,
        /// Popup title describing the current review-request phase.
        title: String,
    },
    /// Launch-configuration selector overlay opened from session view when
    /// multiple entries are configured.
    LaunchConfigurationSelector {
        /// Available launch configurations in display/selection order.
        commands: Vec<String>,
        /// View state restored after launch-configuration selection or cancel.
        restore_view: ConfirmationViewMode,
        /// Highlighted launch-configuration index in `commands`.
        selected_command_index: usize,
    },
    /// Session-view popup that collects an optional remote branch name before
    /// publishing or refreshing the current forge review request.
    PublishBranchInput {
        /// Default remote branch name used when users leave the field blank.
        default_branch_name: String,
        /// Editable remote branch name. An empty value keeps the default push
        /// target for the session branch before review-request publication.
        input: InputState,
        /// Existing upstream reference, when the session branch already tracks
        /// one remote branch and the input must stay locked.
        locked_upstream_ref: Option<String>,
        /// Publish action that will run when users confirm the popup.
        publish_branch_action: PublishBranchAction,
        /// View state restored after publish or cancel.
        restore_view: ConfirmationViewMode,
    },
    /// Session chat composer for the first prompt or a follow-up reply.
    Prompt {
        /// Active `@`-mention dropdown state for file and directory lookup.
        at_mention_state: Option<PromptAtMentionState>,
        /// Ordered local image attachments referenced by inline placeholders in
        /// `input`.
        attachment_state: PromptAttachmentState,
        /// Panel that currently receives key input: the composer or the chat
        /// transcript above it.
        focus: ChatFocus,
        /// Prompt-history navigation state for `Up`/`Down`.
        history_state: PromptHistoryState,
        /// Slash-command selection state for the current prompt input.
        slash_state: PromptSlashState,
        /// Session whose prompt composer is currently active.
        session_id: SessionId,
        /// Editable prompt text, including inline attachment placeholders.
        input: InputState,
        /// Scroll position applied to the session transcript above the
        /// composer.
        scroll_offset: Option<u16>,
    },
    /// Displays one session transcript without an active input panel.
    View {
        /// Session whose transcript is visible.
        session_id: SessionId,
        /// Scroll position applied to the transcript.
        scroll_offset: Option<u16>,
    },
    /// Displays an immediately responsive loading page while the full session
    /// diff is computed outside the foreground event loop.
    DiffLoading {
        /// Scroll position restored when loading ends without opening diff.
        fallback_view_scroll_offset: Option<u16>,
        /// Request generation used to ignore a completion after cancellation.
        request_id: u64,
        /// Captured composer or question state restored when loading is
        /// canceled or produces no changes.
        restore: Option<Box<DiffRestoreTarget>>,
        /// Session whose diff is loading.
        session_id: SessionId,
        /// Sidebar section to focus when the diff finishes loading.
        sidebar_focus: DiffSidebarFocus,
    },
    /// Focused diff view with file-tree navigation and independent scrolling.
    Diff {
        /// Raw git diff rendered in the right-hand panel.
        diff: String,
        /// Selected file or folder in the left explorer tree.
        file_explorer_selected_index: usize,
        /// Panel currently receiving changed-file navigation input.
        focus: DiffFocus,
        /// Inline changed-line comments accumulated for the next turn.
        line_comments: DiffLineComments,
        /// Sticky rendered-markdown preview state for the selected file.
        preview: DiffPreview,
        /// Optional linked review-request comments rendered below the files.
        review_comments: Option<DiffReviewComments>,
        /// Captured composer or question state restored when leaving diff, if
        /// the diff was opened from an editing page. `None` restores to `View`
        /// mode. Boxed to keep the `Diff` variant small.
        restore: Option<Box<DiffRestoreTarget>>,
        /// Cached max scroll bound for the current content-area and selection.
        scroll_cache: Option<DiffScrollCache>,
        /// Vertical offset inside the rendered right panel.
        scroll_offset: u16,
        /// Addition or deletion selected in the right-hand diff panel.
        selected_diff_line_index: usize,
        /// Session whose diff is currently visible.
        session_id: SessionId,
    },

    /// Interactive clarification flow that asks agent questions one-by-one.
    Question {
        /// File/directory mention dropdown state for the free-text input.
        at_mention_state: Option<PromptAtMentionState>,
        /// Session receiving the follow-up clarification reply.
        session_id: SessionId,
        /// Ordered clarification prompts emitted by the model.
        questions: Vec<QuestionItem>,
        /// Collected user responses aligned to `questions`.
        responses: Vec<String>,
        /// Active question index inside `questions`.
        current_index: usize,
        /// Which panel currently owns keyboard focus.
        focus: ChatFocus,
        /// Editable response input for the active question.
        input: InputState,
        /// Scroll position applied to the session transcript above the
        /// question panel.
        scroll_offset: Option<u16>,
        /// Highlighted option index when the current question has predefined
        /// options. `None` means free-text input is active.
        selected_option_index: Option<usize>,
    },

    /// Displays context-sensitive keybindings above the originating page.
    Help {
        /// Originating page state used for help content and restoration.
        context: HelpContext,
        /// Vertical help-content scroll offset.
        scroll_offset: u16,
    },
}

/// Captures which page opened the help overlay so it can be restored on close.
pub enum HelpContext {
    /// Generic list-mode help context with precomputed keybindings.
    List {
        /// Keybinding entries for the active list tab.
        keybindings: Vec<HelpAction>,
    },
    /// Session-view help context and action availability.
    View {
        /// Whether the session may be forked.
        can_fork_session: bool,
        /// Whether the session branch may enter the merge queue.
        can_merge_session_branch: bool,
        /// Whether any session-branch mutation may begin.
        can_mutate_session_branch: bool,
        /// Whether the session worktree may be opened externally.
        can_open_worktree: bool,
        /// Whether the session branch may be rebased.
        can_rebase_session_branch: bool,
        /// Whether the session has a diff available to inspect.
        can_show_diff: bool,
        /// Whether a follow-up agent turn may be submitted.
        can_reply_to_session: bool,
        /// Whether a staged draft session may begin its first turn.
        can_start_staged_session: bool,
        /// Whether linked forge review comments may be opened.
        can_view_review_comments: bool,
        /// Pull-request publication action currently available.
        publish_pull_request_action: Option<PublishBranchAction>,
        /// Session whose view opened help.
        session_id: SessionId,
        /// Session lifecycle projection used to derive help actions.
        session_state: ViewSessionState,
        /// Transcript scroll position to restore.
        scroll_offset: Option<u16>,
    },
    /// Diff-view help context and restorable diff state.
    Diff {
        /// Whether this session may collect inline comments for a reply.
        can_comment: bool,
        /// Raw git diff to restore after help closes.
        diff: String,
        /// Selected file-tree row to restore.
        file_explorer_selected_index: usize,
        /// Panel that held keyboard focus before help opened.
        focus: DiffFocus,
        /// Inline changed-line comments accumulated before help opened.
        line_comments: DiffLineComments,
        /// Rendered-markdown preview state to restore.
        preview: DiffPreview,
        /// Optional linked review-request comments to restore.
        review_comments: Option<Box<DiffReviewComments>>,
        /// Preserved diff restore target so the help→diff→exit path can still
        /// return to the originating page when the diff was opened from there.
        /// Boxed to keep the `Diff` variant small.
        restore: Option<Box<DiffRestoreTarget>>,
        /// Session whose diff is visible.
        session_id: SessionId,
        /// Diff-panel scroll position to restore.
        scroll_offset: u16,
        /// Addition or deletion selected before help opened.
        selected_diff_line_index: usize,
    },
}

impl HelpContext {
    /// Returns projected keybinding entries for the originating page.
    pub fn keybindings(&self) -> Vec<HelpAction> {
        match self {
            HelpContext::View {
                can_fork_session,
                can_merge_session_branch,
                can_mutate_session_branch,
                can_open_worktree,
                can_rebase_session_branch,
                can_reply_to_session,
                can_show_diff,
                can_start_staged_session,
                can_view_review_comments,
                publish_pull_request_action,
                session_state,
                ..
            } => help_action::view_actions_with_review_comments(
                ViewHelpState {
                    can_fork_session: ViewActionAvailability::from_bool(*can_fork_session),
                    can_merge_session_branch: ViewActionAvailability::from_bool(
                        *can_merge_session_branch,
                    ),
                    can_mutate_session_branch: ViewActionAvailability::from_bool(
                        *can_mutate_session_branch,
                    ),
                    can_open_worktree: ViewActionAvailability::from_bool(*can_open_worktree),
                    can_rebase_session_branch: ViewActionAvailability::from_bool(
                        *can_rebase_session_branch,
                    ),
                    can_show_diff: ViewActionAvailability::from_bool(*can_show_diff),
                    reply_to_session: ViewActionAvailability::from_bool(*can_reply_to_session),
                    can_start_staged_session: ViewActionAvailability::from_bool(
                        *can_start_staged_session,
                    ),
                    publish_pull_request_action: *publish_pull_request_action,
                    session_state: *session_state,
                },
                *can_view_review_comments,
            ),
            HelpContext::List { keybindings } => keybindings.clone(),
            HelpContext::Diff { can_comment, .. } => help_action::diff_actions(*can_comment),
        }
    }

    /// Reconstructs the `AppMode` that was active before help was opened.
    pub fn restore_mode(self) -> AppMode {
        match self {
            HelpContext::List { .. } => AppMode::List,
            HelpContext::View {
                publish_pull_request_action: _,
                session_id,
                scroll_offset,
                ..
            } => AppMode::View {
                session_id,
                scroll_offset,
            },
            HelpContext::Diff {
                can_comment: _,
                diff,
                file_explorer_selected_index,
                focus,
                line_comments,
                preview,
                review_comments,
                restore,
                selected_diff_line_index,
                session_id,
                scroll_offset,
            } => AppMode::Diff {
                diff,
                file_explorer_selected_index,
                focus,
                line_comments,
                preview,
                review_comments: review_comments.map(|review_comments| *review_comments),
                restore,
                scroll_cache: None,
                selected_diff_line_index,
                session_id,
                scroll_offset,
            },
        }
    }

    /// Display title for the help overlay header.
    pub fn title(&self) -> &'static str {
        "Keybindings"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::PublishBranchAction;

    #[test]
    fn test_diff_line_comments_edit_and_build_compact_prompt() {
        // Arrange
        let anchor = DiffLineCommentAnchor {
            content: "println!(\"review\");".to_string(),
            line: 12,
            path: "src/main.rs".to_string(),
            side: DiffLineSide::New,
        };
        let mut line_comments = DiffLineComments::default();

        // Act
        line_comments.start_editing(anchor.clone());
        line_comments
            .editing_input_mut()
            .expect("new comment should be editable")
            .insert_text("Please explain this change.");
        line_comments.finish_editing();
        let prompt = line_comments.prompt_text();

        // Assert
        assert_eq!(
            prompt,
            "Line comments:\n- src/main.rs:12 [new]: Please explain this change."
        );
        assert!(!line_comments.is_editing());

        // Act — selecting the same line edits the existing comment.
        let editing_index = line_comments.start_editing(anchor);

        // Assert
        assert_eq!(editing_index, 0);
        assert_eq!(line_comments.comments.len(), 1);
    }

    #[test]
    fn test_deleted_diff_line_prompt_includes_captured_source() {
        // Arrange
        let anchor = DiffLineCommentAnchor {
            content: "let message = \"old\";".to_string(),
            line: 7,
            path: "src/old.rs".to_string(),
            side: DiffLineSide::Old,
        };

        // Act
        let prompt_line = anchor.prompt_line("Keep this behavior.");

        // Assert
        assert_eq!(
            prompt_line,
            "- src/old.rs:7 [old, source=\"let message = \\\"old\\\";\"]: Keep this behavior."
        );
    }

    #[test]
    fn test_diff_line_comments_remove_blank_editor() {
        // Arrange
        let mut line_comments = DiffLineComments::default();
        line_comments.start_editing(DiffLineCommentAnchor {
            content: "removed".to_string(),
            line: 3,
            path: "src/lib.rs".to_string(),
            side: DiffLineSide::Old,
        });

        // Act
        line_comments.finish_editing();
        line_comments.finish_editing();

        // Assert
        assert!(line_comments.comments.is_empty());
        assert!(line_comments.editing_input_mut().is_none());
        assert!(line_comments.prompt_text().is_empty());
    }

    #[test]
    fn test_confirmation_view_mode_into_view_mode_restores_view_identity() {
        // Arrange
        let confirmation_view_mode = ConfirmationViewMode {
            scroll_offset: Some(7),
            session_id: "session-id".into(),
        };

        // Act
        let mode = confirmation_view_mode.into_view_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(7),
            } if session_id == "session-id"
        ));
    }

    #[test]
    fn test_help_context_view_keybindings_for_in_progress_show_sync_and_hide_edit_actions() {
        // Arrange
        let context = HelpContext::View {
            can_fork_session: true,
            can_merge_session_branch: true,
            can_mutate_session_branch: true,
            can_open_worktree: true,
            can_rebase_session_branch: true,
            can_show_diff: true,
            can_reply_to_session: true,
            can_start_staged_session: false,
            can_view_review_comments: false,
            publish_pull_request_action: None,
            session_id: "session-id".into(),
            session_state: ViewSessionState::InProgress,
            scroll_offset: Some(2),
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|binding| binding.key == "q"));
        assert!(bindings.iter().any(|binding| binding.key == "j/k"));
        assert!(bindings.iter().any(|binding| binding.key == "?"));
        assert!(bindings.iter().any(|binding| binding.key == "Ctrl+c"));
        assert!(bindings.iter().any(|binding| binding.key == "r"));
        assert!(!bindings.iter().any(|binding| binding.key == "Enter"));
        assert!(!bindings.iter().any(|binding| binding.key == "d"));
        assert!(!bindings.iter().any(|binding| binding.key == "m"));
        assert!(!bindings.iter().any(|binding| binding.key == "S-Tab"));
    }

    #[test]
    fn test_help_context_restore_mode_ignores_help_only_view_fields() {
        // Arrange
        let context = HelpContext::View {
            can_fork_session: true,
            can_merge_session_branch: true,
            can_mutate_session_branch: true,
            can_open_worktree: true,
            can_rebase_session_branch: true,
            can_show_diff: true,
            can_reply_to_session: true,
            can_start_staged_session: false,
            can_view_review_comments: false,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_id: "session-id".into(),
            session_state: ViewSessionState::InProgress,
            scroll_offset: Some(4),
        };

        // Act
        let mode = context.restore_mode();

        // Assert
        assert!(matches!(
            mode,
            AppMode::View {
                ref session_id,
                scroll_offset: Some(4),
                ..
            } if session_id == "session-id"
        ));
    }

    #[test]
    fn test_help_context_view_keybindings_include_publish_pull_request_action() {
        // Arrange
        let context = HelpContext::View {
            can_fork_session: true,
            can_merge_session_branch: true,
            can_mutate_session_branch: true,
            can_open_worktree: true,
            can_rebase_session_branch: true,
            can_show_diff: true,
            can_reply_to_session: true,
            can_start_staged_session: false,
            can_view_review_comments: false,
            publish_pull_request_action: Some(PublishBranchAction::PublishPullRequest),
            session_id: "session-id".into(),
            session_state: ViewSessionState::Interactive,
            scroll_offset: None,
        };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert!(bindings.iter().any(|binding| binding.key == "p"));
    }

    #[test]
    fn test_help_context_list_keybindings_return_stored_actions() {
        // Arrange
        let keybindings = vec![
            HelpAction::new("quit", "q", "Quit"),
            HelpAction::new("help", "?", "Help"),
        ];
        let context = HelpContext::List { keybindings };

        // Act
        let bindings = context.keybindings();

        // Assert
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| binding.key == "q"));
        assert!(bindings.iter().any(|binding| binding.key == "?"));
    }

    #[test]
    fn test_diff_preview_tracks_enabled_state_and_request_generation() {
        // Arrange
        let states = [
            DiffPreview::Off { request_id: 0 },
            DiffPreview::Unsupported { request_id: 1 },
            DiffPreview::Loading {
                path: "README.md".to_string(),
                request_id: 2,
            },
            DiffPreview::Ready {
                content: "# Ready".to_string(),
                path: "README.md".to_string(),
                request_id: 3,
            },
            DiffPreview::Unavailable {
                path: "README.md".to_string(),
                reason: DiffPreviewUnavailableReason::Deleted,
                request_id: 4,
            },
        ];

        // Act
        let enabled = states
            .iter()
            .map(DiffPreview::is_enabled)
            .collect::<Vec<_>>();
        let request_ids = states
            .iter()
            .map(DiffPreview::request_id)
            .collect::<Vec<_>>();
        let paths = states.iter().map(DiffPreview::path).collect::<Vec<_>>();
        let next_request_id = states[4].next_request_id();
        let disabled = states[3].disabled();

        // Assert
        assert_eq!(enabled, [false, true, true, true, true]);
        assert_eq!(request_ids, [0, 1, 2, 3, 4]);
        assert_eq!(
            paths,
            [
                None,
                None,
                Some("README.md"),
                Some("README.md"),
                Some("README.md")
            ]
        );
        assert_eq!(next_request_id, 5);
        assert_eq!(disabled, DiffPreview::Off { request_id: 4 });
    }
}

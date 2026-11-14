use std::io;

use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::prompt_intent::{
    PromptApplyOutcome, PromptCancellation, PromptImagePaste, PromptSessionMode, PromptSubmission,
    PromptWorkflowOutcome,
};
use crate::domain::agent::{AgentKind, AgentSelection, ReasoningLevel, ResponseStyle};
use crate::domain::composer::PromptAttachment;
use crate::domain::input::{InputCommand, InputEffect, InputState};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{Session, SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::TransientMessageSlot;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffRestoreTarget, DiffSidebarFocus, PromptModeSnapshot,
};
use crate::presentation::prompt::{
    PendingImagePaste, PromptAtMentionState, PromptAttachmentState, PromptGraphState,
    PromptSlashCommandSet, PromptSlashStage, PromptSlashState, PromptSuggestionSelection,
    apply_prompt_delete_range as apply_prompt_delete_range_components,
    current_line_delete_range as prompt_current_line_delete_range, drain_prompt_submission,
    insert_prompt_text, needs_space_after_image_placeholder, prompt_slash_option_count,
    resolve_prompt_slash_selection,
};
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::RenderCacheStore;
use crate::ui::input_layout::{move_input_cursor_down, move_input_cursor_up};

/// Read-only routing flags and session facts captured from `App` before one
/// prompt key is reduced.
///
/// The pure reducer reads only this value and the prompt mode, so key
/// handling never consults application services while it transforms state.
/// Draft sessions only stage prompts while they remain in `Status::Draft`.
/// After the first turn starts, follow-up submissions must route through the
/// normal reply path even though the session still records draft origin.
struct PromptContext {
    input_mode: PromptInputMode,
    /// Session permission mode cycled by `Shift+Tab` and preselected by
    /// `/mode`.
    permission_mode: PermissionMode,
    /// Effective session reasoning level preselected by `/reasoning`.
    reasoning_level: ReasoningLevel,
    /// Session response style preselected by `/style`.
    response_style: ResponseStyle,
    scroll_offset: Option<usize>,
    /// Agent kind of the list-selected session, which slash-menu navigation
    /// uses to count selectable rows.
    selected_session_agent_kind: AgentKind,
    /// Agent kind of the prompt session, which slash-menu submission uses to
    /// resolve the highlighted row.
    session_agent_kind: AgentKind,
    session_id: SessionId,
    session_index: usize,
    session_mode: PromptSessionMode,
    /// Harness model that needs a `ChatGPT` sign-in before a turn can run.
    sign_in_agent: Option<AgentSelection>,
    /// Slash commands offered for the prompt session, which depend on cached
    /// focused-review output and whether the session is busy.
    slash_command_set: PromptSlashCommandSet,
}

impl PromptContext {
    /// Returns whether the prompt is currently editing an active `@` mention.
    fn is_at_mention(&self) -> bool {
        self.input_mode == PromptInputMode::AtMention
    }

    /// Returns whether the prompt is currently editing a slash command.
    fn is_slash_command(&self) -> bool {
        self.input_mode == PromptInputMode::SlashCommand
    }
}

/// Active prompt input sub-mode used for specialized key routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptInputMode {
    /// Prompt text is editing an active file `@` mention.
    AtMention,
    /// Prompt text starts with a slash-command prefix.
    SlashCommand,
    /// Prompt text is normal user input.
    Text,
}

/// Work left after one pure prompt-key reduction.
enum PromptKeyStep {
    /// Applies app effects in order.
    Effects(Vec<PromptEffect>),
    /// Scrolls the focused transcript, which needs the rendered layout.
    ScrollTranscript,
}

/// App-facing work requested by a prompt reduction.
///
/// The reducer has already updated prompt state; the effect step dispatches
/// each value to its app workflow in order.
enum PromptEffect {
    /// Runs `/apply` and applies the composer outcome it requests.
    ApplyFocusedReview,
    /// Cancels the prompt workflow and navigates as its outcome requests.
    CancelPrompt,
    /// Removes attachment files the composer no longer owns.
    CleanupAttachments(Vec<PromptAttachment>),
    /// Loads `@`-mention entries for the reopened dropdown.
    LoadAtMentionEntries,
    /// Opens the `ChatGPT` sign-in popup above the captured composer.
    OpenChatGptSignIn {
        restore: PromptModeSnapshot,
        selected_agent: AgentSelection,
    },
    /// Starts loading the session diff, restoring `restore` on exit.
    OpenDiff { restore: Option<DiffRestoreTarget> },
    /// Persists one permission mode and reports failures in the transcript.
    PersistPermissionMode(PermissionMode),
    /// Lists harness models again for the opened `/model` stage.
    RefreshHarnessModels,
    /// Saves the captured composer for the next prompt reopen.
    SaveProgress(PromptModeSnapshot),
    /// Starts a background clipboard-image capture.
    StartImagePaste { attachment_number: usize },
    /// Submits one drained composer turn.
    SubmitPrompt(PromptTurnSubmission),
    /// Toggles the session speed mode, deferred while the session is busy.
    ToggleSpeedMode,
    /// Applies one slash-selected model, deferred while the session is busy.
    UpdateModel(AgentSelection),
    /// Persists one slash-selected reasoning level.
    UpdateReasoningLevel(ReasoningLevel),
    /// Persists one slash-selected response style.
    UpdateResponseStyle(ResponseStyle),
}

/// One drained composer turn plus the composer to restore when the workflow
/// keeps the prompt open.
struct PromptTurnSubmission {
    /// Archived attachment files cleaned up once the turn leaves the composer.
    archived_attachments: Vec<PromptAttachment>,
    /// Whether `/graph` armed the composer to request a graph proposal.
    is_graph_armed: bool,
    saved_attachment_state: PromptAttachmentState,
    saved_input: InputState,
    submission: PromptSubmission,
}

/// Handles key input while the app is in `AppMode::Prompt`.
///
/// `Tab` moves focus between the composer and the chat transcript above it,
/// unless the `@`-mention dropdown is open and claims the key for completion.
/// `Shift+Tab` cycles the session permission mode while the composer is
/// focused. While the transcript holds focus, scroll keys navigate it and the
/// composer text stays untouched. Pressing `q` from transcript focus returns to
/// the sessions list and saves the complete composer for the next reopen.
///
/// The key is reduced over prompt state by [`reduce_prompt_key`]; the
/// resulting effects are then dispatched to app workflows in order.
pub(crate) async fn handle_with_cache<B: Backend>(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let Some(prompt_context) = prompt_context(app) else {
        return Ok(EventResult::Continue);
    };
    let terminal_size = terminal.size().map_err(crate::runtime::backend_err)?;
    let input_width = terminal_size.width.saturating_sub(2);

    match reduce_prompt_key(&mut app.mode, &prompt_context, input_width, key) {
        PromptKeyStep::Effects(effects) => {
            apply_prompt_effects(app, &prompt_context, effects).await;
        }
        PromptKeyStep::ScrollTranscript => {
            let metrics = ChatScrollMetrics::new(
                app,
                render_cache_store,
                &prompt_context.session_id,
                prompt_context.session_index,
                Rect::new(0, 0, terminal_size.width, terminal_size.height),
            );
            chat_scroll::apply_scroll_key_to_mode(&mut app.mode, metrics, key);
        }
    }

    Ok(EventResult::Continue)
}

/// Inserts pasted content into the prompt input while normalizing mixed
/// line-endings to `\n`.
///
/// Pastes are dropped while the chat transcript holds focus so scrolling the
/// conversation never rewrites the typed draft.
pub(crate) async fn handle_paste(app: &mut App, pasted_text: &str) {
    let Some(unreachable_attachments) = apply_prompt_paste_to_mode(&mut app.mode, pasted_text)
    else {
        return;
    };

    app.cleanup_prompt_attachments(unreachable_attachments)
        .await;
    let Some(prompt_context) = prompt_context(app) else {
        return;
    };
    let mut effects = Vec::new();
    sync_prompt_at_mention_state(&mut app.mode, &mut effects);
    apply_prompt_effects(app, &prompt_context, effects).await;
}

/// Submits a normal text prompt assembled by another interactive mode.
///
/// The owning mode has already chosen normal turn submission, so a leading
/// slash remains user text instead of reopening slash-command routing.
pub(crate) async fn submit_current_text_prompt(app: &mut App) {
    let Some(mut prompt_context) = prompt_context(app) else {
        return;
    };
    prompt_context.input_mode = PromptInputMode::Text;

    let mut effects = Vec::new();
    reduce_prompt_submit(&mut app.mode, &prompt_context, &mut effects);
    apply_prompt_effects(app, &prompt_context, effects).await;
}

/// Returns true when the key event should paste one clipboard image into the
/// prompt composer.
///
/// Accepts both lowercase and shifted uppercase `V` because Linux terminals
/// commonly report `Ctrl+Shift+V` as `KeyCode::Char('V')` with `CONTROL` and
/// `SHIFT` modifiers.
pub(crate) fn is_prompt_image_paste_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v' | 'V'))
        && key
            .modifiers
            .intersects(event::KeyModifiers::ALT | event::KeyModifiers::CONTROL)
}

/// Starts a background clipboard-image capture for the active presentation
/// composer, which shows a loading indicator until the capture completes.
/// The cursor position is recorded so the placeholder lands there even if the
/// user keeps editing.
///
/// Repeated paste shortcuts are ignored while one capture is still running.
pub(crate) fn paste_image_into_active_prompt(app: &mut App, session_id: &SessionId) {
    let Some(attachment_number) = prompt_image_paste_number(&app.mode) else {
        return;
    };

    start_prompt_image_paste(app, session_id, attachment_number);
}

/// Returns the active prompt context for the currently edited session.
///
/// Falls back to the sessions list when the prompt session no longer exists.
fn prompt_context(app: &mut App) -> Option<PromptContext> {
    let AppMode::Prompt {
        at_mention_state,
        input,
        scroll_offset,
        session_id,
        slash_state,
        ..
    } = &app.mode
    else {
        return None;
    };
    let Some(session_index) = app.session_index_for_id(session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    let session = app.session_at(session_index);
    let session_mode = session.map_or(PromptSessionMode::Existing, prompt_session_mode);

    Some(PromptContext {
        input_mode: prompt_input_mode(at_mention_state.as_ref(), input, slash_state),
        permission_mode: session
            .map_or_else(PermissionMode::default, |session| session.permission_mode),
        reasoning_level: session.map_or(
            app.settings.default_smart_reasoning_level,
            Session::effective_reasoning_level,
        ),
        response_style: session
            .map_or_else(ResponseStyle::default, |session| session.response_style),
        scroll_offset: *scroll_offset,
        selected_session_agent_kind: app
            .selected_session()
            .map_or(AgentKind::Codex, |session| session.agent.kind()),
        session_agent_kind: session.map_or(AgentKind::Codex, |session| session.agent.kind()),
        session_id: session_id.clone(),
        session_index,
        session_mode,
        sign_in_agent: app.chatgpt_sign_in_required(session_index),
        slash_command_set: app.prompt_slash_command_set(session_id),
    })
}

/// Returns how prompt submission and cancellation route for `session`.
///
/// Staged drafts exist only on draft sessions, so a new regular session is
/// kept only while its workspace preparation is visible.
fn prompt_session_mode(session: &Session) -> PromptSessionMode {
    let is_new_session = session.status == Status::Draft;
    let is_preparing_workspace = session
        .transient_messages
        .get(TransientMessageSlot::WorkspacePreparation)
        .is_some();

    match (
        is_new_session,
        session.is_draft_session(),
        is_preparing_workspace,
    ) {
        (true, true, _) => PromptSessionMode::NewDraft,
        (true, false, true) => PromptSessionMode::NewRegular,
        (true, false, false) => PromptSessionMode::NewDeletable,
        (false, _, _) => PromptSessionMode::Existing,
    }
}

/// Derives the key-routing sub-mode from the current composer.
///
/// A leading `/` keeps slash-command routing even while the session is
/// `InProgress` or `Rebasing`; [`PromptContext::slash_command_set`] then
/// limits the menu to setting commands.
fn prompt_input_mode(
    at_mention_state: Option<&PromptAtMentionState>,
    input: &InputState,
    slash_state: &PromptSlashState,
) -> PromptInputMode {
    if is_active_at_mention(at_mention_state, input) {
        return PromptInputMode::AtMention;
    }

    if input.text().starts_with('/') && !slash_state.graph.is_armed() {
        return PromptInputMode::SlashCommand;
    }

    PromptInputMode::Text
}

fn is_active_at_mention(
    at_mention_state: Option<&PromptAtMentionState>,
    input: &InputState,
) -> bool {
    at_mention_state.is_some() && input.at_mention_query().is_some()
}

/// Dispatches reduced prompt effects to their app workflows in order.
async fn apply_prompt_effects(
    app: &mut App,
    prompt_context: &PromptContext,
    effects: Vec<PromptEffect>,
) {
    for effect in effects {
        apply_prompt_effect(app, prompt_context, effect).await;
    }
}

/// Dispatches one reduced prompt effect to its app workflow.
async fn apply_prompt_effect(app: &mut App, prompt_context: &PromptContext, effect: PromptEffect) {
    let session_id = &prompt_context.session_id;

    match effect {
        PromptEffect::ApplyFocusedReview => {
            let outcome = app
                .apply_focused_review(session_id, prompt_context.session_index)
                .await;
            let attachments = apply_prompt_apply_outcome(&mut app.mode, outcome);
            app.cleanup_prompt_attachments(attachments).await;
        }
        PromptEffect::CancelPrompt => {
            let outcome = app
                .cancel_prompt(PromptCancellation {
                    session_id: session_id.clone(),
                    session_mode: prompt_context.session_mode,
                })
                .await;
            apply_prompt_workflow_outcome(&mut app.mode, outcome, prompt_context.scroll_offset);
        }
        PromptEffect::CleanupAttachments(attachments) => {
            app.cleanup_prompt_attachments(attachments).await;
        }
        PromptEffect::LoadAtMentionEntries => {
            let lookup_root = app.at_mention_lookup_root(session_id);
            let event_tx = app.services.event_sender();

            at_mention::start_loading_entries(
                event_tx,
                lookup_root,
                session_id.clone(),
                &mut app.sessions,
            );
        }
        PromptEffect::OpenChatGptSignIn {
            restore,
            selected_agent,
        } => app.open_chatgpt_sign_in(selected_agent, restore),
        PromptEffect::OpenDiff { restore } => {
            app.start_diff_view_load(session_id, restore, DiffSidebarFocus::Files, false);
        }
        PromptEffect::PersistPermissionMode(permission_mode) => {
            persist_prompt_permission_mode(app, session_id, permission_mode).await;
        }
        PromptEffect::RefreshHarnessModels => app.refresh_harness_models(),
        PromptEffect::SaveProgress(snapshot) => app.save_prompt_progress(snapshot),
        PromptEffect::StartImagePaste { attachment_number } => {
            start_prompt_image_paste(app, session_id, attachment_number);
        }
        PromptEffect::SubmitPrompt(turn) => submit_prompt_turn(app, turn).await,
        PromptEffect::ToggleSpeedMode => app.toggle_prompt_session_speed_mode(session_id).await,
        PromptEffect::UpdateModel(selected_agent) => {
            app.update_prompt_session_model(session_id, selected_agent)
                .await;
        }
        PromptEffect::UpdateReasoningLevel(reasoning_level) => {
            app.update_prompt_session_reasoning_level(session_id, reasoning_level)
                .await;
        }
        PromptEffect::UpdateResponseStyle(response_style) => {
            app.update_prompt_session_response_style(session_id, response_style)
                .await;
        }
    }
}

/// Applies the composer and navigation changes requested by `/apply` and
/// returns composer attachments that require file cleanup.
fn apply_prompt_apply_outcome(
    mode: &mut AppMode,
    outcome: PromptApplyOutcome,
) -> Vec<PromptAttachment> {
    match outcome {
        PromptApplyOutcome::ClearComposer => clear_prompt_slash_input(mode),
        PromptApplyOutcome::KeepComposer => {
            update_prompt_slash_state(mode, PromptSlashState::reset);

            Vec::new()
        }
        PromptApplyOutcome::ShowSession { session_id } => {
            let attachments = take_prompt_attachment_cleanup(mode);
            *mode = AppMode::View {
                scroll_offset: None,
                session_id,
            };

            attachments
        }
    }
}

/// Clears the slash buffer and returns the composer attachments it owned for
/// file cleanup.
fn clear_prompt_slash_input(mode: &mut AppMode) -> Vec<PromptAttachment> {
    let attachments = take_prompt_attachment_cleanup(mode);

    if let AppMode::Prompt {
        input, slash_state, ..
    } = mode
    {
        input.take_text();
        slash_state.reset();
    }

    attachments
}

/// Removes all attachments owned by the active composer and resets that
/// presentation state before it leaves prompt mode.
fn take_prompt_attachment_cleanup(mode: &mut AppMode) -> Vec<PromptAttachment> {
    let AppMode::Prompt {
        attachment_state, ..
    } = mode
    else {
        return Vec::new();
    };

    let attachments = attachment_state
        .attachments
        .iter()
        .chain(&attachment_state.archived_attachments)
        .cloned()
        .collect();
    attachment_state.reset();

    attachments
}

/// Applies `update` to the prompt slash-command state.
///
/// Other modes are left unchanged.
fn update_prompt_slash_state(mode: &mut AppMode, update: impl FnOnce(&mut PromptSlashState)) {
    if let AppMode::Prompt { slash_state, .. } = mode {
        update(slash_state);
    }
}

/// Applies the navigation requested by one app-layer prompt workflow.
fn apply_prompt_workflow_outcome(
    mode: &mut AppMode,
    outcome: PromptWorkflowOutcome,
    scroll_offset: Option<usize>,
) {
    match outcome {
        PromptWorkflowOutcome::KeepPrompt => {}
        PromptWorkflowOutcome::ShowSession { session_id } => {
            *mode = AppMode::View {
                scroll_offset,
                session_id,
            };
        }
        PromptWorkflowOutcome::ShowSessionList => *mode = AppMode::List,
    }
}

/// Persists one selected permission mode and reports failures in the target
/// session transcript.
async fn persist_prompt_permission_mode(
    app: &mut App,
    session_id: &SessionId,
    permission_mode: PermissionMode,
) {
    if let Err(error) = app
        .update_prompt_session_permission_mode(session_id, permission_mode)
        .await
    {
        app.append_prompt_status_line(
            session_id,
            TranscriptNotice::Error,
            &format!("Failed to change mode; the session remains unchanged: {error}"),
        )
        .await;
    }
}

/// Starts one clipboard-image capture and anchors it at the prompt cursor.
fn start_prompt_image_paste(app: &mut App, session_id: &SessionId, attachment_number: usize) {
    let request_id = app.start_prompt_image_paste(PromptImagePaste {
        attachment_number,
        session_id: session_id.clone(),
    });

    mark_prompt_image_paste_pending(&mut app.mode, request_id);
}

/// Anchors one running clipboard-image capture at the prompt cursor.
///
/// Other modes are left unchanged.
fn mark_prompt_image_paste_pending(mode: &mut AppMode, request_id: u64) {
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = mode
    {
        attachment_state.pending_image_paste = Some(PendingImagePaste {
            anchor: Some(input.cursor),
            request_id,
        });
    }
}

/// Submits one drained turn and restores or releases its composer according
/// to the workflow outcome.
async fn submit_prompt_turn(app: &mut App, turn: PromptTurnSubmission) {
    let outcome = if turn.is_graph_armed {
        app.submit_graph_request(turn.submission).await
    } else {
        app.submit_prompt(turn.submission).await
    };

    if matches!(outcome, PromptWorkflowOutcome::KeepPrompt) {
        restore_prompt_composer(&mut app.mode, turn.saved_attachment_state, turn.saved_input);
    } else {
        app.cleanup_prompt_attachments(turn.archived_attachments)
            .await;
    }
    apply_prompt_workflow_outcome(&mut app.mode, outcome, None);
}

/// Restores the composer captured before a submission the workflow kept.
///
/// Other modes are left unchanged.
fn restore_prompt_composer(
    mode: &mut AppMode,
    saved_attachment_state: PromptAttachmentState,
    saved_input: InputState,
) {
    if let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = mode
    {
        *attachment_state = saved_attachment_state;
        *input = saved_input;
    }
}

/// Reduces one prompt key over `mode` without touching application state.
///
/// Returns the app effects the key requests, in order, or asks the caller to
/// scroll the focused transcript. `input_width` is the composer text width
/// used for vertical cursor movement.
fn reduce_prompt_key(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    input_width: u16,
    key: KeyEvent,
) -> PromptKeyStep {
    if !prompt_context.is_slash_command() {
        update_prompt_slash_state(mode, PromptSlashState::reset);
    }

    let mut effects = Vec::new();
    if prompt_context.is_at_mention() && reduce_at_mention_key(mode, key, &mut effects) {
        return PromptKeyStep::Effects(effects);
    }

    if is_plain_char_key(key, 'q') && prompt_chat_is_focused(mode) {
        exit_to_list_saving_progress(mode, &mut effects);

        return PromptKeyStep::Effects(effects);
    }

    if let Some(step) = reduce_chat_focus_key(mode, key) {
        return step;
    }

    reduce_editing_key(mode, prompt_context, input_width, key, &mut effects);

    PromptKeyStep::Effects(effects)
}

/// Handles keys when the at-mention dropdown is active.
///
/// Returns `true` if the key was consumed by at-mention logic.
fn reduce_at_mention_key(
    mode: &mut AppMode,
    key: KeyEvent,
    effects: &mut Vec<PromptEffect>,
) -> bool {
    match key.code {
        KeyCode::Esc => dismiss_at_mention(mode),
        KeyCode::Enter if !input_key::should_insert_newline(key) => {
            select_at_mention(mode, effects);
        }
        KeyCode::Tab => select_at_mention(mode, effects),
        KeyCode::Up => move_at_mention_selection_up(mode),
        KeyCode::Down => move_at_mention_selection_down(mode),
        _ => return false,
    }

    true
}

/// Clears the at-mention state.
fn dismiss_at_mention(mode: &mut AppMode) {
    if let AppMode::Prompt {
        at_mention_state, ..
    } = mode
    {
        at_mention::dismiss(at_mention_state);
    }
}

/// Selects the currently highlighted file and inserts it into the input.
fn select_at_mention(mode: &mut AppMode, effects: &mut Vec<PromptEffect>) {
    let replacement = match mode {
        AppMode::Prompt {
            at_mention_state: Some(state),
            input,
            ..
        } => at_mention::selected_replacement(input, state),
        _ => return,
    };

    if let Some(selection) = replacement {
        reduce_prompt_input_command(
            mode,
            InputCommand::ReplaceRange {
                start: selection.at_start,
                end: selection.at_end,
                text: selection.text,
            },
            effects,
        );
    }

    dismiss_at_mention(mode);
}

/// Moves the at-mention selection up.
fn move_at_mention_selection_up(mode: &mut AppMode) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = mode
    {
        at_mention::move_selection_up(state);
    }
}

/// Moves the at-mention selection down.
fn move_at_mention_selection_down(mode: &mut AppMode) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        input,
        ..
    } = mode
    {
        at_mention::move_selection_down(input, state);
    }
}

/// Reduces one shared input command while preserving prompt-specific
/// attachment, history, slash-command, and `@`-mention behavior.
fn reduce_prompt_input_command(
    mode: &mut AppMode,
    command: InputCommand,
    effects: &mut Vec<PromptEffect>,
) {
    let unreachable_attachments = apply_prompt_input_to_mode(mode, command);

    push_attachment_cleanup(effects, unreachable_attachments);
    sync_prompt_at_mention_state(mode, effects);
}

/// Reduces one editing command and returns attachments that require file
/// cleanup.
fn apply_prompt_input_to_mode(mode: &mut AppMode, command: InputCommand) -> Vec<PromptAttachment> {
    let delete_range = if let AppMode::Prompt { input, .. } = mode {
        prompt_command_delete_range(input, &command)
    } else {
        None
    };

    if let Some((start, end)) = delete_range {
        return apply_prompt_delete_to_mode(mode, start, end);
    }

    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = mode
    {
        attachment_state.remember_current_revision(input);
        let command = separate_text_from_image_placeholder(input, command);
        let edit_span = prompt_command_edit_span(input, &command);
        let effect = input.apply(command);
        if effect == InputEffect::TextChanged {
            // Deletions return early above, so `Undo` and `Redo` are the only
            // text-changing commands without an edit span.
            if let Some((old_start, old_end, new_end)) = edit_span {
                attachment_state.sync_after_edit(input, old_start, old_end, new_end);
            } else {
                attachment_state.sync_after_history_restore(input);
            }
            unreachable_attachments = attachment_state.prune_unreachable(input);
            history_state.reset_navigation();
            slash_state.reset();
        }
    }

    unreachable_attachments
}

/// Returns the prompt-aware range for shared deletion commands.
///
/// Boundary deletions return `None` without evaluating an out-of-range cursor
/// position.
fn prompt_command_delete_range(
    input: &InputState,
    command: &InputCommand,
) -> Option<(usize, usize)> {
    match command {
        InputCommand::DeleteBackward => {
            (input.cursor > 0).then(|| (input.cursor - 1, input.cursor))
        }
        InputCommand::DeleteCurrentLine => prompt_current_line_delete_range(input),
        InputCommand::DeleteForward => (input.cursor < input.text().chars().count())
            .then_some((input.cursor, input.cursor + 1)),
        InputCommand::DeleteToLineEnd => input.line_end_delete_range(),
        InputCommand::DeleteWordBackward => input.word_delete_range(),
        _ => None,
    }
}

/// Applies one prompt deletion range, expanding it to cover full image
/// placeholder tokens and removing orphaned attachments from prompt state.
fn apply_prompt_delete_to_mode(
    mode: &mut AppMode,
    start: usize,
    end: usize,
) -> Vec<PromptAttachment> {
    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = mode
    {
        apply_prompt_delete_range_components(
            attachment_state,
            history_state,
            input,
            slash_state,
            start,
            end,
        );
        unreachable_attachments = attachment_state.prune_unreachable(input);
    }

    unreachable_attachments
}

/// Prefixes a typed character with a space when it would otherwise run into
/// the `[Image #n]` placeholder ending at the cursor.
fn separate_text_from_image_placeholder(input: &InputState, command: InputCommand) -> InputCommand {
    match command {
        InputCommand::Insert(character)
            if needs_space_after_image_placeholder(input.text(), input.cursor, character) =>
        {
            InputCommand::InsertText(format!(" {character}"))
        }
        command => command,
    }
}

/// Returns the exact character span replaced by one non-deletion text
/// command before that command mutates the input.
fn prompt_command_edit_span(
    input: &InputState,
    command: &InputCommand,
) -> Option<(usize, usize, usize)> {
    match command {
        InputCommand::Insert(_) | InputCommand::InsertNewline => {
            Some((input.cursor, input.cursor, input.cursor + 1))
        }
        InputCommand::InsertText(text) => Some((
            input.cursor,
            input.cursor,
            input.cursor + text.chars().count(),
        )),
        InputCommand::ReplaceRange { start, end, text } => {
            Some((*start, *end, *start + text.chars().count()))
        }
        _ => None,
    }
}

/// Requests cleanup for `attachments` unless the composer released none.
fn push_attachment_cleanup(effects: &mut Vec<PromptEffect>, attachments: Vec<PromptAttachment>) {
    if !attachments.is_empty() {
        effects.push(PromptEffect::CleanupAttachments(attachments));
    }
}

/// Reopens or dismisses the `@` mention dropdown to match the current prompt
/// cursor position.
///
/// This keeps previously inserted `@path` tokens editable after the user types
/// more text elsewhere and later moves the cursor back into the mention. A
/// reopened dropdown requests its entries through
/// [`PromptEffect::LoadAtMentionEntries`].
fn sync_prompt_at_mention_state(mode: &mut AppMode, effects: &mut Vec<PromptEffect>) {
    let AppMode::Prompt {
        at_mention_state,
        input,
        slash_state,
        ..
    } = mode
    else {
        return;
    };
    let input_mode = prompt_input_mode(at_mention_state.as_ref(), input, slash_state);

    match at_mention::sync_action(input, at_mention_state.as_ref()) {
        at_mention::AtMentionSyncAction::Activate
            if input_mode != PromptInputMode::SlashCommand =>
        {
            *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
            effects.push(PromptEffect::LoadAtMentionEntries);
        }
        at_mention::AtMentionSyncAction::Dismiss => at_mention::dismiss(at_mention_state),
        at_mention::AtMentionSyncAction::KeepOpen => at_mention_state
            .iter_mut()
            .for_each(at_mention::reset_selection),
        at_mention::AtMentionSyncAction::Activate => {}
    }
}

fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == event::KeyModifiers::NONE
}

/// Returns whether the prompt transcript currently owns keyboard focus.
fn prompt_chat_is_focused(mode: &AppMode) -> bool {
    matches!(
        mode,
        AppMode::Prompt {
            focus: ChatFocus::Chat,
            ..
        }
    )
}

/// Saves the complete prompt composer and returns to the sessions list.
fn exit_to_list_saving_progress(mode: &mut AppMode, effects: &mut Vec<PromptEffect>) {
    if let Some(snapshot) = take_prompt_snapshot(mode) {
        effects.push(PromptEffect::SaveProgress(snapshot));
    }
}

/// Snapshots the current prompt-mode state for later restoration.
///
/// Abandons any running clipboard image capture, so a restored composer is
/// idle and the late capture is removed as orphaned.
///
/// Returns `None` and preserves the mode when it is not a prompt.
fn take_prompt_snapshot(mode: &mut AppMode) -> Option<PromptModeSnapshot> {
    let previous_mode = std::mem::replace(mode, AppMode::List);

    if let AppMode::Prompt {
        at_mention_state,
        mut attachment_state,
        history_state,
        input,
        scroll_offset,
        session_id,
        slash_state,
        ..
    } = previous_mode
    {
        attachment_state.pending_image_paste = None;

        Some(PromptModeSnapshot {
            at_mention_state,
            attachment_state,
            history_state,
            input,
            scroll_offset,
            session_id,
            slash_state,
        })
    } else {
        *mode = previous_mode;

        None
    }
}

/// Handles keys while the chat transcript above the composer holds focus.
///
/// The shared chat-focus classifier handles `Tab`, transcript navigation, and
/// unsupported keys. `d` opens the diff preview for the session, mirroring
/// question mode, and snapshots the composer so exiting the diff view restores
/// the prompt with its draft, attachments, and history intact. Every other key
/// — including `Ctrl+C` and `Esc` — is swallowed so the typed draft and the
/// prompt itself cannot change while the user reads back the conversation.
/// Swallowed keys skip scroll-metric construction, which lays out the
/// transcript.
///
/// Returns `None` when the focused transcript leaves the key unclaimed.
fn reduce_chat_focus_key(mode: &mut AppMode, key: KeyEvent) -> Option<PromptKeyStep> {
    let AppMode::Prompt { focus, .. } = mode else {
        return None;
    };

    match chat_scroll::classify_chat_focus_action(*focus, key)? {
        chat_scroll::ChatFocusAction::ToggleFocus => {
            chat_scroll::toggle_chat_focus(focus);

            Some(PromptKeyStep::Effects(Vec::new()))
        }
        chat_scroll::ChatFocusAction::OpenDiff => {
            let restore = take_prompt_snapshot(mode).map(DiffRestoreTarget::Prompt);

            Some(PromptKeyStep::Effects(vec![PromptEffect::OpenDiff {
                restore,
            }]))
        }
        chat_scroll::ChatFocusAction::Scroll => Some(PromptKeyStep::ScrollTranscript),
        chat_scroll::ChatFocusAction::Swallow => Some(PromptKeyStep::Effects(Vec::new())),
    }
}

/// Reduces all editing, navigation, and submission keys in prompt mode.
fn reduce_editing_key(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    input_width: u16,
    key: KeyEvent,
    effects: &mut Vec<PromptEffect>,
) {
    match key.code {
        KeyCode::BackTab => {
            let permission_mode = match prompt_context.permission_mode {
                PermissionMode::AutoEdit => PermissionMode::AutoEditAddressComments,
                PermissionMode::AutoEditAddressComments => PermissionMode::ReadOnly,
                PermissionMode::ReadOnly => PermissionMode::AutoEdit,
            };

            effects.push(PromptEffect::PersistPermissionMode(permission_mode));
        }
        KeyCode::Enter | KeyCode::Char('\r' | '\n') if !input_key::should_insert_newline(key) => {
            reduce_prompt_submit(mode, prompt_context, effects);
        }
        KeyCode::Esc | KeyCode::Char('c') if is_prompt_cancel_key(key) => {
            reduce_prompt_cancel(mode, prompt_context, effects);
        }
        KeyCode::Up => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, false, effects);
        }
        KeyCode::Down => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, true, effects);
        }
        KeyCode::Char('k') if prompt_context.is_slash_command() && is_plain_char_key(key, 'k') => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, false, effects);
        }
        KeyCode::Char('j') if prompt_context.is_slash_command() && is_plain_char_key(key, 'j') => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, true, effects);
        }
        KeyCode::Char('v' | 'V') if is_prompt_image_paste_key(key) => {
            if let Some(attachment_number) = prompt_image_paste_number(mode) {
                effects.push(PromptEffect::StartImagePaste { attachment_number });
            }
        }
        KeyCode::Char('p') if input_key::is_control_key(key) => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, false, effects);
        }
        KeyCode::Char('n') if input_key::is_control_key(key) => {
            reduce_prompt_vertical_key(mode, prompt_context, input_width, true, effects);
        }
        _ => {
            if let Some(command) =
                input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
            {
                reduce_prompt_input_command(mode, command, effects);
            }
        }
    }
}

/// Drains the active prompt into a turn submission when it passes
/// prompt-mode validation.
///
/// While the session is `InProgress` or `Rebasing`, slash input selects
/// setting commands only; model and speed changes wait for the active work,
/// and other text is queued. A composer armed by `/graph` requests a graph
/// proposal for the typed task instead. A submitted prompt clears any cached
/// focused-review output for the session so the next turn starts from the raw
/// transcript again.
///
/// Submission is ignored while a clipboard image capture runs, so the pasted
/// image is never dropped from the sent turn or a cleared slash input.
fn reduce_prompt_submit(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    effects: &mut Vec<PromptEffect>,
) {
    let AppMode::Prompt {
        attachment_state,
        input,
        slash_state,
        ..
    } = mode
    else {
        return;
    };
    if attachment_state.is_pasting_image() {
        return;
    }

    if prompt_context.is_slash_command() {
        reduce_prompt_slash_submit(mode, prompt_context, effects);

        return;
    }

    if !input.text().is_empty()
        && let Some(selected_agent) = prompt_context.sign_in_agent
    {
        open_chatgpt_sign_in(mode, selected_agent, effects);

        return;
    }

    let saved_attachment_state = attachment_state.clone();
    let saved_input = input.clone();
    let is_graph_armed = slash_state.graph.is_armed();
    let (prompt, archived_attachments) = take_submitted_turn_prompt(mode);

    effects.push(PromptEffect::SubmitPrompt(PromptTurnSubmission {
        archived_attachments,
        is_graph_armed,
        saved_attachment_state,
        saved_input,
        submission: PromptSubmission {
            prompt,
            session_id: prompt_context.session_id.clone(),
            session_mode: prompt_context.session_mode,
        },
    }));
}

/// Reduces the selected slash-command action from presentation-owned state.
fn reduce_prompt_slash_submit(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    effects: &mut Vec<PromptEffect>,
) {
    let selection = match mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => {
            let selection = resolve_prompt_slash_selection(
                input.text(),
                slash_state,
                prompt_context.session_agent_kind,
                prompt_context.slash_command_set,
            );
            if selection == Some(PromptSuggestionSelection::Command("/graph")) {
                slash_state.graph = PromptGraphState::Armed;
            }

            selection
        }
        _ => None,
    };
    match selection {
        Some(PromptSuggestionSelection::Command("/graph")) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
        }
        Some(PromptSuggestionSelection::Command("/apply")) => {
            effects.push(PromptEffect::ApplyFocusedReview);
        }
        Some(PromptSuggestionSelection::Command("/mode")) => {
            let selected_index = PermissionMode::ALL
                .iter()
                .position(|permission_mode| *permission_mode == prompt_context.permission_mode)
                .unwrap_or(0);

            open_prompt_slash_stage(mode, PromptSlashStage::Mode, selected_index);
        }
        Some(PromptSuggestionSelection::Command("/reasoning")) => {
            let selected_index = ReasoningLevel::ALL
                .iter()
                .position(|level| *level == prompt_context.reasoning_level)
                .unwrap_or(0);

            open_prompt_slash_stage(mode, PromptSlashStage::Reasoning, selected_index);
        }
        Some(PromptSuggestionSelection::Command("/fast")) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            effects.push(PromptEffect::ToggleSpeedMode);
        }
        Some(PromptSuggestionSelection::Command("/style")) => {
            let selected_index = ResponseStyle::ALL
                .iter()
                .position(|response_style| *response_style == prompt_context.response_style)
                .unwrap_or(0);

            open_prompt_slash_stage(mode, PromptSlashStage::Style, selected_index);
        }
        Some(PromptSuggestionSelection::Command(_)) => {
            if let AppMode::Prompt { slash_state, .. } = mode
                && slash_state.open_model_stage()
            {
                effects.push(PromptEffect::RefreshHarnessModels);
            }
        }
        Some(PromptSuggestionSelection::Agent(selected_agent)) => {
            update_prompt_slash_state(mode, |slash_state| {
                slash_state.selected_agent = Some(selected_agent);
                slash_state.stage = PromptSlashStage::Model;
                slash_state.selected_index = 0;
            });
        }
        Some(PromptSuggestionSelection::Model(selected_agent)) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            effects.push(PromptEffect::UpdateModel(selected_agent));
        }
        Some(PromptSuggestionSelection::SignIn(selected_agent)) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            open_chatgpt_sign_in(mode, selected_agent, effects);
        }
        Some(PromptSuggestionSelection::Mode(permission_mode)) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            effects.push(PromptEffect::PersistPermissionMode(permission_mode));
        }
        Some(PromptSuggestionSelection::Reasoning(reasoning_level)) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            effects.push(PromptEffect::UpdateReasoningLevel(reasoning_level));
        }
        Some(PromptSuggestionSelection::Style(response_style)) => {
            push_attachment_cleanup(effects, clear_prompt_slash_input(mode));
            effects.push(PromptEffect::UpdateResponseStyle(response_style));
        }
        None => {}
    }
}

/// Opens one slash-command stage with `selected_index` highlighted.
fn open_prompt_slash_stage(mode: &mut AppMode, stage: PromptSlashStage, selected_index: usize) {
    update_prompt_slash_state(mode, |slash_state| {
        slash_state.stage = stage;
        slash_state.selected_agent = None;
        slash_state.selected_index = selected_index;
    });
}

/// Captures the composer and requests the `ChatGPT` sign-in popup for
/// `selected_agent`, keeping the draft for when the popup closes.
fn open_chatgpt_sign_in(
    mode: &mut AppMode,
    selected_agent: AgentSelection,
    effects: &mut Vec<PromptEffect>,
) {
    if let Some(restore) = take_prompt_snapshot(mode) {
        effects.push(PromptEffect::OpenChatGptSignIn {
            restore,
            selected_agent,
        });
    }
}

/// Drains presentation-owned prompt input into an app-layer submission and
/// returns archived attachment files that require cleanup.
fn take_submitted_turn_prompt(mode: &mut AppMode) -> (TurnPrompt, Vec<PromptAttachment>) {
    let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = mode
    else {
        return (TurnPrompt::from_text(String::new()), Vec::new());
    };
    let archived_attachments = attachment_state.archived_attachments.clone();
    let submission = drain_prompt_submission(attachment_state, input);
    let attachments = submission
        .attachments
        .into_iter()
        .map(|attachment| TurnPromptAttachment {
            local_image_path: attachment.local_image_path,
            placeholder: attachment.placeholder,
        })
        .collect();
    let prompt = TurnPrompt {
        attachments,
        text: submission.text,
        text_source: TurnPromptTextSource::UserPrompt,
    };

    (prompt, archived_attachments)
}

fn is_prompt_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc || key.modifiers.contains(event::KeyModifiers::CONTROL)
}

/// Cancels the active prompt and releases any composer-owned attachment
/// files.
///
/// A composer armed by `/graph` is only disarmed, keeping its draft. Existing
/// focused-review output is restored into session view because no new prompt
/// was submitted.
fn reduce_prompt_cancel(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    effects: &mut Vec<PromptEffect>,
) {
    if prompt_context.is_slash_command() {
        push_attachment_cleanup(effects, clear_prompt_slash_input(mode));

        return;
    }

    if let AppMode::Prompt { slash_state, .. } = mode
        && slash_state.graph.disarm()
    {
        return;
    }

    push_attachment_cleanup(effects, take_prompt_attachment_cleanup(mode));
    effects.push(PromptEffect::CancelPrompt);
}

/// Moves the slash-menu selection, or else the cursor between wrapped input
/// lines, or else through prompt history.
fn reduce_prompt_vertical_key(
    mode: &mut AppMode,
    prompt_context: &PromptContext,
    input_width: u16,
    is_down: bool,
    effects: &mut Vec<PromptEffect>,
) {
    if prompt_context.is_slash_command() {
        move_prompt_slash_selection(mode, prompt_context, is_down);

        return;
    }

    let moved_cursor = match mode {
        AppMode::Prompt { input, .. } => {
            let next_cursor = if is_down {
                move_input_cursor_down(input.text(), input_width, input.cursor)
            } else {
                move_input_cursor_up(input.text(), input_width, input.cursor)
            };
            let moved_cursor = next_cursor != input.cursor;
            input.cursor = next_cursor;

            moved_cursor
        }
        _ => false,
    };

    if !moved_cursor {
        if is_down {
            navigate_prompt_history_down(mode);
        } else {
            navigate_prompt_history_up(mode);
        }
    }

    sync_prompt_at_mention_state(mode, effects);
}

fn move_prompt_slash_selection(mode: &mut AppMode, prompt_context: &PromptContext, is_next: bool) {
    let AppMode::Prompt {
        input, slash_state, ..
    } = mode
    else {
        return;
    };
    let option_count = prompt_slash_option_count(
        input.text(),
        slash_state,
        prompt_context.selected_session_agent_kind,
        prompt_context.slash_command_set,
    );
    if option_count == 0 {
        return;
    }

    let selected_index = slash_state.selected_index.min(option_count - 1);
    slash_state.selected_index = if is_next {
        (selected_index + 1) % option_count
    } else {
        selected_index.checked_sub(1).unwrap_or(option_count - 1)
    };
}

/// Shows the previous prompt-history entry, stashing the draft on first use.
///
/// Navigation pauses while a clipboard image capture runs so the image lands
/// in the composer text it was pasted into instead of a history entry.
fn navigate_prompt_history_up(mode: &mut AppMode) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = mode
    {
        if history_state.entries.is_empty() || attachment_state.is_pasting_image() {
            return;
        }

        let next_index = if let Some(selected_index) = history_state.selected_index {
            selected_index.saturating_sub(1)
        } else {
            history_state.draft_text = Some(input.text().to_string());
            attachment_state.remember_current_revision(input);
            history_state.draft_input_revision = Some(input.revision());

            history_state.entries.len().saturating_sub(1)
        };

        history_state.selected_index = Some(next_index);
        attachment_state.archive_current();
        input.reset_text(history_state.entries[next_index].clone());
    }
}

/// Shows the next prompt-history entry or restores the stashed draft.
///
/// Navigation pauses while a clipboard image capture runs, matching
/// [`navigate_prompt_history_up`].
fn navigate_prompt_history_down(mode: &mut AppMode) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = mode
    {
        if attachment_state.is_pasting_image() {
            return;
        }

        let Some(selected_index) = history_state.selected_index else {
            return;
        };

        if selected_index + 1 < history_state.entries.len() {
            let next_index = selected_index + 1;

            history_state.selected_index = Some(next_index);
            attachment_state.archive_current();
            input.reset_text(history_state.entries[next_index].clone());

            return;
        }

        history_state.selected_index = None;
        let draft_input_revision = history_state.draft_input_revision.take();
        input.reset_text(history_state.draft_text.take().unwrap_or_default());
        if let Some(draft_input_revision) = draft_input_revision {
            attachment_state.restore_draft_revision(draft_input_revision, input);
        } else {
            attachment_state.archive_current();
        }
    }
}

/// Returns the attachment number for a new clipboard-image capture, or
/// `None` outside a prompt or while one capture still runs.
fn prompt_image_paste_number(mode: &AppMode) -> Option<usize> {
    match mode {
        AppMode::Prompt {
            attachment_state, ..
        } if !attachment_state.is_pasting_image() => Some(attachment_state.next_attachment_number),
        _ => None,
    }
}

/// Reduces accepted paste input, or returns `None` when the paste is ignored.
fn apply_prompt_paste_to_mode(
    mode: &mut AppMode,
    pasted_text: &str,
) -> Option<Vec<PromptAttachment>> {
    let normalized_text = input_key::normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return None;
    }

    if let AppMode::Prompt {
        focus: ChatFocus::Chat,
        ..
    } = mode
    {
        return None;
    }

    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = mode
    {
        attachment_state.remember_current_revision(input);
        let insert_start = input.cursor;
        insert_prompt_text(input, history_state, slash_state, &normalized_text);
        attachment_state.sync_after_edit(input, insert_start, insert_start, input.cursor);
        unreachable_attachments = attachment_state.prune_unreachable(input);
    }

    Some(unreachable_attachments)
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;

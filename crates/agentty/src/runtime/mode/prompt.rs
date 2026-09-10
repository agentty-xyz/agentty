use std::io;
use std::path::PathBuf;

use crossterm::event::{self, KeyCode, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;

use crate::app::App;
use crate::app::prompt_intent::{
    PromptApplyOutcome, PromptCancellation, PromptImagePaste, PromptSessionMode, PromptSubmission,
    PromptWorkflowOutcome,
};
use crate::domain::agent::{AgentKind, ReasoningLevel, ResponseStyle, SpeedMode};
use crate::domain::composer::PromptAttachment;
use crate::domain::input::{InputCommand, InputEffect, InputState};
use crate::domain::permission::PermissionMode;
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffRestoreTarget, DiffSidebarFocus, PromptModeSnapshot,
};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptSlashStage, PromptSuggestionSelection,
    apply_prompt_delete_range as apply_prompt_delete_range_components,
    current_line_delete_range as prompt_current_line_delete_range, drain_prompt_submission,
    insert_prompt_local_image, insert_prompt_text, prompt_slash_option_count,
    resolve_prompt_slash_selection,
};
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::RenderCacheStore;
use crate::ui::input_layout::{move_input_cursor_down, move_input_cursor_up};

/// Captures prompt-mode routing flags derived from the current session.
///
/// Draft sessions only stage prompts while they remain in `Status::Draft`.
/// After the first turn starts, follow-up submissions must route through the
/// normal reply path even though the session still records draft origin.
struct PromptContext {
    input_mode: PromptInputMode,
    scroll_offset: Option<u16>,
    session_id: SessionId,
    session_index: usize,
    session_mode: PromptSessionMode,
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

/// Handles key input while the app is in `AppMode::Prompt`.
///
/// `Tab` moves focus between the composer and the chat transcript above it,
/// unless the `@`-mention dropdown is open and claims the key for completion.
/// `Shift+Tab` cycles the session permission mode while the composer is
/// focused. While the transcript holds focus, scroll keys navigate it and the
/// composer text stays untouched. Pressing `q` from transcript focus returns to
/// the sessions list and saves the complete composer for the next reopen.
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

    if !prompt_context.is_slash_command() {
        reset_prompt_slash_state(app);
    }

    if prompt_context.is_at_mention() && handle_at_mention_key(app, key).await {
        return Ok(EventResult::Continue);
    }

    if is_plain_char_key(key, 'q') && prompt_chat_is_focused(app) {
        exit_to_list_saving_progress(app);

        return Ok(EventResult::Continue);
    }

    if handle_chat_focus_key(app, render_cache_store, terminal, &prompt_context, key)? {
        return Ok(EventResult::Continue);
    }

    handle_editing_key(app, terminal, key, &prompt_context).await?;

    Ok(EventResult::Continue)
}

/// Returns whether the prompt transcript currently owns keyboard focus.
fn prompt_chat_is_focused(app: &App) -> bool {
    matches!(
        app.mode,
        AppMode::Prompt {
            focus: ChatFocus::Chat,
            ..
        }
    )
}

/// Saves the complete prompt composer and returns to the sessions list.
fn exit_to_list_saving_progress(app: &mut App) {
    if let Some(snapshot) = take_prompt_snapshot(app) {
        app.save_prompt_progress(snapshot);
    }
}

/// Handles keys while the chat transcript above the composer holds focus.
///
/// The shared chat-focus classifier handles `Tab`, transcript navigation, and
/// unsupported keys. `d` opens the diff preview for the session, mirroring
/// question mode. Every other key — including `Ctrl+C` and `Esc` — is swallowed
/// so the typed draft and the prompt itself cannot change while the user reads
/// back the conversation. Swallowed keys skip scroll-metric construction, which
/// lays out the transcript.
///
/// Returns `true` when the key was consumed by the focused transcript.
fn handle_chat_focus_key<B: Backend>(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal: &Terminal<B>,
    prompt_context: &PromptContext,
    key: KeyEvent,
) -> io::Result<bool>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let AppMode::Prompt { focus, .. } = &app.mode else {
        return Ok(false);
    };

    match chat_scroll::classify_chat_focus_action(*focus, key) {
        None => Ok(false),
        Some(chat_scroll::ChatFocusAction::ToggleFocus) => {
            if let AppMode::Prompt { focus, .. } = &mut app.mode {
                chat_scroll::toggle_chat_focus(focus);
            }

            Ok(true)
        }
        Some(chat_scroll::ChatFocusAction::OpenDiff) => {
            show_prompt_diff(app, &prompt_context.session_id);

            Ok(true)
        }
        Some(chat_scroll::ChatFocusAction::Scroll) => {
            let terminal_size = terminal.size().map_err(crate::runtime::backend_err)?;
            let metrics = ChatScrollMetrics::new(
                app,
                render_cache_store,
                &prompt_context.session_id,
                prompt_context.session_index,
                Rect::new(0, 0, terminal_size.width, terminal_size.height),
            );

            if let AppMode::Prompt { scroll_offset, .. } = &mut app.mode {
                chat_scroll::apply_scroll_key(scroll_offset, metrics, key);
            }

            Ok(true)
        }
        Some(chat_scroll::ChatFocusAction::Swallow) => Ok(true),
    }
}

/// Opens the diff preview from prompt mode.
///
/// Snapshots the current composer state so that exiting the diff view restores
/// the prompt with its draft, attachments, and history intact instead of
/// falling back to session view.
fn show_prompt_diff(app: &mut App, session_id: &str) {
    let restore = take_prompt_snapshot(app).map(DiffRestoreTarget::Prompt);
    app.start_diff_view_load(
        &SessionId::from(session_id),
        restore,
        DiffSidebarFocus::Files,
        false,
    );
}

/// Snapshots the current prompt-mode state for later restoration.
///
/// Returns `None` if the app is not in prompt mode.
fn take_prompt_snapshot(app: &mut App) -> Option<PromptModeSnapshot> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);

    if let AppMode::Prompt {
        at_mention_state,
        attachment_state,
        history_state,
        input,
        scroll_offset,
        session_id,
        slash_state,
        ..
    } = mode
    {
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
        app.mode = mode;

        None
    }
}

/// Handles keys when the at-mention dropdown is active.
///
/// Returns `true` if the key was consumed by at-mention logic.
async fn handle_at_mention_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => dismiss_at_mention(app),
        KeyCode::Enter if !input_key::should_insert_newline(key) => {
            handle_at_mention_select(app).await;
        }
        KeyCode::Tab => handle_at_mention_select(app).await,
        KeyCode::Up => handle_at_mention_up(app),
        KeyCode::Down => handle_at_mention_down(app),
        _ => return false,
    }

    true
}

/// Handles all editing, navigation, and submission keys in prompt mode.
async fn handle_editing_key<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    match key.code {
        KeyCode::BackTab => {
            toggle_prompt_permission_mode(app, prompt_context).await;
        }
        KeyCode::Enter | KeyCode::Char('\r' | '\n') if !input_key::should_insert_newline(key) => {
            handle_prompt_submit_key(app, prompt_context).await;
        }
        KeyCode::Esc | KeyCode::Char('c') if is_prompt_cancel_key(key) => {
            handle_prompt_cancel_key(app, prompt_context).await;
        }
        KeyCode::Up => handle_prompt_up_key(app, terminal, prompt_context)?,
        KeyCode::Down => handle_prompt_down_key(app, terminal, prompt_context)?,
        KeyCode::Char('k') if prompt_context.is_slash_command() && is_plain_char_key(key, 'k') => {
            handle_prompt_up_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('j') if prompt_context.is_slash_command() && is_plain_char_key(key, 'j') => {
            handle_prompt_down_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('v' | 'V') if is_prompt_image_paste_key(key) => {
            handle_prompt_image_paste(app, prompt_context).await;
        }
        KeyCode::Char('p') if input_key::is_control_key(key) => {
            handle_prompt_up_key(app, terminal, prompt_context)?;
        }
        KeyCode::Char('n') if input_key::is_control_key(key) => {
            handle_prompt_down_key(app, terminal, prompt_context)?;
        }
        _ => {
            if let Some(command) =
                input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
            {
                apply_prompt_input_command(app, command).await;
            }
        }
    }

    Ok(())
}

/// Applies one shared input command while preserving prompt-specific
/// attachment, history, slash-command, and `@`-mention behavior.
async fn apply_prompt_input_command(app: &mut App, command: InputCommand) {
    let delete_range = if let AppMode::Prompt { input, .. } = &app.mode {
        prompt_command_delete_range(input, &command)
    } else {
        None
    };

    if let Some((start, end)) = delete_range {
        apply_prompt_delete_range(app, start, end).await;

        return;
    }

    let is_history_restore = matches!(command, InputCommand::Undo | InputCommand::Redo);
    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        attachment_state.remember_current_revision(input);
        let edit_span = prompt_command_edit_span(input, &command);
        let effect = input.apply(command);
        if effect == InputEffect::TextChanged {
            if is_history_restore {
                attachment_state.sync_after_history_restore(input);
            } else if let Some((old_start, old_end, new_end)) = edit_span {
                attachment_state.sync_after_edit(input, old_start, old_end, new_end);
            }
            unreachable_attachments = attachment_state.prune_unreachable(input);
            history_state.reset_navigation();
            slash_state.reset();
        }
    }

    app.cleanup_prompt_attachments(unreachable_attachments)
        .await;
    sync_prompt_at_mention_state(app);
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

/// Inserts pasted content into the prompt input while normalizing mixed
/// line-endings to `\n`.
///
/// Pastes are dropped while the chat transcript holds focus so scrolling the
/// conversation never rewrites the typed draft.
pub(crate) async fn handle_paste(app: &mut App, pasted_text: &str) {
    let normalized_text = input_key::normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return;
    }

    if let AppMode::Prompt {
        focus: ChatFocus::Chat,
        ..
    } = &app.mode
    {
        return;
    }

    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        attachment_state.remember_current_revision(input);
        let insert_start = input.cursor;
        insert_prompt_text(input, history_state, slash_state, &normalized_text);
        attachment_state.sync_after_edit(input, insert_start, insert_start, input.cursor);
        unreachable_attachments = attachment_state.prune_unreachable(input);
    }

    app.cleanup_prompt_attachments(unreachable_attachments)
        .await;
    sync_prompt_at_mention_state(app);
}

/// Returns the active prompt context for the currently edited session.
fn prompt_context(app: &mut App) -> Option<PromptContext> {
    let (is_at_mention, is_slash_command, scroll_offset, session_id) = match &app.mode {
        AppMode::Prompt {
            at_mention_state,
            input,
            scroll_offset,
            session_id,
            ..
        } => (
            is_active_at_mention(at_mention_state.as_ref(), input),
            input.text().starts_with('/'),
            *scroll_offset,
            session_id.clone(),
        ),
        _ => return None,
    };

    let Some(session_index) = app.session_index_for_id(&session_id) else {
        app.mode = AppMode::List;

        return None;
    };

    let session = app.sessions.session_at(session_index);
    let session_mode = session.map_or(PromptSessionMode::Existing, |session| {
        let is_new_session = session.status == crate::domain::session::Status::Draft;

        match (
            is_new_session,
            session.is_draft_session(),
            session.has_staged_drafts(),
        ) {
            (true, true, _) => PromptSessionMode::NewDraft,
            (true, false, false) if session.transient_messages.get(crate::domain::transient_message::TransientMessageSlot::WorkspacePreparation).is_some() => PromptSessionMode::NewRegular,
            (true, false, false) => PromptSessionMode::NewDeletable,
            (true, false, true) => PromptSessionMode::NewRegular,
            (false, _, _) => PromptSessionMode::Existing,
        }
    });
    // While the session is `InProgress` or `Rebasing` the composer queues the
    // next chat message instead of dispatching it. Demote a leading `/` to
    // plain text so slash commands cannot run while the active operation is
    // still in flight and so arrow-key navigation behaves as text editing
    // rather than slash-menu selection.
    let session_queues_messages = session.is_some_and(|session| {
        matches!(
            session.status,
            crate::domain::session::Status::InProgress | crate::domain::session::Status::Rebasing
        )
    });
    let input_mode = match (is_at_mention, is_slash_command, session_queues_messages) {
        (true, _, _) => PromptInputMode::AtMention,
        (false, true, false) => PromptInputMode::SlashCommand,
        (false, _, _) => PromptInputMode::Text,
    };

    Some(PromptContext {
        input_mode,
        scroll_offset,
        session_id,
        session_index,
        session_mode,
    })
}

fn is_active_at_mention(
    at_mention_state: Option<&PromptAtMentionState>,
    input: &InputState,
) -> bool {
    at_mention_state.is_some() && input.at_mention_query().is_some()
}

/// Reopens or dismisses the `@` mention dropdown to match the current prompt
/// cursor position.
///
/// This keeps previously inserted `@path` tokens editable after the user types
/// more text elsewhere and later moves the cursor back into the mention.
fn sync_prompt_at_mention_state(app: &mut App) {
    let Some(prompt_context) = prompt_context(app) else {
        return;
    };

    let sync_action = match &app.mode {
        AppMode::Prompt {
            at_mention_state,
            input,
            ..
        } => at_mention::sync_action(input, at_mention_state.as_ref()),
        _ => return,
    };

    match sync_action {
        at_mention::AtMentionSyncAction::Activate if !prompt_context.is_slash_command() => {
            activate_at_mention(app, &prompt_context);
        }
        at_mention::AtMentionSyncAction::Dismiss => dismiss_at_mention(app),
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let AppMode::Prompt {
                at_mention_state: Some(state),
                ..
            } = &mut app.mode
            {
                at_mention::reset_selection(state);
            }
        }
        at_mention::AtMentionSyncAction::Activate => {}
    }
}

fn reset_prompt_slash_state(app: &mut App) {
    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.reset();
    }
}

fn is_prompt_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc || key.modifiers.contains(event::KeyModifiers::CONTROL)
}

fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == event::KeyModifiers::NONE
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

fn handle_prompt_up_key<B: Backend>(
    app: &mut App,
    terminal: &Terminal<B>,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if prompt_context.is_slash_command() {
        move_prompt_slash_selection(app, false);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        let next_cursor = move_input_cursor_up(input.text(), input_width, input.cursor);
        if next_cursor != input.cursor {
            input.cursor = next_cursor;
            sync_prompt_at_mention_state(app);

            return Ok(());
        }
    }

    navigate_prompt_history_up(app);
    sync_prompt_at_mention_state(app);

    Ok(())
}

fn handle_prompt_down_key<B: Backend>(
    app: &mut App,
    terminal: &Terminal<B>,
    prompt_context: &PromptContext,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    if prompt_context.is_slash_command() {
        move_prompt_slash_selection(app, true);

        return Ok(());
    }

    let input_width = prompt_input_width(terminal)?;
    if let AppMode::Prompt { input, .. } = &mut app.mode {
        let next_cursor = move_input_cursor_down(input.text(), input_width, input.cursor);
        if next_cursor != input.cursor {
            input.cursor = next_cursor;
            sync_prompt_at_mention_state(app);

            return Ok(());
        }
    }

    navigate_prompt_history_down(app);
    sync_prompt_at_mention_state(app);

    Ok(())
}

fn navigate_prompt_history_up(app: &mut App) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = &mut app.mode
    {
        if history_state.entries.is_empty() {
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

fn navigate_prompt_history_down(app: &mut App) {
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        ..
    } = &mut app.mode
    {
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

fn move_prompt_slash_selection(app: &mut App, is_next: bool) {
    let (
        available_agent_kinds,
        input_text,
        personalities,
        selected_agent,
        selected_index,
        session_agent_kind,
        session_id,
        stage,
    ) = match &app.mode {
        AppMode::Prompt {
            input,
            session_id,
            slash_state,
            ..
        } => (
            slash_state.available_agent_kinds.clone(),
            input.text().to_string(),
            slash_state.personalities.clone(),
            slash_state.selected_agent,
            slash_state.selected_index,
            app.selected_session()
                .map_or(AgentKind::Codex, |session| session.agent.kind()),
            Some(session_id.clone()),
            slash_state.stage,
        ),
        _ => return,
    };
    let allow_apply_command = session_id
        .is_some_and(|session_id| app.prompt_apply_command_is_available_for_session(&session_id));

    let option_count = prompt_slash_option_count(
        &input_text,
        stage,
        selected_agent,
        &available_agent_kinds,
        &personalities,
        session_agent_kind,
        allow_apply_command,
    );
    if option_count == 0 {
        return;
    }

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        let selected_index = selected_index.min(option_count - 1);
        slash_state.selected_index = if is_next {
            (selected_index + 1) % option_count
        } else {
            selected_index.checked_sub(1).unwrap_or(option_count - 1)
        };
    }
}

/// Submits the active prompt when it passes prompt-mode validation.
///
/// A submitted prompt clears any cached focused-review output for the session
/// so the next turn starts from the raw transcript again. While the session
/// is `InProgress` or `Rebasing`, slash command mode is already demoted to
/// text in [`prompt_context`], so any leading `/` falls through to the queue
/// path instead of executing a slash command against the active operation.
async fn handle_prompt_submit_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command() {
        handle_prompt_slash_submit(app, prompt_context).await;

        return;
    }

    let composer = match &app.mode {
        AppMode::Prompt {
            input,
            attachment_state,
            ..
        } => Some((input.clone(), attachment_state.clone())),
        _ => None,
    };
    let (prompt, archived_attachments) = take_submitted_turn_prompt(app);
    let outcome = app
        .submit_prompt(PromptSubmission {
            prompt,
            session_id: prompt_context.session_id.clone(),
            session_mode: prompt_context.session_mode,
        })
        .await;

    if matches!(outcome, PromptWorkflowOutcome::KeepPrompt) {
        if let (
            Some((saved_input, saved_attachments)),
            AppMode::Prompt {
                input,
                attachment_state,
                ..
            },
        ) = (composer, &mut app.mode)
        {
            *input = saved_input;
            *attachment_state = saved_attachments;
        }
    } else {
        app.cleanup_prompt_attachments(archived_attachments).await;
    }
    apply_prompt_workflow_outcome(app, outcome, None);
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

    handle_prompt_submit_key(app, &prompt_context).await;
}

/// Dispatches one clipboard-image paste intent for the active prompt.
async fn handle_prompt_image_paste(app: &mut App, prompt_context: &PromptContext) {
    paste_image_into_active_prompt(app, &prompt_context.session_id).await;
}

/// Cancels the active prompt and drops any composer-owned attachment files.
///
/// Existing focused-review output is restored into session view because no new
/// prompt was submitted.
async fn handle_prompt_cancel_key(app: &mut App, prompt_context: &PromptContext) {
    if prompt_context.is_slash_command() {
        clear_prompt_slash_input(app).await;

        return;
    }

    let attachments = take_prompt_attachment_cleanup(app);
    app.cleanup_prompt_attachments(attachments).await;
    let outcome = app
        .cancel_prompt(PromptCancellation {
            session_id: prompt_context.session_id.clone(),
            session_mode: prompt_context.session_mode,
        })
        .await;

    apply_prompt_workflow_outcome(app, outcome, prompt_context.scroll_offset);
}

/// Executes the selected slash-command action from presentation-owned state.
async fn handle_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
    let session_id = &prompt_context.session_id;
    let session_agent_kind = app
        .session_at(prompt_context.session_index)
        .map_or(AgentKind::Codex, |session| session.agent.kind());
    let selection = match &app.mode {
        AppMode::Prompt {
            input, slash_state, ..
        } => resolve_prompt_slash_selection(
            input.text(),
            slash_state,
            session_agent_kind,
            app.prompt_apply_command_is_available_for_session(session_id),
        ),
        _ => None,
    };
    match selection {
        Some(PromptSuggestionSelection::Command("/apply")) => {
            let outcome = app
                .apply_focused_review(session_id, prompt_context.session_index)
                .await;
            apply_prompt_apply_outcome(app, outcome).await;
        }
        Some(PromptSuggestionSelection::Command("/mode")) => {
            open_prompt_permission_mode_stage(app, prompt_context.session_index);
        }
        Some(PromptSuggestionSelection::Command("/reasoning")) => {
            open_prompt_reasoning_stage(app, prompt_context.session_index);
        }
        Some(PromptSuggestionSelection::Command("/speed")) => {
            open_prompt_speed_stage(app, prompt_context.session_index);
        }
        Some(PromptSuggestionSelection::Command("/style")) => {
            open_prompt_response_style_stage(app, prompt_context.session_index);
        }
        Some(PromptSuggestionSelection::Command("/personality")) => {
            let personalities = app.list_prompt_personalities(session_id).await;
            let selected_personality_id = app
                .session_at(prompt_context.session_index)
                .and_then(|session| session.personality_id.as_deref());
            let selected_index = selected_personality_id
                .and_then(|selected_id| {
                    personalities
                        .iter()
                        .position(|personality| personality.id == selected_id)
                })
                .map_or(0, |index| index.saturating_add(1));

            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.personalities = personalities;
                slash_state.stage = PromptSlashStage::Personality;
                slash_state.selected_agent = None;
                slash_state.selected_index = selected_index;
            }
        }
        Some(PromptSuggestionSelection::Command(_)) => {
            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.stage = PromptSlashStage::Agent;
                slash_state.selected_agent = None;
                slash_state.selected_index = 0;
            }
        }
        Some(PromptSuggestionSelection::Agent(selected_agent)) => {
            if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
                slash_state.selected_agent = Some(selected_agent);
                slash_state.stage = PromptSlashStage::Model;
                slash_state.selected_index = 0;
            }
        }
        Some(PromptSuggestionSelection::Model(selected_agent)) => {
            clear_prompt_slash_input(app).await;
            app.update_prompt_session_model(session_id, selected_agent)
                .await;
        }
        Some(PromptSuggestionSelection::Mode(permission_mode)) => {
            clear_prompt_slash_input(app).await;
            persist_prompt_permission_mode(app, prompt_context, permission_mode).await;
        }
        Some(PromptSuggestionSelection::Personality(personality)) => {
            clear_prompt_slash_input(app).await;
            app.update_prompt_session_personality(session_id, personality)
                .await;
        }
        Some(PromptSuggestionSelection::Reasoning(reasoning_level)) => {
            clear_prompt_slash_input(app).await;
            app.update_prompt_session_reasoning_level(session_id, reasoning_level)
                .await;
        }
        Some(PromptSuggestionSelection::Speed(speed_mode)) => {
            clear_prompt_slash_input(app).await;
            app.update_prompt_session_speed_mode(session_id, speed_mode)
                .await;
        }
        Some(PromptSuggestionSelection::Style(response_style)) => {
            clear_prompt_slash_input(app).await;
            app.update_prompt_session_response_style(session_id, response_style)
                .await;
        }
        None => {}
    }
}

/// Cycles and persists the permission mode without changing the composer.
async fn toggle_prompt_permission_mode(app: &mut App, prompt_context: &PromptContext) {
    let current_permission_mode = app
        .session_at(prompt_context.session_index)
        .map_or_else(PermissionMode::default, |session| session.permission_mode);
    let permission_mode = match current_permission_mode {
        PermissionMode::AutoEdit => PermissionMode::AutoEditAddressComments,
        PermissionMode::AutoEditAddressComments => PermissionMode::ReadOnly,
        PermissionMode::ReadOnly => PermissionMode::AutoEdit,
    };

    persist_prompt_permission_mode(app, prompt_context, permission_mode).await;
}

/// Persists one selected permission mode and reports failures in the target
/// session transcript.
async fn persist_prompt_permission_mode(
    app: &mut App,
    prompt_context: &PromptContext,
    permission_mode: PermissionMode,
) {
    if let Err(error) = app
        .update_prompt_session_permission_mode(&prompt_context.session_id, permission_mode)
        .await
    {
        app.append_prompt_status_line(
            &prompt_context.session_id,
            TranscriptNotice::Error,
            &format!("Failed to change mode; the session remains unchanged: {error}"),
        )
        .await;
    }
}

/// Opens `/mode` with the current session mode preselected.
fn open_prompt_permission_mode_stage(app: &mut App, session_index: usize) {
    let selected_permission_mode = app
        .session_at(session_index)
        .map_or_else(PermissionMode::default, |session| session.permission_mode);
    let selected_index = PermissionMode::ALL
        .iter()
        .position(|permission_mode| *permission_mode == selected_permission_mode)
        .unwrap_or(0);

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Mode;
        slash_state.selected_agent = None;
        slash_state.selected_index = selected_index;
    }
}

/// Opens `/reasoning` with the effective session level preselected.
fn open_prompt_reasoning_stage(app: &mut App, session_index: usize) {
    let selected_reasoning_level = app
        .session_at(session_index)
        .map_or(app.settings.default_smart_reasoning_level, |session| {
            session.effective_reasoning_level()
        });
    let selected_index = ReasoningLevel::ALL
        .iter()
        .position(|level| *level == selected_reasoning_level)
        .unwrap_or(0);

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Reasoning;
        slash_state.selected_agent = None;
        slash_state.selected_index = selected_index;
    }
}

/// Opens `/style` with the current session preference preselected.
fn open_prompt_response_style_stage(app: &mut App, session_index: usize) {
    let selected_response_style = app
        .session_at(session_index)
        .map_or_else(ResponseStyle::default, |session| session.response_style);
    let selected_index = ResponseStyle::ALL
        .iter()
        .position(|response_style| *response_style == selected_response_style)
        .unwrap_or(0);

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Style;
        slash_state.selected_agent = None;
        slash_state.selected_index = selected_index;
    }
}

/// Opens `/speed` with the current session preference preselected.
fn open_prompt_speed_stage(app: &mut App, session_index: usize) {
    let selected_speed_mode = app
        .session_at(session_index)
        .map_or_else(SpeedMode::default, |session| session.speed_mode);
    let selected_index = SpeedMode::ALL
        .iter()
        .position(|speed_mode| *speed_mode == selected_speed_mode)
        .unwrap_or(0);

    if let AppMode::Prompt { slash_state, .. } = &mut app.mode {
        slash_state.stage = PromptSlashStage::Speed;
        slash_state.selected_agent = None;
        slash_state.selected_index = selected_index;
    }
}

/// Applies the navigation requested by one app-layer prompt workflow.
fn apply_prompt_workflow_outcome(
    app: &mut App,
    outcome: PromptWorkflowOutcome,
    scroll_offset: Option<u16>,
) {
    match outcome {
        PromptWorkflowOutcome::KeepPrompt => {}
        PromptWorkflowOutcome::ShowSession { session_id } => {
            app.mode = AppMode::View {
                scroll_offset,
                session_id,
            };
        }
        PromptWorkflowOutcome::ShowSessionList => app.mode = AppMode::List,
    }
}

/// Applies the composer and navigation changes requested by `/apply`.
async fn apply_prompt_apply_outcome(app: &mut App, outcome: PromptApplyOutcome) {
    match outcome {
        PromptApplyOutcome::ClearComposer => clear_prompt_slash_input(app).await,
        PromptApplyOutcome::KeepComposer => reset_prompt_slash_state(app),
        PromptApplyOutcome::ShowSession { session_id } => {
            let attachments = take_prompt_attachment_cleanup(app);
            app.cleanup_prompt_attachments(attachments).await;
            app.mode = AppMode::View {
                scroll_offset: None,
                session_id,
            };
        }
    }
}

/// Persists a clipboard image and inserts it into the active presentation
/// composer when the capture succeeds.
pub(crate) async fn paste_image_into_active_prompt(app: &mut App, session_id: &SessionId) {
    let attachment_number = match &app.mode {
        AppMode::Prompt {
            attachment_state, ..
        } => attachment_state.next_attachment_number,
        _ => return,
    };
    let request = PromptImagePaste {
        attachment_number,
        session_id: session_id.clone(),
    };
    let Some(local_image_path) = app.persist_prompt_image(request).await else {
        return;
    };

    let unreachable_attachments = insert_pasted_image_placeholder(app, local_image_path);
    app.cleanup_prompt_attachments(unreachable_attachments)
        .await;
}

/// Inserts one persisted image placeholder into presentation-owned prompt
/// state.
fn insert_pasted_image_placeholder(
    app: &mut App,
    local_image_path: PathBuf,
) -> Vec<PromptAttachment> {
    if let AppMode::Prompt {
        at_mention_state,
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
    {
        insert_prompt_local_image(
            attachment_state,
            history_state,
            input,
            slash_state,
            local_image_path,
        );
        *at_mention_state = None;

        return attachment_state.prune_unreachable(input);
    }

    Vec::new()
}

/// Drains presentation-owned prompt input into an app-layer submission and
/// returns archived attachment files that require cleanup.
fn take_submitted_turn_prompt(app: &mut App) -> (TurnPrompt, Vec<PromptAttachment>) {
    let AppMode::Prompt {
        attachment_state,
        input,
        ..
    } = &mut app.mode
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

/// Removes all attachments owned by the active composer and resets that
/// presentation state before it leaves prompt mode.
fn take_prompt_attachment_cleanup(app: &mut App) -> Vec<PromptAttachment> {
    let attachments = prompt_attachment_cleanup(app);

    reset_prompt_attachment_state(app);

    attachments
}

/// Clones every image attachment owned by the active presentation composer.
fn prompt_attachment_cleanup(app: &App) -> Vec<PromptAttachment> {
    let AppMode::Prompt {
        attachment_state, ..
    } = &app.mode
    else {
        return Vec::new();
    };

    attachment_state
        .attachments
        .iter()
        .chain(&attachment_state.archived_attachments)
        .cloned()
        .collect()
}

/// Clears attachment state after its files have been cleaned up elsewhere.
fn reset_prompt_attachment_state(app: &mut App) {
    let AppMode::Prompt {
        attachment_state, ..
    } = &mut app.mode
    else {
        return;
    };

    attachment_state.reset();
}

/// Clears the slash buffer and cleans up composer attachments it owned.
async fn clear_prompt_slash_input(app: &mut App) {
    let attachments = take_prompt_attachment_cleanup(app);
    app.cleanup_prompt_attachments(attachments).await;

    if let AppMode::Prompt {
        input, slash_state, ..
    } = &mut app.mode
    {
        input.take_text();
        slash_state.reset();
    }
}

fn prompt_input_width<B: Backend>(terminal: &Terminal<B>) -> io::Result<u16>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let terminal_width = terminal.size().map_err(crate::runtime::backend_err)?.width;

    Ok(terminal_width.saturating_sub(2))
}

/// Applies one prompt deletion range, expanding it to cover full image
/// placeholder tokens and removing orphaned attachments from prompt state.
async fn apply_prompt_delete_range(app: &mut App, start: usize, end: usize) {
    let mut unreachable_attachments = Vec::new();
    if let AppMode::Prompt {
        attachment_state,
        history_state,
        input,
        slash_state,
        ..
    } = &mut app.mode
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

    app.cleanup_prompt_attachments(unreachable_attachments)
        .await;
    sync_prompt_at_mention_state(app);
}

/// Starts asynchronous loading of at-mention file entries for the prompt
/// session.
///
/// Draft sessions in `Draft` state defer worktree creation. Regular drafts
/// index the active project working directory, while stacked drafts index the
/// parent worktree until their own folder is materialized.
fn activate_at_mention(app: &mut App, prompt_context: &PromptContext) {
    let lookup_root = app.at_mention_lookup_root(&prompt_context.session_id);
    let session_id = prompt_context.session_id.clone();
    let event_tx = app.services.event_sender();

    at_mention::start_loading_entries(event_tx, lookup_root, session_id, &mut app.sessions);

    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    }
}

/// Clears the at-mention state.
fn dismiss_at_mention(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state, ..
    } = &mut app.mode
    {
        at_mention::dismiss(at_mention_state);
    }
}

/// Moves the at-mention selection up.
fn handle_at_mention_up(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_up(state);
    }
}

/// Moves the at-mention selection down.
fn handle_at_mention_down(app: &mut App) {
    if let AppMode::Prompt {
        at_mention_state: Some(state),
        input,
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_down(input, state);
    }
}

/// Selects the currently highlighted file and inserts it into the input.
async fn handle_at_mention_select(app: &mut App) {
    let replacement = match &app.mode {
        AppMode::Prompt {
            at_mention_state: Some(state),
            input,
            ..
        } => at_mention::selected_replacement(input, state),
        _ => return,
    };

    let Some(selection) = replacement else {
        dismiss_at_mention(app);

        return;
    };

    apply_prompt_input_command(
        app,
        InputCommand::ReplaceRange {
            start: selection.at_start,
            end: selection.at_end,
            text: selection.text,
        },
    )
    .await;

    dismiss_at_mention(app);
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;

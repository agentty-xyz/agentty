use ag_session::{AnswerQuestionsRequest, QuestionAnswer};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tracing::warn;

use crate::app::session::SessionTaskService;
use crate::app::{self, App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::question::{QuestionItem, QuestionProgress, default_option_index};
use crate::domain::session::{SessionId, Status};
use crate::presentation::app_mode::{
    AppMode, ChatFocus, DiffRestoreTarget, DiffSidebarFocus, QuestionModeSnapshot,
};
use crate::presentation::prompt::PromptAtMentionState;
use crate::runtime::EventResult;
use crate::runtime::mode::chat_scroll::{self, ChatScrollMetrics};
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::RenderCacheStore;

/// Default response stored when users skip one model question.
const NO_ANSWER: &str = "no answer";

/// Applies one key event in question-answer mode.
///
/// `Tab` toggles focus between the question panel and the chat output for
/// scrolling, unless an open `@` lookup consumes it for file selection. When
/// chat is focused, scroll keys (`j`/`k`/`Up`/`Down`/`g`/`G`/
/// `Ctrl+d`/`Ctrl+u`) navigate the session transcript. `Enter` submits the
/// typed answer (or `no answer` when blank), `Ctrl+C` ends the entire turn
/// without sending a reply while the answer input is focused, `Esc` dismisses
/// an open at-mention dropdown without ending the turn, and `q` returns to the
/// sessions list while saving already-submitted answers for the next visit
/// (skipped while the user is actively typing a free-text answer so the
/// character can still be inserted into the response).
pub(crate) async fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
    mut key: KeyEvent,
) -> EventResult {
    // Canonicalize plain terminal Enter encodings before lookup and submission
    // routing. Modified character forms retain their multiline editing meaning.
    if key.modifiers.is_empty() && input_key::is_enter_key(key.code) {
        key.code = KeyCode::Enter;
    }

    if is_plain_q(key) && should_exit_to_list_on_q(app) {
        exit_to_list_saving_progress(app);

        return EventResult::Continue;
    }

    if is_answer_input_focused(app) && is_active_at_mention(app) && handle_at_mention_key(app, key)
    {
        return EventResult::Continue;
    }

    if handle_chat_focus_key(app, render_cache_store, terminal_size, key) {
        return EventResult::Continue;
    }

    if is_ctrl_c(key) {
        if is_answer_input_focused(app) {
            end_turn_no_answer(app).await;
        }

        return EventResult::Continue;
    }

    let Some(action) = resolve_question_action(app, key) else {
        return EventResult::Continue;
    };

    match action {
        QuestionAction::Submit(response) => submit_response(app, response).await,
        QuestionAction::Continue => sync_question_at_mention_state(app),
    }

    EventResult::Continue
}

/// Returns whether `key` is a plain `Ctrl+C` press.
fn is_ctrl_c(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Returns whether the question answer input currently holds focus.
fn is_answer_input_focused(app: &App) -> bool {
    matches!(
        &app.mode,
        AppMode::Question {
            focus: ChatFocus::Input,
            ..
        }
    )
}

/// Returns whether `key` is a plain `q` press without modifiers.
fn is_plain_q(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q')) && key.modifiers.is_empty()
}

/// Returns whether a plain `q` press should exit question mode to the sessions
/// list.
///
/// `q` exits while reading the chat transcript (`ChatFocus::Chat`) or while
/// navigating predefined options (`selected_option_index` is `Some`). It is
/// preserved as a free-text character whenever the answer input is focused and
/// the user is past the option list, so answers can still contain the letter.
fn should_exit_to_list_on_q(app: &App) -> bool {
    let AppMode::Question {
        focus,
        selected_option_index,
        ..
    } = &app.mode
    else {
        return false;
    };

    *focus == ChatFocus::Chat || selected_option_index.is_some()
}

/// Saves in-progress clarification answers and returns to the sessions list.
///
/// The saved progress is restored by `App::enter_question_mode` the next
/// time the session's question mode opens, so answers already submitted for
/// earlier questions survive leaving the view with `q`.
fn exit_to_list_saving_progress(app: &mut App) {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);

    if let AppMode::Question {
        current_index,
        input,
        responses,
        selected_option_index,
        session_id,
        ..
    } = mode
    {
        app.question_progress.insert(
            session_id,
            QuestionProgress {
                current_index,
                input,
                responses,
                selected_option_index,
            },
        );
    }
}

/// Applies shared semantic actions while the chat output area is focused.
///
/// Returns `true` when the key was consumed as a chat-focus action. Question
/// mode permits the shared diff-preview action; unsupported actions are
/// swallowed to keep the answer draft unchanged.
fn handle_chat_focus_key(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
    key: KeyEvent,
) -> bool {
    let AppMode::Question {
        focus, session_id, ..
    } = &app.mode
    else {
        return false;
    };
    let focus = *focus;

    match chat_scroll::classify_chat_focus_action(focus, key) {
        None => false,
        Some(chat_scroll::ChatFocusAction::ToggleFocus) => {
            if let AppMode::Question { focus, .. } = &mut app.mode {
                chat_scroll::toggle_chat_focus(focus);
            }

            true
        }
        Some(chat_scroll::ChatFocusAction::OpenDiff) => {
            let session_id = session_id.clone();

            show_question_diff(app, &session_id);

            true
        }
        Some(chat_scroll::ChatFocusAction::Scroll) => {
            if let Some(metrics) = question_scroll_metrics(app, render_cache_store, terminal_size)
                && let AppMode::Question { scroll_offset, .. } = &mut app.mode
            {
                chat_scroll::apply_scroll_key(scroll_offset, metrics, key);
            }

            true
        }
        Some(chat_scroll::ChatFocusAction::Swallow) => true,
    }
}

/// Returns transcript scroll metrics while question mode focuses the chat.
///
/// Returns `None` when the answer input holds focus, so scroll keys stay
/// available as answer text. Questions raised for a session that is not loaded
/// into the session list scroll over an empty transcript.
fn question_scroll_metrics(
    app: &App,
    render_cache_store: &RenderCacheStore,
    terminal_size: Rect,
) -> Option<ChatScrollMetrics> {
    let AppMode::Question {
        focus: ChatFocus::Chat,
        session_id,
        ..
    } = &app.mode
    else {
        return None;
    };

    let session_index = app
        .sessions
        .sessions()
        .iter()
        .position(|session| session.id == *session_id);

    Some(session_index.map_or_else(
        || ChatScrollMetrics::empty(terminal_size),
        |session_index| {
            ChatScrollMetrics::new(
                app,
                render_cache_store,
                session_id,
                session_index,
                terminal_size,
            )
        },
    ))
}

/// Opens the diff preview from question mode.
///
/// Snapshots the current question state so that exiting the diff view
/// restores back to question mode instead of session view.
fn show_question_diff(app: &mut App, session_id: &str) {
    let restore = take_question_snapshot(app).map(DiffRestoreTarget::Question);
    app.start_diff_view_load(
        &SessionId::from(session_id),
        restore,
        DiffSidebarFocus::Files,
        false,
    );
}

/// Snapshots the current question-mode state for later restoration.
///
/// Returns `None` if the app is not in question mode.
fn take_question_snapshot(app: &mut App) -> Option<QuestionModeSnapshot> {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);

    if let AppMode::Question {
        at_mention_state,
        current_index,
        input,
        questions,
        responses,
        scroll_offset,
        selected_option_index,
        session_id,
        ..
    } = mode
    {
        Some(QuestionModeSnapshot {
            at_mention_state,
            current_index,
            input,
            questions,
            responses,
            scroll_offset,
            selected_option_index,
            session_id,
        })
    } else {
        app.mode = mode;

        None
    }
}

/// Inserts pasted text into the active question response input.
///
/// Paste only takes effect when the user is in free-text mode
/// (`selected_option_index` is `None`). While navigating predefined options,
/// paste is ignored — the user must first navigate past the last option to
/// enter free-text mode.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let normalized_text = input_key::normalize_pasted_text(pasted_text);
    if normalized_text.is_empty() {
        return;
    }

    if let AppMode::Question {
        input,
        selected_option_index,
        ..
    } = &mut app.mode
    {
        if selected_option_index.is_some() {
            return;
        }

        input.insert_text(&normalized_text);
    }

    sync_question_at_mention_state(app);
}

/// Semantic action emitted by one question-mode key event.
enum QuestionAction {
    Submit(String),
    Continue,
}

/// Resolves and applies one key event against question input state.
///
/// When navigating predefined options (`selected_option_index` is `Some`),
/// `Up`/`Down`/`j`/`k` cycle through the options. Moving past the last (or
/// first) option automatically enters free-text mode where the text input is
/// visible. In free-text mode, `Up` returns to the last predefined option
/// and `Down` wraps to the first.
fn resolve_question_action(app: &mut App, key: KeyEvent) -> Option<QuestionAction> {
    let action = {
        let AppMode::Question {
            current_index,
            input,
            questions,
            selected_option_index,
            ..
        } = &mut app.mode
        else {
            return None;
        };

        let option_count = questions
            .get(*current_index)
            .map_or(0, |item| item.options.len());
        let is_navigating_options = selected_option_index.is_some();

        match key.code {
            KeyCode::Enter | KeyCode::Char('\r' | '\n')
                if !is_navigating_options && input_key::should_insert_newline(key) =>
            {
                input.insert_newline();

                QuestionAction::Continue
            }
            KeyCode::Enter => {
                resolve_enter_action(input, questions, *current_index, selected_option_index)
            }
            KeyCode::Up | KeyCode::Char('k') if is_navigating_options => {
                navigate_option_up(selected_option_index);

                QuestionAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') if is_navigating_options => {
                navigate_option_down(selected_option_index, option_count);

                QuestionAction::Continue
            }
            KeyCode::Up
                if !is_navigating_options
                    && option_count > 0
                    && input_key::is_cursor_on_first_line(input) =>
            {
                *selected_option_index = Some(option_count - 1);

                QuestionAction::Continue
            }
            KeyCode::Down
                if !is_navigating_options
                    && option_count > 0
                    && input_key::is_cursor_on_last_line(input) =>
            {
                *selected_option_index = Some(0);

                QuestionAction::Continue
            }
            _ if !is_navigating_options => resolve_free_text_key(input, key),
            _ => QuestionAction::Continue,
        }
    };

    sync_question_at_mention_state(app);

    Some(action)
}

/// Resolves an `Enter` key press in question mode.
///
/// When navigating options, submits the highlighted predefined option. In
/// free-text mode, submits the typed text.
fn resolve_enter_action(
    input: &mut InputState,
    questions: &[QuestionItem],
    current_index: usize,
    selected_option_index: &mut Option<usize>,
) -> QuestionAction {
    if let Some(option_index) = *selected_option_index {
        let selected_text = questions
            .get(current_index)
            .and_then(|item| item.options.get(option_index))
            .cloned()
            .unwrap_or_default();

        QuestionAction::Submit(normalize_response_text(&selected_text))
    } else {
        let response_text = input.take_text();

        QuestionAction::Submit(normalize_response_text(&response_text))
    }
}

/// Moves the selected option index up, entering free-text mode when wrapping
/// past the first predefined option.
///
/// # Invariant
///
/// The caller in `resolve_question_action` only routes `Up`/`k` here when
/// `is_navigating_options` (i.e. `selected_option_index.is_some()`) holds, so
/// `*selected_option_index` is always `Some(_)` on entry. Reaching the `None`
/// arm means a key handler regressed.
fn navigate_option_up(selected_option_index: &mut Option<usize>) {
    *selected_option_index = match *selected_option_index {
        Some(0) => None,
        Some(index) => Some(index.saturating_sub(1)),
        None => unreachable!("navigate_option_up requires selected_option_index = Some(_)"),
    };
}

/// Moves the selected option index down, entering free-text mode when
/// advancing past the last predefined option.
///
/// # Invariant
///
/// The caller in `resolve_question_action` only routes `Down`/`j` here when
/// `is_navigating_options` (i.e. `selected_option_index.is_some()`) holds, so
/// `*selected_option_index` is always `Some(_)` on entry. Reaching the `None`
/// arm means a key handler regressed.
fn navigate_option_down(selected_option_index: &mut Option<usize>, option_count: usize) {
    *selected_option_index = match *selected_option_index {
        Some(index) if index + 1 >= option_count => None,
        Some(index) => Some(index + 1),
        None => unreachable!("navigate_option_down requires selected_option_index = Some(_)"),
    };
}

/// Resolves a key event in free-text input mode (no option selected).
fn resolve_free_text_key(input: &mut InputState, key: KeyEvent) -> QuestionAction {
    if let Some(command) = input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
    {
        input.apply(command);
    }

    QuestionAction::Continue
}

/// Returns whether the question-mode at-mention dropdown is currently visible.
fn is_active_at_mention(app: &App) -> bool {
    matches!(
        &app.mode,
        AppMode::Question {
            at_mention_state: Some(_),
            input,
            selected_option_index: None,
            ..
        } if input.at_mention_query().is_some()
    )
}

/// Intercepts navigation/selection keys when the at-mention dropdown is open.
///
/// Returns `true` when the key was consumed by the at-mention handler.
fn handle_at_mention_key(app: &mut App, key: KeyEvent) -> bool {
    if input_key::should_insert_newline(key) {
        return false;
    }

    match key.code {
        KeyCode::Esc => dismiss_question_at_mention(app),
        KeyCode::Enter | KeyCode::Tab => {
            handle_question_at_mention_select(app);

            return true;
        }
        KeyCode::Up => handle_question_at_mention_up(app),
        KeyCode::Down => handle_question_at_mention_down(app),
        _ => return false,
    }

    true
}

/// Keeps the at-mention dropdown aligned with the current input cursor
/// position.
///
/// Opens the dropdown when the cursor sits inside an `@` token and the
/// dropdown is not yet visible. Resets the selection index when already open.
/// Dismisses the dropdown when the cursor moves away from any `@` token.
fn sync_question_at_mention_state(app: &mut App) {
    let (session_id, sync_action) = match &app.mode {
        AppMode::Question {
            at_mention_state,
            input,
            selected_option_index: None,
            session_id,
            ..
        } => (
            session_id.clone(),
            at_mention::sync_action(input, at_mention_state.as_ref()),
        ),
        _ => return,
    };

    match sync_action {
        at_mention::AtMentionSyncAction::Activate => activate_question_at_mention(app, &session_id),
        at_mention::AtMentionSyncAction::Dismiss => dismiss_question_at_mention(app),
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let AppMode::Question {
                at_mention_state: Some(state),
                ..
            } = &mut app.mode
            {
                at_mention::reset_selection(state);
            }
        }
    }
}

/// Starts asynchronous loading of file entries for the question-mode
/// at-mention dropdown.
fn activate_question_at_mention(app: &mut App, session_id: &str) {
    let lookup_root = app.at_mention_lookup_root(session_id);
    let owned_session_id = SessionId::from(session_id);
    let event_tx = app.services.event_sender();

    at_mention::start_loading_entries(event_tx, lookup_root, owned_session_id, &mut app.sessions);

    if let AppMode::Question {
        at_mention_state, ..
    } = &mut app.mode
    {
        *at_mention_state = Some(PromptAtMentionState::new(Vec::new()));
    }
}

/// Clears the question-mode at-mention dropdown state.
fn dismiss_question_at_mention(app: &mut App) {
    if let AppMode::Question {
        at_mention_state, ..
    } = &mut app.mode
    {
        at_mention::dismiss(at_mention_state);
    }
}

/// Moves the at-mention selection up in question mode.
fn handle_question_at_mention_up(app: &mut App) {
    if let AppMode::Question {
        at_mention_state: Some(state),
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_up(state);
    }
}

/// Moves the at-mention selection down in question mode.
fn handle_question_at_mention_down(app: &mut App) {
    if let AppMode::Question {
        at_mention_state: Some(state),
        input,
        ..
    } = &mut app.mode
    {
        at_mention::move_selection_down(input, state);
    }
}

/// Selects the currently highlighted file and inserts it into the question
/// input.
fn handle_question_at_mention_select(app: &mut App) {
    let replacement = match &app.mode {
        AppMode::Question {
            at_mention_state: Some(state),
            input,
            ..
        } => at_mention::selected_replacement(input, state),
        _ => return,
    };

    if replacement.is_none() {
        dismiss_question_at_mention(app);

        return;
    }

    if let Some(selection) = replacement
        && let AppMode::Question { input, .. } = &mut app.mode
    {
        input.replace_range(selection.at_start, selection.at_end, &selection.text);
    }

    sync_question_at_mention_state(app);
}

/// Stores one question response and runs follow-up reply when complete.
async fn submit_response(app: &mut App, response: String) {
    let Some(completed_response) = store_question_response(app, response) else {
        return;
    };

    let session_id = completed_response.session_id.clone();
    let answers =
        structured_question_answers(&completed_response.questions, &completed_response.responses);
    let is_orchestration_proxy = app.has_orchestration_question_proxy(&session_id).await;
    app.mode = AppMode::View {
        session_id: session_id.clone(),
        scroll_offset: None,
    };
    let service = app.session_service();
    let request = service.answer_questions(&session_id, AnswerQuestionsRequest { answers });
    let reply_enqueued = match app.drive_session_request(request).await {
        Ok(()) => true,
        Err(error) => {
            warn!(
                session_id = %session_id,
                error = %error,
                "failed to send completed question response"
            );

            false
        }
    };

    // Optimistically advance the session out of `Question` once the reply is
    // enqueued so the open-view question reconciliation does not re-open the
    // just-answered panel before the worker transitions to `InProgress`. The
    // send eligibility check runs against the pre-send `Question` status, so
    // this must happen after `send_message()`. Skip the advance when the reply
    // never reached the worker (for example a rejected stacked reply) so the
    // pending question is not hidden behind a stalled `InProgress` state.
    if reply_enqueued {
        app.clear_diff_comment_progress(&session_id);
        mark_answered_session(app, &session_id, is_orchestration_proxy);
    } else {
        restore_completed_question_response(app, completed_response);
    }
}

/// Advances the session that owned one accepted question response.
fn mark_answered_session(app: &mut App, session_id: &str, is_orchestration_proxy: bool) {
    if is_orchestration_proxy {
        mark_orchestration_controller_review(app, session_id);
    } else {
        mark_session_in_progress(app, session_id);
    }
}

struct CompletedQuestionResponse {
    questions: Vec<QuestionItem>,
    responses: Vec<String>,
    scroll_offset: Option<u16>,
    session_id: SessionId,
}

/// Restores the final question and answer when reply enqueue fails.
///
/// `store_question_response` has already consumed the completed question-mode
/// vectors by this point. Rebuilding the panel at the final question keeps all
/// earlier answers plus the just-entered final answer visible so pressing
/// `Enter` retries the same clarification reply instead of discarding input.
fn restore_completed_question_response(
    app: &mut App,
    mut completed_response: CompletedQuestionResponse,
) {
    let Some(final_response) = completed_response.responses.pop() else {
        return;
    };
    if completed_response.questions.is_empty() {
        return;
    }

    let current_index = completed_response
        .responses
        .len()
        .min(completed_response.questions.len().saturating_sub(1));
    let selected_option_index =
        completed_response
            .questions
            .get(current_index)
            .and_then(|question| {
                question
                    .options
                    .iter()
                    .position(|option| option == &final_response)
            });
    let input = if selected_option_index.is_some() || final_response == NO_ANSWER {
        InputState::default()
    } else {
        InputState::with_text(final_response)
    };

    app.mode = AppMode::Question {
        at_mention_state: None,
        current_index,
        focus: ChatFocus::Input,
        input,
        questions: completed_response.questions,
        responses: completed_response.responses,
        scroll_offset: completed_response.scroll_offset,
        selected_option_index,
        session_id: completed_response.session_id,
    };
}

/// Optimistically advances one session's live handle and snapshot status to
/// [`Status::InProgress`] so a submitted answer is reflected immediately.
///
/// Both the shared handle and the render snapshot are updated so the next
/// `sync_from_handles` sweep does not revert the snapshot back to `Question`.
/// The session worker persists the authoritative transition when it dequeues
/// the reply command.
fn mark_session_in_progress(app: &mut App, session_id: &str) {
    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::InProgress;
    }

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::InProgress;
    }
}

/// Optimistically restores a controller after its proxied child answers were
/// accepted.
fn mark_orchestration_controller_review(app: &mut App, session_id: &str) {
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
        session.status = Status::Review;
        session.questions.clear();
    }
}

/// Ends the question turn without sending a reply to the agent.
///
/// Triggered by `Ctrl+C` or `Esc` while the answer input is focused. The
/// session status is reverted to `Review` so the
/// user can inspect the current diff or start a new follow-up manually.
/// If the database write fails the mode stays on `Question` so the user
/// can retry, avoiding a split between persisted and in-memory state.
///
/// The persisted transition uses the timing-aware status update so any
/// lingering active-work interval is closed before the session returns to
/// `Review`. After the write succeeds it emits both
/// [`AppEvent::SessionUpdated`] plus session and project refresh events so the
/// UI refreshes the focused session snapshot and any aggregate project-list
/// state. It also updates the shared runtime handle status alongside the
/// snapshot so the periodic `sync_from_handles` cycle does not revert the
/// status back to `Question`.
async fn end_turn_no_answer(app: &mut App) {
    let AppMode::Question { session_id, .. } = &app.mode else {
        return;
    };

    let session_id = session_id.clone();
    let timestamp_seconds =
        app::session::unix_timestamp_from_system_time(app.services.clock().now_system_time());

    if app
        .services
        .db()
        .sessions()
        .update_session_status_with_timing_at(
            &session_id,
            &Status::Review.to_string(),
            timestamp_seconds,
        )
        .await
        .is_err()
    {
        return;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str())
        && let Ok(mut handle_status) = handles.status.lock()
    {
        *handle_status = Status::Review;
    }
    app.sessions.wake_session_worker(session_id.as_str());

    app.services.emit_app_event(AppEvent::SessionUpdated {
        session_id: session_id.clone(),
        version: SessionTaskService::next_session_update_version(
            &app.services.session_update_versions(),
            session_id.as_str(),
        ),
    });
    app.services.emit_session_and_project_refresh_events();

    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = Status::Review;
    }

    app.mode = AppMode::View {
        session_id,
        scroll_offset: None,
    };
}

/// Writes one response into question mode and returns completion payload when
/// all questions are answered.
fn store_question_response(app: &mut App, response: String) -> Option<CompletedQuestionResponse> {
    let AppMode::Question {
        at_mention_state,
        current_index,
        input,
        questions,
        responses,
        scroll_offset,
        selected_option_index,
        session_id,
        ..
    } = &mut app.mode
    else {
        return None;
    };

    responses.push(response);
    *current_index += 1;
    *input = InputState::default();
    *at_mention_state = None;
    *selected_option_index = default_option_index(questions, *current_index);

    if *current_index < questions.len() {
        return None;
    }

    Some(CompletedQuestionResponse {
        questions: std::mem::take(questions),
        responses: std::mem::take(responses),
        scroll_offset: *scroll_offset,
        session_id: session_id.clone(),
    })
}

/// Returns a normalized user response, falling back to `no answer`.
fn normalize_response_text(response_text: &str) -> String {
    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        return NO_ANSWER.to_string();
    }

    trimmed.to_string()
}

/// Pairs the current ordered question set with its collected responses.
fn structured_question_answers(
    questions: &[QuestionItem],
    responses: &[String],
) -> Vec<QuestionAnswer> {
    questions
        .iter()
        .zip(responses)
        .map(|(question, answer)| QuestionAnswer {
            answer: answer.clone(),
            question: question.text.clone(),
        })
        .collect()
}

#[cfg(test)]
#[path = "question_test.rs"]
mod tests;

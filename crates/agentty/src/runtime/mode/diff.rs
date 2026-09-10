use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::{App, AppEvent};
use crate::domain::input::InputState;
use crate::domain::session::SessionId;
use crate::presentation::app_mode::{
    AppMode, DiffCommentTarget, DiffFocus, DiffLineCommentTarget, DiffLineComments, DiffPreview,
    DiffPreviewUnavailableReason, DiffRestoreTarget, DiffReviewComments, DiffScrollCache,
    DiffSidebarFocus, HelpContext, PromptModeSnapshot, ViewportRect,
    allows_diff_line_comment_reply,
};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState,
};
use crate::runtime::EventResult;
use crate::runtime::mode::{at_mention, input_key};
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::{RenderCacheStore, diff_util, page};

/// Handles key input while the app is in `AppMode::Diff`.
///
/// File selection via `j`/`k` wraps around between the first and last file
/// explorer entries. Leaving diff mode restores the prior composer or question
/// snapshot when present; otherwise it rebuilds session view with any cached
/// focused review output for the same session.
pub(crate) fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if handle_line_comment_edit_key(app, key) {
        return EventResult::Continue;
    }

    if handle_help_key(app, key) {
        return EventResult::Continue;
    }

    if handle_exit_key(app, key) {
        return EventResult::Continue;
    }

    handle_navigation_key(app, render_cache_store, content_area, key);

    EventResult::Continue
}

/// Handles the only interactive action available while a full diff loads.
pub(crate) fn handle_loading(app: &mut App, key: KeyEvent) -> EventResult {
    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        app.cancel_diff_view_load();
    }

    EventResult::Continue
}

/// Opens diff help while preserving the current diff-mode snapshot.
fn handle_help_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code != KeyCode::Char('?') {
        return false;
    }

    let can_comment = can_reply_with_line_comments(app);
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments,
        restore,
        session_id,
        selected_diff_line_index,
        scroll_offset,
        ..
    } = mode
    {
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                can_comment,
                diff,
                file_explorer_selected_index,
                focus,
                line_comments,
                preview,
                review_comments: review_comments.map(Box::new),
                restore,
                selected_diff_line_index,
                session_id,
                scroll_offset,
            },
            scroll_offset: 0,
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Leaves diff mode and restores the originating view or question state.
fn handle_exit_key(app: &mut App, key: KeyEvent) -> bool {
    let should_exit = match key.code {
        KeyCode::Char('q') => true,
        KeyCode::Esc => !matches!(
            app.mode,
            AppMode::Diff {
                focus: DiffFocus::Content,
                ..
            }
        ),
        _ => false,
    };
    if !should_exit {
        return false;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        line_comments,
        restore,
        session_id,
        ..
    } = mode
    {
        app.save_diff_comment_progress(session_id.clone(), line_comments);
        app.mode = if let Some(restore) = restore {
            restore.into_mode()
        } else {
            AppMode::View {
                session_id,
                scroll_offset: None,
            }
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Applies file-selection and scroll navigation keys in diff mode.
fn handle_navigation_key(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) {
    let can_reply_with_line_comments = can_reply_with_line_comments(app);
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::Diff {
        diff,
        mut file_explorer_selected_index,
        mut focus,
        mut line_comments,
        mut preview,
        mut review_comments,
        restore,
        mut scroll_cache,
        mut scroll_offset,
        mut selected_diff_line_index,
        session_id,
    } = mode
    else {
        app.mode = mode;

        return;
    };

    let mut navigation = DiffKeyNavigation {
        diff: &diff,
        file_explorer_selected_index: &mut file_explorer_selected_index,
        focus: &mut focus,
        line_comments: &mut line_comments,
        preview: &mut preview,
        review_comments: &mut review_comments,
        scroll_cache: &mut scroll_cache,
        scroll_offset: &mut scroll_offset,
        selected_diff_line_index: &mut selected_diff_line_index,
        session_id: &session_id,
    };
    let row_selection_key_handled = handle_row_selection_key(
        render_cache_store,
        key,
        &mut navigation,
        can_reply_with_line_comments,
    );
    let line_comment_target = (!row_selection_key_handled)
        .then(|| {
            selected_line_comment_target(
                render_cache_store,
                key,
                &navigation,
                can_reply_with_line_comments,
            )
        })
        .flatten();
    let selection_changed = !row_selection_key_handled
        && apply_navigation_key(app, render_cache_store, content_area, key, &mut navigation);

    if selection_changed && preview.is_enabled() {
        refresh_selected_preview(
            app,
            render_cache_store,
            &diff,
            file_explorer_selected_index,
            &mut preview,
            &session_id,
        );
    }

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        review_comments,
        restore,
        scroll_cache,
        scroll_offset,
        selected_diff_line_index,
        session_id,
    };
    if let Some(target) = line_comment_target {
        start_line_comment_edit(app, render_cache_store, content_area, target);
    } else if should_submit_line_comments(app, key) {
        open_line_comment_prompt(app);
    }
}

/// Returns whether `s` requests submission of all completed diff comments.
pub(crate) fn should_submit_line_comments(app: &App, key: KeyEvent) -> bool {
    let AppMode::Diff { line_comments, .. } = &app.mode else {
        return false;
    };

    can_reply_with_line_comments(app)
        && is_plain_char_key(key, 's')
        && !line_comments.is_editing()
        && !line_comments.is_selecting()
        && !line_comments.comments.is_empty()
}

/// Handles `Shift+V` row-selection entry and `Esc` cancellation.
fn handle_row_selection_key(
    render_cache_store: &RenderCacheStore,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
    can_reply_with_line_comments: bool,
) -> bool {
    if navigation.line_comments.is_selecting() && key.code == KeyCode::Esc {
        navigation.line_comments.cancel_selection();

        return true;
    }
    let review_comments_are_focused = navigation
        .review_comments
        .as_ref()
        .is_some_and(|review_comments| review_comments.sidebar_focus == DiffSidebarFocus::Comments);
    if !can_reply_with_line_comments
        || !is_shift_char_key(key, 'v')
        || *navigation.focus != DiffFocus::Content
        || review_comments_are_focused
        || selected_preview_is_visible(
            navigation.diff,
            *navigation.file_explorer_selected_index,
            render_cache_store.diff_layout_cache(),
            navigation.preview,
        )
    {
        return false;
    }

    navigation
        .line_comments
        .start_selection(*navigation.selected_diff_line_index);

    true
}

/// Returns the selected file or changed rows requested for comment editing.
fn selected_line_comment_target(
    render_cache_store: &RenderCacheStore,
    key: KeyEvent,
    navigation: &DiffKeyNavigation<'_>,
    can_reply_with_line_comments: bool,
) -> Option<DiffCommentTarget> {
    let review_comments_are_focused = navigation
        .review_comments
        .as_ref()
        .is_some_and(|review_comments| review_comments.sidebar_focus == DiffSidebarFocus::Comments);
    if can_reply_with_line_comments && is_shift_char_key(key, 'c') && !review_comments_are_focused {
        return render_cache_store
            .diff_layout_cache()
            .content(navigation.diff)
            .selected_file_path(*navigation.file_explorer_selected_index)
            .map(DiffCommentTarget::file);
    }
    if !can_reply_with_line_comments
        || key.code != KeyCode::Enter
        || key.modifiers != KeyModifiers::NONE
        || *navigation.focus != DiffFocus::Content
        || review_comments_are_focused
        || selected_preview_is_visible(
            navigation.diff,
            *navigation.file_explorer_selected_index,
            render_cache_store.diff_layout_cache(),
            navigation.preview,
        )
    {
        return None;
    }

    if let Some(target) = navigation.line_comments.selected_comment_target() {
        return Some(target.clone());
    }

    let content = render_cache_store
        .diff_layout_cache()
        .content(navigation.diff);
    let (start_changed_line_index, end_changed_line_index) = navigation
        .line_comments
        .selected_row_bounds(*navigation.selected_diff_line_index);
    if start_changed_line_index == end_changed_line_index {
        return content
            .selected_changed_line(
                *navigation.file_explorer_selected_index,
                start_changed_line_index,
            )
            .map(DiffLineCommentTarget::single)
            .map(Into::into);
    }
    let anchors = content.selected_changed_lines(
        *navigation.file_explorer_selected_index,
        start_changed_line_index,
        end_changed_line_index,
    );

    DiffLineCommentTarget::from_anchors(anchors).map(Into::into)
}

/// Starts comment editing and keeps its file or changed rows visible.
fn start_line_comment_edit(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    target: impl Into<DiffCommentTarget>,
) {
    let target = target.into();
    let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        focus,
        line_comments,
        preview,
        scroll_cache,
        scroll_offset,
        selected_diff_line_index,
        ..
    } = &mut app.mode
    else {
        return;
    };

    let content = render_cache_store.diff_layout_cache().content(diff);
    let (comment_changed_line_index, select_changed_line) = match &target {
        DiffCommentTarget::File { path }
            if content.selected_file_path(*file_explorer_selected_index) == Some(path.as_str()) =>
        {
            line_comments.cancel_selection();

            (0, true)
        }
        DiffCommentTarget::File { .. } => return,
        DiffCommentTarget::Lines(target) => {
            let Some(comment_changed_line_index) = content
                .changed_line_index_for_anchor(*file_explorer_selected_index, target.last_anchor())
            else {
                return;
            };

            (comment_changed_line_index, false)
        }
    };
    let editing_index = line_comments.start_editing_target(target);
    *focus = DiffFocus::Content;
    *preview = preview.disabled();
    *scroll_cache = None;
    if select_changed_line {
        *selected_diff_line_index = comment_changed_line_index;
    }
    let layout = page::diff::diff_changed_line_layout(
        diff,
        line_comments,
        *file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    *scroll_offset = layout
        .content_selection_scroll_offset(
            comment_changed_line_index,
            Some(editing_index),
            *scroll_offset,
        )
        .unwrap_or(*scroll_offset);
}

/// Applies one key to the active diff comment editor before shortcuts run.
fn handle_line_comment_edit_key(app: &mut App, key: KeyEvent) -> bool {
    let AppMode::Diff {
        line_comments,
        scroll_cache,
        ..
    } = &mut app.mode
    else {
        return false;
    };
    if !line_comments.is_editing() {
        return false;
    }

    if !input_key::should_insert_newline(key)
        && let Some(state) = &mut line_comments.at_mention_state
        && let Some(input) = line_comments
            .editing_index
            .and_then(|index| line_comments.comments.get_mut(index))
            .map(|comment| &mut comment.input)
    {
        match key.code {
            KeyCode::Esc => line_comments.at_mention_state = None,
            KeyCode::Up => at_mention::move_selection_up(state),
            KeyCode::Down => at_mention::move_selection_down(input, state),
            KeyCode::Tab | KeyCode::Enter | KeyCode::Char('\r' | '\n') => {
                if let Some(selection) = at_mention::selected_replacement(input, state) {
                    input.replace_range(selection.at_start, selection.at_end, &selection.text);
                }
                line_comments.at_mention_state = None;
            }
            _ => return apply_comment_input_key(app, key),
        }

        return true;
    }

    if key.code == KeyCode::Esc
        || (input_key::is_enter_key(key.code) && !input_key::should_insert_newline(key))
    {
        let previous_count = line_comments.comments.len();
        line_comments.finish_editing();
        if line_comments.comments.len() != previous_count {
            *scroll_cache = None;
        }

        return true;
    }
    apply_comment_input_key(app, key)
}

/// Applies text editing before refreshing the repository lookup.
fn apply_comment_input_key(app: &mut App, key: KeyEvent) -> bool {
    if let AppMode::Diff { line_comments, .. } = &mut app.mode
        && let Some(command) =
            input_key::command_for_key(key, input_key::InputCapabilities::MULTILINE)
        && let Some(input) = line_comments.editing_input_mut()
    {
        input.apply(command);
        sync_comment_at_mention(app);
    }

    true
}

/// Refreshes lookup state after typing, cursor movement, or paste.
fn sync_comment_at_mention(app: &mut App) {
    let AppMode::Diff {
        line_comments,
        session_id,
        ..
    } = &mut app.mode
    else {
        return;
    };
    let Some(index) = line_comments.editing_index else {
        return;
    };
    match at_mention::sync_action(
        &line_comments.comments[index].input,
        line_comments.at_mention_state.as_deref(),
    ) {
        at_mention::AtMentionSyncAction::Dismiss => line_comments.at_mention_state = None,
        at_mention::AtMentionSyncAction::KeepOpen => {
            if let Some(state) = &mut line_comments.at_mention_state {
                at_mention::reset_selection(state);
            }
        }
        at_mention::AtMentionSyncAction::Activate => {
            line_comments.at_mention_state = Some(Box::new(PromptAtMentionState::new(Vec::new())));
            let session_id = session_id.clone();
            let lookup_root = app.at_mention_lookup_root(&session_id);
            at_mention::start_loading_entries(
                app.services.event_sender(),
                lookup_root,
                session_id,
                &mut app.sessions,
            );
        }
    }
}

/// Inserts normalized pasted text into the active multiline diff comment
/// editor.
pub(crate) fn handle_paste(app: &mut App, pasted_text: &str) {
    let AppMode::Diff { line_comments, .. } = &mut app.mode else {
        return;
    };
    let Some(input) = line_comments.editing_input_mut() else {
        return;
    };

    input.insert_text(&input_key::normalize_pasted_text(pasted_text));
    sync_comment_at_mention(app);
}

/// Replaces Diff mode with one next-turn prompt containing every comment.
fn open_line_comment_prompt(app: &mut App) {
    let (line_comments, restored_prompt, session_id) = match &app.mode {
        AppMode::Diff {
            line_comments,
            restore,
            session_id,
            ..
        } => {
            let restored_prompt = match restore.as_deref() {
                Some(DiffRestoreTarget::Prompt(snapshot)) => Some(snapshot.clone()),
                Some(DiffRestoreTarget::Question(_)) => return,
                None => None,
            };

            (line_comments.clone(), restored_prompt, session_id.clone())
        }
        _ => return,
    };
    let Some(session) = app.sessions.session_for_id(session_id.as_str()) else {
        return;
    };
    if !allows_diff_line_comment_reply(session, app.sessions.sessions(), None) {
        return;
    }
    let history_entries = super::session_view::session_prompt_history_entries(session);
    app.save_diff_comment_progress(session_id.clone(), line_comments.clone());

    let slash_state = app.prompt_slash_state();
    let mut snapshot = restored_prompt.unwrap_or_else(|| PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(history_entries),
        input: InputState::default(),
        scroll_offset: None,
        session_id,
        slash_state,
    });
    append_line_comments(&mut snapshot, &line_comments);
    app.mode = snapshot.into_prompt_mode();
}

/// Returns whether the active diff can collect comments for one agent reply.
fn can_reply_with_line_comments(app: &App) -> bool {
    let AppMode::Diff {
        restore,
        session_id,
        ..
    } = &app.mode
    else {
        return false;
    };
    let Some(session) = app.sessions.session_for_id(session_id.as_str()) else {
        return false;
    };

    allows_diff_line_comment_reply(session, app.sessions.sessions(), restore.as_deref())
}

/// Appends every structured line-comment block without losing image positions.
fn append_line_comments(snapshot: &mut PromptModeSnapshot, line_comments: &DiffLineComments) {
    let comment_blocks = line_comments.prompt_text();
    if comment_blocks.is_empty() {
        return;
    }
    let separator = if snapshot.input.is_empty() {
        ""
    } else {
        "\n\n"
    };
    let insertion = format!("{separator}{comment_blocks}");
    snapshot.input.move_end();
    let insertion_start = snapshot.input.cursor;
    snapshot
        .attachment_state
        .remember_current_revision(&snapshot.input);
    snapshot.input.insert_text(&insertion);
    snapshot.attachment_state.sync_after_edit(
        &snapshot.input,
        insertion_start,
        insertion_start,
        snapshot.input.cursor,
    );
    snapshot.at_mention_state = None;
    snapshot.history_state.reset_navigation();
    snapshot.slash_state.reset();
}

/// Mutable diff-mode values affected by one navigation key.
struct DiffKeyNavigation<'a> {
    diff: &'a str,
    file_explorer_selected_index: &'a mut usize,
    focus: &'a mut DiffFocus,
    line_comments: &'a mut DiffLineComments,
    preview: &'a mut DiffPreview,
    review_comments: &'a mut Option<DiffReviewComments>,
    scroll_cache: &'a mut Option<DiffScrollCache>,
    scroll_offset: &'a mut u16,
    selected_diff_line_index: &'a mut usize,
    session_id: &'a SessionId,
}

impl DiffKeyNavigation<'_> {
    /// Reborrows the right-pane subset used by focus and cursor helpers.
    fn content_navigation(&mut self) -> DiffContentNavigation<'_> {
        DiffContentNavigation {
            diff: self.diff,
            file_explorer_selected_index: *self.file_explorer_selected_index,
            focus: self.focus,
            line_comments: self.line_comments,
            preview: self.preview,
            review_comments_are_visible: self.review_comments.is_some(),
            scroll_cache: self.scroll_cache,
            scroll_offset: self.scroll_offset,
            selected_diff_line_index: self.selected_diff_line_index,
        }
    }
}

/// Applies one file-tree or right-pane navigation key.
fn apply_navigation_key(
    app: &App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
) -> bool {
    if apply_unfocused_scroll_key(render_cache_store, content_area, key, navigation) {
        return false;
    }

    match key.code {
        KeyCode::Char(character @ ('j' | 'k'))
            if *navigation.focus == DiffFocus::Files && is_plain_char_key(key, character) =>
        {
            let content = render_cache_store
                .diff_layout_cache()
                .content(navigation.diff);
            let new_index = selected_index_after_key(
                character,
                *navigation.file_explorer_selected_index,
                content.item_count(),
            );
            if *navigation.file_explorer_selected_index != new_index {
                *navigation.file_explorer_selected_index = new_index;
                navigation.line_comments.clear_comment_selection();
                *navigation.scroll_cache = None;
                *navigation.scroll_offset = 0;
                *navigation.selected_diff_line_index = 0;

                return true;
            }
        }
        KeyCode::Enter | KeyCode::Char('l')
            if *navigation.focus == DiffFocus::Files
                && (key.code == KeyCode::Enter || is_plain_char_key(key, 'l')) =>
        {
            let mut content_navigation = navigation.content_navigation();
            focus_selected_file_changes(&mut content_navigation, content_area, render_cache_store);
        }
        KeyCode::Down | KeyCode::Char('J' | 'j')
            if *navigation.focus == DiffFocus::Content
                && is_content_navigation_key(key, KeyCode::Down, 'j') =>
        {
            let mut content_navigation = navigation.content_navigation();
            move_content_selection(
                &mut content_navigation,
                content_area,
                render_cache_store,
                DiffContentDirection::Next,
            );
        }
        KeyCode::Up | KeyCode::Char('K' | 'k')
            if *navigation.focus == DiffFocus::Content
                && is_content_navigation_key(key, KeyCode::Up, 'k') =>
        {
            let mut content_navigation = navigation.content_navigation();
            move_content_selection(
                &mut content_navigation,
                content_area,
                render_cache_store,
                DiffContentDirection::Previous,
            );
        }
        KeyCode::Esc | KeyCode::Left if *navigation.focus == DiffFocus::Content => {
            navigation.line_comments.cancel_selection();
            *navigation.focus = DiffFocus::Files;
        }
        KeyCode::Char(character @ ('f' | 'h'))
            if *navigation.focus == DiffFocus::Content && is_plain_char_key(key, character) =>
        {
            navigation.line_comments.cancel_selection();
            *navigation.focus = DiffFocus::Files;
        }
        KeyCode::Char('p')
            if *navigation.focus == DiffFocus::Files && is_plain_char_key(key, 'p') =>
        {
            if let Some(updated_preview) = toggle_selected_preview(
                app,
                render_cache_store.diff_layout_cache(),
                navigation.diff,
                *navigation.file_explorer_selected_index,
                navigation.preview,
                navigation.session_id,
            ) {
                *navigation.preview = updated_preview;
                *navigation.scroll_cache = None;
                *navigation.scroll_offset = 0;
            }
        }
        KeyCode::Char('c')
            if *navigation.focus == DiffFocus::Files
                && is_plain_char_key(key, 'c')
                && navigation.review_comments.is_some() =>
        {
            *navigation.focus = DiffFocus::Files;
            focus_review_comments(
                navigation.review_comments,
                navigation.scroll_cache,
                navigation.scroll_offset,
            );
        }
        _ => {}
    }

    false
}

/// Applies a row-scroll key without moving focus out of the Files pane.
fn apply_unfocused_scroll_key(
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
    navigation: &mut DiffKeyNavigation<'_>,
) -> bool {
    if *navigation.focus != DiffFocus::Files {
        return false;
    }
    let direction = match key.code {
        KeyCode::Down | KeyCode::Char('J' | 'j')
            if is_unfocused_scroll_key(key, KeyCode::Down, 'j') =>
        {
            DiffContentDirection::Next
        }
        KeyCode::Up | KeyCode::Char('K' | 'k')
            if is_unfocused_scroll_key(key, KeyCode::Up, 'k') =>
        {
            DiffContentDirection::Previous
        }
        _ => return false,
    };
    let mut content_navigation = navigation.content_navigation();
    scroll_content_by_row(
        &mut content_navigation,
        content_area,
        render_cache_store,
        direction,
    );

    true
}

/// Returns whether a key scrolls the right pane while Files stays focused.
fn is_unfocused_scroll_key(key: KeyEvent, arrow_key: KeyCode, character: char) -> bool {
    key.code == arrow_key || is_shift_char_key(key, character)
}

/// Returns whether a key moves through the active right-hand content pane.
fn is_content_navigation_key(key: KeyEvent, arrow_key: KeyCode, character: char) -> bool {
    if key.code == arrow_key {
        return true;
    }
    if is_plain_char_key(key, character) {
        return true;
    }

    is_shift_char_key(key, character)
}

/// Mutable diff-pane navigation values shared by focus and cursor movement.
struct DiffContentNavigation<'a> {
    diff: &'a str,
    file_explorer_selected_index: usize,
    focus: &'a mut DiffFocus,
    line_comments: &'a mut DiffLineComments,
    preview: &'a DiffPreview,
    review_comments_are_visible: bool,
    scroll_cache: &'a mut Option<DiffScrollCache>,
    scroll_offset: &'a mut u16,
    selected_diff_line_index: &'a mut usize,
}

/// Direction in which the right-hand diff cursor moves.
#[derive(Clone, Copy)]
enum DiffContentDirection {
    Next,
    Previous,
}

/// Moves focus into a file's preview or first visible added/removed line.
fn focus_selected_file_changes(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
) {
    let content = render_cache_store
        .diff_layout_cache()
        .content(navigation.diff);
    let preview_is_visible = selected_preview_is_visible(
        navigation.diff,
        navigation.file_explorer_selected_index,
        render_cache_store.diff_layout_cache(),
        navigation.preview,
    );
    if !content.selected_item_is_file(navigation.file_explorer_selected_index) {
        return;
    }

    if preview_is_visible {
        *navigation.focus = DiffFocus::Content;
        *navigation.selected_diff_line_index = 0;
        navigation.line_comments.clear_comment_selection();

        return;
    }
    let changed_line_layout = page::diff::diff_changed_line_layout(
        navigation.diff,
        navigation.line_comments,
        navigation.file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    let page_areas = diff_util::diff_page_areas(content_area);
    let sidebar_areas = diff_util::diff_sidebar_areas(
        page_areas.file_list_area,
        navigation.review_comments_are_visible,
    );
    let selected_visual_row = FileExplorer::selected_visual_row(
        navigation.file_explorer_selected_index,
        content.item_count(),
        sidebar_areas.file_list_area,
    )
    .unwrap_or_default();
    let Some(selected_diff_line_index) = changed_line_layout
        .changed_line_index_at_visual_row(*navigation.scroll_offset, selected_visual_row)
    else {
        return;
    };

    *navigation.focus = DiffFocus::Content;
    *navigation.selected_diff_line_index = selected_diff_line_index;
    navigation.line_comments.clear_comment_selection();
    *navigation.scroll_offset = changed_line_layout
        .changed_line_scroll_offset(
            *navigation.selected_diff_line_index,
            *navigation.scroll_offset,
        )
        .unwrap_or(*navigation.scroll_offset);
}

/// Moves through preview rows or added/removed lines in the active file.
fn move_content_selection(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
    direction: DiffContentDirection,
) {
    if selected_preview_is_visible(
        navigation.diff,
        navigation.file_explorer_selected_index,
        render_cache_store.diff_layout_cache(),
        navigation.preview,
    ) {
        scroll_content_by_row(navigation, content_area, render_cache_store, direction);

        return;
    }

    let changed_line_layout = page::diff::diff_changed_line_layout(
        navigation.diff,
        navigation.line_comments,
        navigation.file_explorer_selected_index,
        content_area,
        render_cache_store.diff_layout_cache(),
    );
    let selected_comment_index = navigation.line_comments.selected_comment_index();
    let (selected_diff_line_index, selected_comment_index) =
        if navigation.line_comments.is_selecting() {
            let line_count = changed_line_layout.changed_line_count();
            let selected_diff_line_index = match direction {
                DiffContentDirection::Next => (*navigation.selected_diff_line_index)
                    .saturating_add(1)
                    .min(line_count.saturating_sub(1)),
                DiffContentDirection::Previous => {
                    (*navigation.selected_diff_line_index).saturating_sub(1)
                }
            };

            (selected_diff_line_index, None)
        } else {
            match direction {
                DiffContentDirection::Next => changed_line_layout.next_content_selection(
                    *navigation.selected_diff_line_index,
                    selected_comment_index,
                ),
                DiffContentDirection::Previous => changed_line_layout.previous_content_selection(
                    *navigation.selected_diff_line_index,
                    selected_comment_index,
                ),
            }
        };
    *navigation.selected_diff_line_index = selected_diff_line_index;
    if let Some(comment_index) = selected_comment_index {
        navigation.line_comments.select_comment(comment_index);
    } else {
        navigation.line_comments.clear_comment_selection();
    }
    *navigation.scroll_offset = changed_line_layout
        .content_selection_scroll_offset(
            *navigation.selected_diff_line_index,
            selected_comment_index,
            *navigation.scroll_offset,
        )
        .unwrap_or(*navigation.scroll_offset);
}

/// Scrolls the selected file or preview by one rendered row without changing
/// pane focus or the selected changed-line cursor.
fn scroll_content_by_row(
    navigation: &mut DiffContentNavigation<'_>,
    content_area: Rect,
    render_cache_store: &RenderCacheStore,
    direction: DiffContentDirection,
) {
    let max_scroll_offset = diff_max_scroll_offset(
        &DiffScrollLimitInput {
            content_area,
            diff: navigation.diff,
            diff_layout_cache: render_cache_store.diff_layout_cache(),
            line_comments: navigation.line_comments,
            markdown_render_cache: render_cache_store.markdown_render_cache(),
            preview: navigation.preview,
            selected_index: navigation.file_explorer_selected_index,
        },
        navigation.scroll_cache,
    );
    *navigation.scroll_offset = match direction {
        DiffContentDirection::Next => (*navigation.scroll_offset)
            .min(max_scroll_offset)
            .saturating_add(1)
            .min(max_scroll_offset),
        DiffContentDirection::Previous => (*navigation.scroll_offset)
            .min(max_scroll_offset)
            .saturating_sub(1),
    };
}

/// Returns whether the active selection is currently showing markdown preview
/// content or an availability notice instead of raw diff lines.
fn selected_preview_is_visible(
    diff: &str,
    selected_file_index: usize,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    preview: &DiffPreview,
) -> bool {
    let Some(preview_path) = preview.path() else {
        return false;
    };
    let content = diff_layout_cache.content(diff);

    content.selected_markdown_path(selected_file_index) == Some(preview_path)
}

fn focus_review_comments(
    review_comments: &mut Option<DiffReviewComments>,
    scroll_cache: &mut Option<DiffScrollCache>,
    scroll_offset: &mut u16,
) {
    if let Some(review_comments) = review_comments {
        review_comments.sidebar_focus = DiffSidebarFocus::Comments;
    }
    *scroll_cache = None;
    *scroll_offset = 0;
}

fn refresh_selected_preview(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    diff: &str,
    file_explorer_selected_index: usize,
    preview: &mut DiffPreview,
    session_id: &SessionId,
) {
    *preview = start_selected_preview_load(
        app,
        render_cache_store.diff_layout_cache(),
        diff,
        file_explorer_selected_index,
        preview,
        session_id,
    );
}

/// Returns the wrapped explorer selection for a plain `j` or `k` key.
fn selected_index_after_key(character: char, current_index: usize, item_count: usize) -> usize {
    if character == 'j' {
        return FileExplorer::next_selected_index(current_index, item_count);
    }

    FileExplorer::previous_selected_index(current_index, item_count)
}

/// Toggles preview for the selected row, ignoring unsupported toggle-on keys.
fn toggle_selected_preview(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> Option<DiffPreview> {
    if preview.is_enabled() {
        return Some(preview.disabled());
    }
    selected_markdown_path(diff, selected_index, diff_layout_cache)?;

    Some(start_selected_preview_load(
        app,
        diff_layout_cache,
        diff,
        selected_index,
        preview,
        session_id,
    ))
}

/// Returns the selected markdown path from the cached diff tree.
fn selected_markdown_path(
    diff: &str,
    selected_index: usize,
    diff_layout_cache: &page::diff::DiffLayoutCache,
) -> Option<String> {
    diff_layout_cache
        .content(diff)
        .selected_markdown_path(selected_index)
        .map(str::to_string)
}

/// Starts a bounded background read for the active markdown selection.
fn start_selected_preview_load(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> DiffPreview {
    let request_id = preview.next_request_id();
    let Some(path) = selected_markdown_path(diff, selected_index, diff_layout_cache) else {
        return DiffPreview::Unsupported { request_id };
    };
    let Some(session_folder) = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| session.folder.clone())
    else {
        return DiffPreview::Unavailable {
            path,
            reason: DiffPreviewUnavailableReason::LoadFailed(
                "Session worktree is unavailable".to_string(),
            ),
            request_id,
        };
    };

    let event_sender = app.services.event_sender();
    let git_client = app.services.git_client();
    let loaded_path = path.clone();
    let loaded_session_id = session_id.into();
    tokio::spawn(async move {
        let result = git_client
            .read_worktree_file(session_folder, loaded_path.clone())
            .await
            .map_err(|error| error.to_string());
        let _ = event_sender.send(AppEvent::DiffPreviewLoaded {
            path: loaded_path,
            request_id,
            result,
            session_id: loaded_session_id,
        });
    });

    DiffPreview::Loading { path, request_id }
}

/// Returns true when the key event is a plain character key with no
/// modifiers.
fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::NONE
}

/// Returns true when the key event is a shifted character key, accepting both
/// uppercase and lowercase char payloads emitted by terminals.
fn is_shift_char_key(key: KeyEvent, character: char) -> bool {
    let lowercase_character = character.to_ascii_lowercase();
    let uppercase_character = character.to_ascii_uppercase();

    key.modifiers == KeyModifiers::SHIFT
        && matches!(
            key.code,
            KeyCode::Char(pressed)
                if pressed == lowercase_character || pressed == uppercase_character
        )
}

/// Inputs used to resolve and cache the active diff scroll limit.
struct DiffScrollLimitInput<'a> {
    content_area: Rect,
    diff: &'a str,
    diff_layout_cache: &'a page::diff::DiffLayoutCache,
    line_comments: &'a DiffLineComments,
    markdown_render_cache: &'a crate::ui::markdown::MarkdownRenderCache,
    preview: &'a DiffPreview,
    selected_index: usize,
}

/// Returns the max valid scroll offset for the active diff selection.
fn diff_max_scroll_offset(
    input: &DiffScrollLimitInput<'_>,
    scroll_cache: &mut Option<DiffScrollCache>,
) -> u16 {
    if let Some(cached_scroll_limit) = scroll_cache
        && cached_scroll_limit.content_area == viewport_rect(input.content_area)
        && cached_scroll_limit.file_explorer_selected_index == input.selected_index
    {
        return cached_scroll_limit.max_scroll_offset;
    }

    let max_scroll_offset = page::diff::diff_view_max_scroll_offset(
        input.diff,
        input.line_comments,
        input.selected_index,
        input.content_area,
        input.diff_layout_cache,
        input.markdown_render_cache,
        input.preview,
    );

    *scroll_cache = Some(DiffScrollCache {
        content_area: viewport_rect(input.content_area),
        file_explorer_selected_index: input.selected_index,
        max_scroll_offset,
    });

    max_scroll_offset
}

/// Converts terminal geometry into the frontend-neutral cached viewport key.
fn viewport_rect(content_area: Rect) -> ViewportRect {
    ViewportRect {
        height: content_area.height,
        width: content_area.width,
        x: content_area.x,
        y: content_area.y,
    }
}

#[cfg(test)]
#[path = "diff_test.rs"]
pub(crate) mod tests;

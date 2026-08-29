//! Shared prompt-composer state and pure helpers for slash suggestions,
//! inline attachment placeholders, and prompt submission payloads.

use std::path::PathBuf;

pub use ag_protocol::render_prompt_text_for_agent;

use crate::domain::agent::{
    self, AgentKind, AgentSelection, AgentSelectionMetadata, ReasoningLevel, ResponseStyle,
    SpeedMode,
};
use crate::domain::input::InputState;
use crate::domain::permission::PermissionMode;
use crate::domain::personality::PersonalitySummary;

/// One selectable row in the prompt slash-command menu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestionItem {
    /// Optional compact badge rendered before the main label.
    pub badge: Option<String>,
    /// Optional explanatory text rendered after the label.
    pub detail: Option<String>,
    /// Primary row label used for selection and insertion.
    pub label: String,
    /// Optional trailing metadata rendered with subdued styling.
    pub metadata: Option<String>,
}

/// Render-ready prompt suggestion dropdown state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestionList {
    /// Dropdown rows in display order.
    pub items: Vec<PromptSuggestionItem>,
    /// Highlighted row index in `items`.
    pub selected_index: usize,
    /// Dropdown title shown in the border chrome.
    pub title: String,
}

/// Semantic action represented by the currently highlighted prompt slash item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptSuggestionSelection {
    /// Slash command selected from the first stage.
    Command(&'static str),
    /// Agent selected during `/model` agent selection.
    Agent(AgentKind),
    /// Agent and model selected during `/model` model selection.
    Model(AgentSelection),
    /// Session mode selected during `/mode` selection.
    Mode(PermissionMode),
    /// Workspace personality selected or cleared during `/personality`.
    Personality(Option<PersonalitySummary>),
    /// Session-scoped reasoning selection chosen during `/reasoning`.
    Reasoning(ReasoningLevel),
    /// Session-scoped response style chosen during `/style`.
    Style(ResponseStyle),
    /// Session-scoped response-speed selection chosen during `/speed`.
    Speed(SpeedMode),
}

/// Concrete character location owned by an attachment in one input revision.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AttachmentRevision {
    revision: u64,
    start: usize,
}

/// Inline attachment metadata for one pasted local image placeholder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachment {
    /// Stable display number shown inside the inline `[Image #n]` token.
    pub attachment_number: usize,
    /// Local image path that will later be handed off to runtime transport.
    pub local_image_path: PathBuf,
    /// Placeholder token inserted into the prompt composer text.
    pub placeholder: String,
    current_start: Option<usize>,
    valid_locations: Vec<AttachmentRevision>,
}

impl PromptAttachment {
    /// Creates attachment metadata for one pasted local image.
    #[must_use]
    pub fn new(attachment_number: usize, local_image_path: PathBuf) -> Self {
        Self {
            attachment_number,
            current_start: None,
            local_image_path,
            placeholder: Self::placeholder_for(attachment_number),
            valid_locations: Vec::new(),
        }
    }

    /// Builds the inline placeholder token for one attachment number.
    #[must_use]
    pub fn placeholder_for(attachment_number: usize) -> String {
        format!("[Image #{attachment_number}]")
    }

    /// Records the concrete placeholder occurrence owned by this attachment
    /// in one input revision.
    fn remember_revision(&mut self, revision: u64) {
        let Some(start) = self.current_start else {
            return;
        };
        if !self
            .valid_locations
            .iter()
            .any(|location| location.revision == revision)
        {
            self.valid_locations
                .push(AttachmentRevision { revision, start });
        }
    }

    /// Restores the concrete placeholder occurrence owned in `revision`.
    fn restore_revision(&mut self, revision: u64) {
        self.current_start = self
            .valid_locations
            .iter()
            .find(|location| location.revision == revision)
            .map(|location| location.start);
    }

    /// Updates the owned placeholder occurrence across one text edit.
    fn apply_edit(&mut self, old_start: usize, old_end: usize, new_end: usize) {
        let Some(current_start) = self.current_start else {
            return;
        };
        let current_end = current_start + self.placeholder.chars().count();

        if old_end <= current_start {
            let removed_length = old_end - old_start;
            let inserted_length = new_end - old_start;
            self.current_start = Some(if inserted_length >= removed_length {
                current_start + inserted_length - removed_length
            } else {
                current_start - (removed_length - inserted_length)
            });
        } else if old_start < current_end {
            self.current_start = None;
        }
    }
}

/// Attachment-only snapshot drained from the prompt composer during submit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptComposerSubmission {
    /// Attachments that still appear in the submitted prompt text.
    pub attachments: Vec<PromptAttachment>,
    /// Submitted prompt text after draining the input buffer.
    pub text: String,
}

impl PromptComposerSubmission {
    /// Returns whether both the text and attachment list are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }
}

/// UI state for pasted local-image attachments in prompt mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAttachmentState {
    /// Deleted attachments retained while input undo history can restore their
    /// placeholders.
    pub archived_attachments: Vec<PromptAttachment>,
    /// Attachments in the same order their placeholders were inserted.
    pub attachments: Vec<PromptAttachment>,
    /// Next placeholder number that should be assigned to a pasted image.
    pub next_attachment_number: usize,
}

impl PromptAttachmentState {
    /// Registers a pasted local image and returns the placeholder inserted
    /// into the prompt input text.
    pub fn register_local_image(
        &mut self,
        local_image_path: PathBuf,
        placeholder_start: usize,
    ) -> String {
        let mut attachment = PromptAttachment::new(self.next_attachment_number, local_image_path);
        attachment.current_start = Some(placeholder_start);
        let placeholder = attachment.placeholder.clone();

        self.attachments.push(attachment);
        self.refresh_next_attachment_number();

        placeholder
    }

    /// Returns attachment metadata for the given inline placeholder token.
    #[must_use]
    pub fn attachment_for_placeholder(&self, placeholder: &str) -> Option<&PromptAttachment> {
        self.attachments
            .iter()
            .find(|attachment| attachment.placeholder == placeholder)
    }

    /// Recomputes the next placeholder number after all active and archived
    /// attachment numbers.
    pub fn refresh_next_attachment_number(&mut self) {
        self.next_attachment_number = self
            .attachments
            .iter()
            .chain(&self.archived_attachments)
            .map(|attachment| attachment.attachment_number)
            .max()
            .unwrap_or_default()
            .saturating_add(1);
    }

    /// Associates currently active attachments with the current input
    /// revision before a text mutation changes it.
    pub fn remember_current_revision(&mut self, input: &InputState) {
        for attachment in &mut self.attachments {
            attachment.remember_revision(input.revision());
        }
    }

    /// Reconciles concrete attachment occurrences after one normal text edit
    /// without activating archived images from lookalike placeholder text.
    pub fn sync_after_edit(
        &mut self,
        input: &InputState,
        old_start: usize,
        old_end: usize,
        new_end: usize,
    ) {
        for attachment in &mut self.attachments {
            attachment.apply_edit(old_start, old_end, new_end);
        }

        let (mut active, removed): (Vec<_>, Vec<_>) = std::mem::take(&mut self.attachments)
            .into_iter()
            .partition(|attachment| attachment.current_start.is_some());
        for attachment in &mut active {
            attachment.remember_revision(input.revision());
        }

        self.attachments = active;
        self.archived_attachments.extend(removed);
        self.archived_attachments
            .sort_by_key(|attachment| attachment.attachment_number);
    }

    /// Restores attachment membership for the exact input revision reached
    /// through undo or redo.
    pub fn sync_after_history_restore(&mut self, input: &InputState) {
        let mut tracked_attachments = std::mem::take(&mut self.attachments);
        tracked_attachments.append(&mut self.archived_attachments);
        tracked_attachments.sort_by_key(|attachment| attachment.attachment_number);

        for attachment in &mut tracked_attachments {
            attachment.restore_revision(input.revision());
        }

        (self.attachments, self.archived_attachments) = tracked_attachments
            .into_iter()
            .partition(|attachment| attachment.current_start.is_some());
    }

    /// Archives every current attachment while prompt-history navigation is
    /// showing a previously submitted text entry.
    pub fn archive_current(&mut self) {
        for attachment in &mut self.attachments {
            attachment.current_start = None;
        }
        self.archived_attachments.append(&mut self.attachments);
        self.archived_attachments
            .sort_by_key(|attachment| attachment.attachment_number);
    }

    /// Restores draft attachment occurrences from `draft_revision` into the
    /// input's new current revision after prompt-history navigation.
    pub fn restore_draft_revision(&mut self, draft_revision: u64, input: &InputState) {
        let mut tracked_attachments = std::mem::take(&mut self.attachments);
        tracked_attachments.append(&mut self.archived_attachments);
        tracked_attachments.sort_by_key(|attachment| attachment.attachment_number);
        for attachment in &mut tracked_attachments {
            attachment.restore_revision(draft_revision);
            attachment.remember_revision(input.revision());
        }

        (self.attachments, self.archived_attachments) = tracked_attachments
            .into_iter()
            .partition(|attachment| attachment.current_start.is_some());
    }

    /// Drops archived attachments whose valid revisions have all fallen out
    /// of bounded input undo/redo history and returns them for file cleanup.
    pub fn prune_unreachable(&mut self, input: &InputState) -> Vec<PromptAttachment> {
        for attachment in self
            .attachments
            .iter_mut()
            .chain(&mut self.archived_attachments)
        {
            attachment
                .valid_locations
                .retain(|location| input.retains_revision(location.revision));
        }

        let (retained, unreachable) = std::mem::take(&mut self.archived_attachments)
            .into_iter()
            .partition(|attachment| !attachment.valid_locations.is_empty());
        self.archived_attachments = retained;

        unreachable
    }

    /// Clears all tracked attachments and resets numbering back to the first
    /// placeholder.
    pub fn reset(&mut self) {
        self.archived_attachments.clear();
        self.attachments.clear();
        self.next_attachment_number = 1;
    }
}

impl Default for PromptAttachmentState {
    /// Creates empty prompt attachment state with attachment numbering
    /// starting at 1.
    fn default() -> Self {
        Self {
            archived_attachments: Vec::new(),
            attachments: Vec::new(),
            next_attachment_number: 1,
        }
    }
}

/// UI state for navigating previously sent prompts with `Up` and `Down`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistoryState {
    /// Input revision containing the attachment membership of `draft_text`.
    pub draft_input_revision: Option<u64>,
    /// Draft input captured before entering history navigation.
    pub draft_text: Option<String>,
    /// Previously sent user prompts in chronological order.
    pub entries: Vec<String>,
    /// Currently selected history entry index, if any.
    pub selected_index: Option<usize>,
}

impl PromptHistoryState {
    /// Creates history state from prior prompt entries.
    #[must_use]
    pub fn new(entries: Vec<String>) -> Self {
        Self {
            draft_input_revision: None,
            draft_text: None,
            entries,
            selected_index: None,
        }
    }

    /// Clears active history navigation and stored draft text.
    pub fn reset_navigation(&mut self) {
        self.draft_input_revision = None;
        self.draft_text = None;
        self.selected_index = None;
    }
}

/// Steps in prompt slash command selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptSlashStage {
    /// Selecting the agent for the current slash command.
    Agent,
    /// Selecting the slash command itself.
    Command,
    /// Selecting a model after choosing an agent.
    Model,
    /// Selecting a session permission and automation mode.
    Mode,
    /// Selecting or clearing a workspace personality.
    Personality,
    /// Selecting a session-specific reasoning level override.
    Reasoning,
    /// Selecting a session-specific response style.
    Style,
    /// Selecting a session-specific response-speed preference.
    Speed,
}

/// UI state for prompt-only slash command selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PromptSlashState {
    /// Agent kinds currently runnable on this machine for `/model`.
    pub available_agent_kinds: Vec<AgentKind>,
    /// Workspace personalities loaded when `/personality` was accepted.
    pub personalities: Vec<PersonalitySummary>,
    /// Agent selected for the current slash workflow, when applicable.
    pub selected_agent: Option<AgentKind>,
    /// Highlighted option inside the active slash menu.
    pub selected_index: usize,
    /// Active slash-command selection stage.
    pub stage: PromptSlashStage,
}

impl PromptSlashState {
    /// Creates a new slash state scoped to the provided locally available
    /// agent kinds.
    #[must_use]
    pub fn with_available_agent_kinds(available_agent_kinds: Vec<AgentKind>) -> Self {
        Self {
            available_agent_kinds,
            personalities: Vec::new(),
            selected_agent: None,
            selected_index: 0,
            stage: PromptSlashStage::Command,
        }
    }

    /// Replaces the locally available agent kinds while keeping prompt slash
    /// selection state coherent.
    pub fn replace_available_agent_kinds(&mut self, available_agent_kinds: Vec<AgentKind>) {
        self.available_agent_kinds = available_agent_kinds;

        if self
            .selected_agent
            .is_some_and(|selected_agent| !self.available_agent_kinds.contains(&selected_agent))
        {
            self.selected_agent = None;
            self.selected_index = 0;

            if matches!(self.stage, PromptSlashStage::Model) {
                self.stage = PromptSlashStage::Agent;
            }
        }
    }

    /// Resets slash state back to command selection.
    pub fn reset(&mut self) {
        self.selected_agent = None;
        self.personalities.clear();
        self.selected_index = 0;
        self.stage = PromptSlashStage::Command;
    }
}

impl Default for PromptSlashState {
    /// Creates a slash state at command selection with every agent kind
    /// marked available.
    fn default() -> Self {
        Self::with_available_agent_kinds(AgentKind::ALL.to_vec())
    }
}

/// Full prompt composer state for one session prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptComposerState {
    /// Ordered local image attachments referenced by inline placeholders in
    /// `input`.
    pub attachment_state: PromptAttachmentState,
    /// Prompt-history navigation state for `Up` and `Down`.
    pub history_state: PromptHistoryState,
    /// Editable prompt text, including inline attachment placeholders.
    pub input: InputState,
    /// Slash-command selection state for the current prompt input.
    pub slash_state: PromptSlashState,
}

impl PromptComposerState {
    /// Creates a prompt composer with empty input and prompt history.
    #[must_use]
    pub fn new(available_agent_kinds: Vec<AgentKind>) -> Self {
        Self::with_input_and_history(InputState::default(), available_agent_kinds, Vec::new())
    }

    /// Creates a prompt composer with explicit input and history snapshots.
    #[must_use]
    pub fn with_input_and_history(
        input: InputState,
        available_agent_kinds: Vec<AgentKind>,
        history_entries: Vec<String>,
    ) -> Self {
        Self {
            attachment_state: PromptAttachmentState::default(),
            history_state: PromptHistoryState::new(history_entries),
            input,
            slash_state: PromptSlashState::with_available_agent_kinds(available_agent_kinds),
        }
    }

    /// Returns whether the composer currently starts with a slash command.
    #[must_use]
    pub fn is_slash_command(&self) -> bool {
        self.input.text().starts_with('/')
    }

    /// Builds the render-ready prompt slash suggestion list for the current
    /// input and slash stage.
    #[must_use]
    pub fn slash_suggestion_list(
        &self,
        session_agent_kind: AgentKind,
    ) -> Option<PromptSuggestionList> {
        build_prompt_slash_suggestion_list(
            self.input.text(),
            &self.slash_state,
            session_agent_kind,
            true,
        )
    }

    /// Resolves the semantic action behind the currently highlighted prompt
    /// slash item.
    #[must_use]
    pub fn selected_slash_action(
        &self,
        session_agent_kind: AgentKind,
    ) -> Option<PromptSuggestionSelection> {
        resolve_prompt_slash_selection(
            self.input.text(),
            &self.slash_state,
            session_agent_kind,
            true,
        )
    }

    /// Inserts pasted prompt text by delegating to the canonical field-level
    /// helper and clears any transient slash/history navigation state.
    pub fn insert_text(&mut self, text: &str) {
        self.attachment_state.remember_current_revision(&self.input);
        let insert_start = self.input.cursor;
        insert_prompt_text(
            &mut self.input,
            &mut self.history_state,
            &mut self.slash_state,
            text,
        );
        self.attachment_state.sync_after_edit(
            &self.input,
            insert_start,
            insert_start,
            self.input.cursor,
        );
    }

    /// Inserts one typed character by delegating to the canonical field-level
    /// helper and clears transient slash/history state.
    pub fn insert_char(&mut self, character: char) {
        self.attachment_state.remember_current_revision(&self.input);
        let insert_start = self.input.cursor;
        insert_prompt_character(
            &mut self.input,
            &mut self.history_state,
            &mut self.slash_state,
            character,
        );
        self.attachment_state.sync_after_edit(
            &self.input,
            insert_start,
            insert_start,
            self.input.cursor,
        );
    }

    /// Registers one pasted image by delegating to the canonical field-level
    /// helper, inserts its placeholder into the prompt, and clears transient
    /// slash/history state.
    pub fn insert_local_image(&mut self, local_image_path: PathBuf) {
        insert_prompt_local_image(
            &mut self.attachment_state,
            &mut self.history_state,
            &mut self.input,
            &mut self.slash_state,
            local_image_path,
        );
    }

    /// Applies a prompt deletion range by delegating to the canonical
    /// field-level helper, expanding it to whole image placeholders and
    /// pruning orphaned attachment metadata.
    pub fn delete_range(&mut self, start: usize, end: usize) {
        apply_prompt_delete_range(
            &mut self.attachment_state,
            &mut self.history_state,
            &mut self.input,
            &mut self.slash_state,
            start,
            end,
        );
    }

    /// Drains the prompt composer by delegating to the canonical field-level
    /// helper, returning text plus attachment metadata suitable for runtime
    /// turn submission.
    pub fn take_submission(&mut self) -> PromptComposerSubmission {
        drain_prompt_submission(&mut self.attachment_state, &mut self.input)
    }
}

impl Default for PromptComposerState {
    fn default() -> Self {
        Self::new(AgentKind::ALL.to_vec())
    }
}

/// Returns the number of selectable options in the active slash stage.
///
/// `allow_apply_command` controls whether `/apply` participates in the
/// command-stage list so navigation counts match the visible slash menu.
#[must_use]
pub fn prompt_slash_option_count(
    input: &str,
    stage: PromptSlashStage,
    selected_agent: Option<AgentKind>,
    available_agent_kinds: &[AgentKind],
    personalities: &[PersonalitySummary],
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> usize {
    build_prompt_slash_suggestion_list(
        input,
        &PromptSlashState {
            available_agent_kinds: available_agent_kinds.to_vec(),
            personalities: personalities.to_vec(),
            selected_agent,
            selected_index: 0,
            stage,
        },
        session_agent_kind,
        allow_apply_command,
    )
    .map_or(0, |suggestion_list| suggestion_list.items.len())
}

/// Returns the character range deleted by one current-line delete action.
#[must_use]
pub fn current_line_delete_range(input: &InputState) -> Option<(usize, usize)> {
    let characters: Vec<char> = input.text().chars().collect();
    if characters.is_empty() {
        return None;
    }

    let cursor = input.cursor.min(characters.len());
    let mut line_start = cursor;
    while line_start > 0 && characters[line_start - 1] != '\n' {
        line_start -= 1;
    }

    let mut line_end = cursor;
    while line_end < characters.len() && characters[line_end] != '\n' {
        line_end += 1;
    }

    let delete_range = if line_start > 0 {
        (line_start - 1, line_end)
    } else if line_end < characters.len() {
        (line_start, line_end + 1)
    } else {
        (line_start, line_end)
    };

    if delete_range.0 == delete_range.1 {
        return None;
    }

    Some(delete_range)
}

/// Expands one deletion range to cover any overlapping `[Image #n]`
/// placeholders so partial token edits remove the whole placeholder.
#[must_use]
pub fn expand_delete_range_to_image_tokens(text: &str, start: usize, end: usize) -> (usize, usize) {
    let mut expanded_start = start;
    let mut expanded_end = end;

    for (token_start, token_end, _) in image_token_ranges(text) {
        if token_start < expanded_end && expanded_start < token_end {
            expanded_start = expanded_start.min(token_start);
            expanded_end = expanded_end.max(token_end);
        }
    }

    (expanded_start, expanded_end)
}

/// Returns all valid `[Image #n]` placeholder token ranges in `text`.
#[must_use]
pub fn image_token_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        if let Some(end_index) = image_token_end_index(&characters, index) {
            let placeholder = characters[index..end_index].iter().collect::<String>();
            ranges.push((index, end_index, placeholder));
            index = end_index;

            continue;
        }

        index += 1;
    }

    ranges
}

/// Inserts pasted prompt text and clears transient slash/history state.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_text`] delegates here to keep behavior
/// centralized.
pub fn insert_prompt_text(
    input: &mut InputState,
    history_state: &mut PromptHistoryState,
    slash_state: &mut PromptSlashState,
    text: &str,
) {
    input.insert_text(text);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Inserts one typed character and clears transient slash/history state.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_char`] delegates here to keep behavior
/// centralized.
pub fn insert_prompt_character(
    input: &mut InputState,
    history_state: &mut PromptHistoryState,
    slash_state: &mut PromptSlashState,
    character: char,
) {
    input.insert_char(character);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Inserts one pasted image placeholder and records the attachment metadata.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::insert_local_image`] delegates here to keep
/// behavior centralized.
pub fn insert_prompt_local_image(
    attachment_state: &mut PromptAttachmentState,
    history_state: &mut PromptHistoryState,
    input: &mut InputState,
    slash_state: &mut PromptSlashState,
    local_image_path: PathBuf,
) {
    attachment_state.remember_current_revision(input);
    let placeholder_start = input.cursor;
    let placeholder = PromptAttachment::placeholder_for(attachment_state.next_attachment_number);
    input.insert_text(&placeholder);
    attachment_state.sync_after_edit(input, placeholder_start, placeholder_start, input.cursor);
    attachment_state.register_local_image(local_image_path, placeholder_start);
    attachment_state.remember_current_revision(input);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Applies one prompt deletion range, expanding it to whole image placeholders
/// and pruning orphaned attachment metadata.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::delete_range`] delegates here to keep behavior
/// centralized.
pub fn apply_prompt_delete_range(
    attachment_state: &mut PromptAttachmentState,
    history_state: &mut PromptHistoryState,
    input: &mut InputState,
    slash_state: &mut PromptSlashState,
    start: usize,
    end: usize,
) {
    let (delete_start, delete_end) = expand_delete_range_to_image_tokens(input.text(), start, end);
    if delete_start >= delete_end {
        return;
    }

    attachment_state.remember_current_revision(input);
    input.replace_range(delete_start, delete_end, "");
    attachment_state.sync_after_edit(input, delete_start, delete_end, delete_start);
    history_state.reset_navigation();
    slash_state.reset();
}

/// Drains the prompt composer into text plus attachment metadata suitable for
/// runtime turn submission.
///
/// This is the canonical field-level mutation path used by runtime code.
/// [`PromptComposerState::take_submission`] delegates here to keep behavior
/// centralized.
pub fn drain_prompt_submission(
    attachment_state: &mut PromptAttachmentState,
    input: &mut InputState,
) -> PromptComposerSubmission {
    let text = input.take_text();
    let mut attachments = attachment_state
        .attachments
        .iter()
        .filter(|attachment| attachment.current_start.is_some())
        .cloned()
        .collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.current_start.unwrap_or(usize::MAX));
    attachment_state.reset();

    PromptComposerSubmission { attachments, text }
}

/// Builds the render-ready prompt slash suggestion list for the provided
/// input and slash state.
///
/// `allow_apply_command` controls whether `/apply` participates in the
/// command-stage list so rendered rows stay aligned with selection indexes.
#[must_use]
pub fn build_prompt_slash_suggestion_list(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Option<PromptSuggestionList> {
    build_slash_suggestion_list(input, slash_state, session_agent_kind, allow_apply_command)
}

/// Resolves the semantic prompt slash action behind the current selection.
///
/// The resolved item is clamped to the same visible selection range used by
/// [`build_prompt_slash_suggestion_list`] so submit behavior stays aligned with
/// the currently rendered highlight even when `selected_index` is stale.
/// `allow_apply_command` must match the value used for rendering and
/// navigation counts.
#[must_use]
pub fn resolve_prompt_slash_selection(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Option<PromptSuggestionSelection> {
    selected_slash_action(input, slash_state, session_agent_kind, allow_apply_command)
}

/// Builds one prompt slash suggestion list for the provided input state.
fn build_slash_suggestion_list(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Option<PromptSuggestionList> {
    if !input.starts_with('/') {
        return None;
    }

    let (title, items): (&str, Vec<PromptSuggestionItem>) = match slash_state.stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(input, session_agent_kind, allow_apply_command)
                .into_iter()
                .map(|command| PromptSuggestionItem {
                    badge: None,
                    detail: Some(command_description(command).to_string()),
                    label: command.to_string(),
                    metadata: None,
                })
                .collect::<Vec<_>>();

            ("Slash Command (j/k move, Enter select)", commands)
        }
        PromptSlashStage::Agent => (
            "/model Agent (j/k move, Enter select)",
            slash_state
                .available_agent_kinds
                .iter()
                .map(|agent_kind| PromptSuggestionItem {
                    badge: None,
                    detail: Some(agent_kind.description().to_string()),
                    label: agent_kind.name().to_string(),
                    metadata: None,
                })
                .collect(),
        ),
        PromptSlashStage::Model => {
            let selected_agent_kind = resolve_model_stage_agent(
                session_agent_kind,
                &slash_state.available_agent_kinds,
                slash_state.selected_agent,
            )?;
            let models = selected_agent_kind
                .models()
                .iter()
                .map(|model| PromptSuggestionItem {
                    badge: None,
                    detail: Some(model.description().to_string()),
                    label: model.name().to_string(),
                    metadata: None,
                })
                .collect::<Vec<_>>();

            ("/model Model (j/k move, Enter select)", models)
        }
        PromptSlashStage::Mode => (
            "/mode (j/k move, Enter select)",
            permission_mode_suggestion_items(),
        ),
        PromptSlashStage::Personality => (
            "/personality (j/k move, Enter select)",
            personality_suggestion_items(&slash_state.personalities),
        ),
        PromptSlashStage::Reasoning => (
            "/reasoning Level (j/k move, Enter select)",
            reasoning_suggestion_items(),
        ),
        PromptSlashStage::Style => (
            "/style Response style (j/k move, Enter select)",
            response_style_suggestion_items(),
        ),
        PromptSlashStage::Speed => (
            "/speed Mode (j/k move, Enter select)",
            speed_suggestion_items(),
        ),
    };

    if items.is_empty() {
        return None;
    }

    let max_index = items.len().saturating_sub(1);

    Some(PromptSuggestionList {
        items,
        selected_index: slash_state.selected_index.min(max_index),
        title: title.to_string(),
    })
}

/// Returns the semantic slash action mapped to the current selection state.
fn selected_slash_action(
    input: &str,
    slash_state: &PromptSlashState,
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Option<PromptSuggestionSelection> {
    match slash_state.stage {
        PromptSlashStage::Command => {
            let commands = prompt_slash_commands(input, session_agent_kind, allow_apply_command);
            let selected_command = commands
                .get(clamp_selected_index(
                    slash_state.selected_index,
                    commands.len(),
                ))
                .copied()?;

            Some(PromptSuggestionSelection::Command(selected_command))
        }
        PromptSlashStage::Agent => slash_state
            .available_agent_kinds
            .get(clamp_selected_index(
                slash_state.selected_index,
                slash_state.available_agent_kinds.len(),
            ))
            .copied()
            .map(PromptSuggestionSelection::Agent),
        PromptSlashStage::Model => {
            let selected_agent_kind = resolve_model_stage_agent(
                session_agent_kind,
                &slash_state.available_agent_kinds,
                slash_state.selected_agent,
            )?;
            let models = selected_agent_kind.models();
            let selected_model = models
                .get(clamp_selected_index(
                    slash_state.selected_index,
                    models.len(),
                ))
                .copied()?;

            Some(PromptSuggestionSelection::Model(AgentSelection::new(
                selected_agent_kind,
                selected_model,
            )))
        }
        PromptSlashStage::Mode => {
            let selected_permission_mode = PermissionMode::ALL
                .get(clamp_selected_index(
                    slash_state.selected_index,
                    PermissionMode::ALL.len(),
                ))
                .copied()?;

            Some(PromptSuggestionSelection::Mode(selected_permission_mode))
        }
        PromptSlashStage::Personality => {
            if slash_state.personalities.is_empty() {
                return None;
            }

            if slash_state.selected_index == 0 {
                return Some(PromptSuggestionSelection::Personality(None));
            }

            slash_state
                .personalities
                .get(clamp_selected_index(
                    slash_state.selected_index.saturating_sub(1),
                    slash_state.personalities.len(),
                ))
                .cloned()
                .map(|personality| PromptSuggestionSelection::Personality(Some(personality)))
        }
        PromptSlashStage::Reasoning => {
            let options = reasoning_options();
            let selected_reasoning = options
                .get(clamp_selected_index(
                    slash_state.selected_index,
                    options.len(),
                ))
                .copied()?;

            Some(PromptSuggestionSelection::Reasoning(selected_reasoning))
        }
        PromptSlashStage::Style => {
            let styles = ResponseStyle::ALL;
            let selected_style = styles[slash_state
                .selected_index
                .min(styles.len().saturating_sub(1))];

            Some(PromptSuggestionSelection::Style(selected_style))
        }
        PromptSlashStage::Speed => {
            let options = SpeedMode::ALL;
            let selected_speed_mode = options
                .get(clamp_selected_index(
                    slash_state.selected_index,
                    options.len(),
                ))
                .copied()?;

            Some(PromptSuggestionSelection::Speed(selected_speed_mode))
        }
    }
}

/// Clamps one slash-menu selection index to the highest visible row index.
fn clamp_selected_index(selected_index: usize, option_count: usize) -> usize {
    selected_index.min(option_count.saturating_sub(1))
}

/// Resolves the agent shown for `/model` model selection while preserving the
/// current session agent when it is still locally runnable.
///
/// When `selected_agent` is absent, this intentionally prefers
/// `session_agent_kind` over the first available agent so the model list stays
/// aligned with the current session backend.
fn resolve_model_stage_agent(
    session_agent_kind: AgentKind,
    available_agent_kinds: &[AgentKind],
    selected_agent: Option<AgentKind>,
) -> Option<AgentKind> {
    selected_agent.or_else(|| {
        agent::resolve_prompt_model_agent_kind(session_agent_kind, available_agent_kinds)
    })
}

/// Returns the fixed description text for one slash command label.
fn command_description(command: &str) -> &'static str {
    match command {
        "/apply" => "Verify focused-review suggestions, then apply the correct ones.",
        "/mode" => "Choose editing permissions and focused-review automation.",
        "/model" => "Choose an agent and model for this session.",
        "/personality" => "List: .agents/agents/. Choose a personality for this session.",
        "/reasoning" => "Override the reasoning level for this session.",
        "/style" => "Control how concise or detailed responses are.",
        "/speed" => "Choose normal or fast responses for this session.",
        _ => "Prompt slash command.",
    }
}

/// Returns all slash commands whose fuzzy characters match the current input.
fn prompt_slash_commands(
    input: &str,
    session_agent_kind: AgentKind,
    allow_apply_command: bool,
) -> Vec<&'static str> {
    let lowered = input.to_lowercase();
    let mut commands = vec![
        "/apply",
        "/mode",
        "/model",
        "/personality",
        "/reasoning",
        "/style",
        "/speed",
    ];
    if !allow_apply_command {
        commands.retain(|command| *command != "/apply");
    }
    if !session_agent_kind.supports_speed_mode() {
        commands.retain(|command| *command != "/speed");
    }
    commands.retain(|command| slash_command_fuzzy_score(command, &lowered).is_some());
    commands.sort_by_key(|command| slash_command_fuzzy_score(command, &lowered).unwrap_or(0));

    commands
}

/// Scores how well one slash command matches a lowercase fuzzy query.
fn slash_command_fuzzy_score(command: &str, lowered_query: &str) -> Option<usize> {
    let command_name = command.trim_start_matches('/');
    let query = lowered_query.trim_start_matches('/');
    if query.is_empty() {
        return Some(0);
    }

    if command_name == query {
        return Some(0);
    }
    if command_name.starts_with(query) {
        return Some(1);
    }

    if let Some(match_start) = command_name.find(query) {
        return Some(100 + match_start);
    }

    let mut query_chars = query.chars().peekable();
    let mut gap_count = 0;
    let mut first_match_index = None;
    let mut last_match_index = None;

    for (command_index, command_char) in command_name.chars().enumerate() {
        let Some(query_char) = query_chars.peek() else {
            break;
        };

        if command_char != *query_char {
            continue;
        }

        if let Some(previous_match_index) = last_match_index {
            gap_count += command_index.saturating_sub(previous_match_index + 1);
        }
        if first_match_index.is_none() {
            first_match_index = Some(command_index);
        }
        last_match_index = Some(command_index);
        query_chars.next();
    }

    if query_chars.peek().is_some() {
        return None;
    }

    Some(200 + first_match_index.unwrap_or(0) * 2 + gap_count)
}

/// Returns the stable `/reasoning` selection options.
fn reasoning_options() -> Vec<ReasoningLevel> {
    ReasoningLevel::ALL.to_vec()
}

/// Returns the render-ready dropdown rows for `/reasoning`.
fn reasoning_suggestion_items() -> Vec<PromptSuggestionItem> {
    ReasoningLevel::ALL
        .into_iter()
        .map(|reasoning_level| PromptSuggestionItem {
            badge: None,
            detail: Some(reasoning_level.description().to_string()),
            label: reasoning_level.name().to_string(),
            metadata: None,
        })
        .collect()
}

/// Returns the render-ready dropdown rows for `/style`.
fn response_style_suggestion_items() -> Vec<PromptSuggestionItem> {
    ResponseStyle::ALL
        .iter()
        .copied()
        .map(|response_style| PromptSuggestionItem {
            badge: None,
            detail: Some(response_style.description().to_string()),
            label: response_style.name().to_string(),
            metadata: None,
        })
        .collect()
}

/// Returns the render-ready dropdown rows for `/speed`.
fn speed_suggestion_items() -> Vec<PromptSuggestionItem> {
    SpeedMode::ALL
        .into_iter()
        .map(|speed_mode| PromptSuggestionItem {
            badge: None,
            detail: Some(speed_mode.description().to_string()),
            label: speed_mode.name().to_string(),
            metadata: None,
        })
        .collect()
}

/// Returns the render-ready dropdown rows for `/mode`.
fn permission_mode_suggestion_items() -> Vec<PromptSuggestionItem> {
    PermissionMode::ALL
        .into_iter()
        .map(|permission_mode| PromptSuggestionItem {
            badge: None,
            detail: Some(permission_mode.description().to_string()),
            label: permission_mode.display_label().to_string(),
            metadata: None,
        })
        .collect()
}

/// Returns render-ready rows for `/personality`.
fn personality_suggestion_items(personalities: &[PersonalitySummary]) -> Vec<PromptSuggestionItem> {
    if personalities.is_empty() {
        return vec![PromptSuggestionItem {
            badge: None,
            detail: None,
            label: "No personalities found in `.agents/agents`.".to_string(),
            metadata: None,
        }];
    }

    std::iter::once(PromptSuggestionItem {
        badge: None,
        detail: Some("Use the agent's default behavior.".to_string()),
        label: "None (default)".to_string(),
        metadata: None,
    })
    .chain(
        personalities
            .iter()
            .map(|personality| PromptSuggestionItem {
                badge: None,
                detail: Some(personality.description.clone()),
                label: personality.name.clone(),
                metadata: None,
            }),
    )
    .collect()
}

/// Returns the exclusive end index for an `[Image #n]` placeholder token that
/// starts at `start_index`.
fn image_token_end_index(characters: &[char], start_index: usize) -> Option<usize> {
    let token_body = characters.get(start_index..)?;
    if token_body.len() < "[Image #1]".chars().count() || token_body.first() != Some(&'[') {
        return None;
    }

    let image_prefix = ['[', 'I', 'm', 'a', 'g', 'e', ' ', '#'];
    if token_body.get(..image_prefix.len())? != image_prefix {
        return None;
    }

    let mut scan_index = start_index + image_prefix.len();
    let mut saw_digit = false;
    while let Some(character) = characters.get(scan_index) {
        if character.is_ascii_digit() {
            saw_digit = true;
            scan_index += 1;

            continue;
        }

        if *character == ']' && saw_digit {
            return Some(scan_index + 1);
        }

        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::domain::input::INPUT_HISTORY_LIMIT;

    fn insert_test_attachment(
        attachment_state: &mut PromptAttachmentState,
        input: &mut InputState,
        local_image_path: PathBuf,
    ) -> String {
        let mut history_state = PromptHistoryState::default();
        let mut slash_state = PromptSlashState::default();
        insert_prompt_local_image(
            attachment_state,
            &mut history_state,
            input,
            &mut slash_state,
            local_image_path,
        );

        attachment_state
            .attachments
            .last()
            .expect("attachment should be registered")
            .placeholder
            .clone()
    }

    #[test]
    fn test_prompt_attachment_state_registers_images_in_placeholder_order() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::default();

        // Act
        let first_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"), 0);
        let second_placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"), 10);

        // Assert
        assert_eq!(first_placeholder, "[Image #1]");
        assert_eq!(second_placeholder, "[Image #2]");
        assert_eq!(attachment_state.attachments.len(), 2);
        let attachment = attachment_state
            .attachment_for_placeholder("[Image #2]")
            .expect("second attachment should exist");
        assert_eq!(attachment.attachment_number, 2);
        assert_eq!(
            attachment.local_image_path,
            PathBuf::from("/tmp/second-image.png")
        );
        assert_eq!(attachment.placeholder, "[Image #2]");
    }

    #[test]
    fn test_prompt_attachment_state_reset_clears_attachments_and_restarts_numbering() {
        // Arrange
        let mut attachment_state = PromptAttachmentState::default();
        let _ = attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"), 0);

        // Act
        attachment_state.reset();
        let placeholder =
            attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"), 0);

        // Assert
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(attachment_state.next_attachment_number, 2);
        assert_eq!(placeholder, "[Image #1]");
    }

    #[test]
    fn test_prompt_attachment_state_refresh_next_attachment_number_stays_monotonic() {
        // Arrange
        let mut attachment_state = PromptAttachmentState {
            archived_attachments: Vec::new(),
            attachments: vec![
                PromptAttachment::new(1, PathBuf::from("/tmp/first-image.png")),
                PromptAttachment::new(3, PathBuf::from("/tmp/third-image.png")),
            ],
            next_attachment_number: 99,
        };

        // Act
        attachment_state.refresh_next_attachment_number();

        // Assert
        assert_eq!(attachment_state.next_attachment_number, 4);
    }

    #[test]
    fn test_prompt_attachment_state_ignores_revision_before_attachment_is_placed() {
        // Arrange
        let input = InputState::default();
        let mut attachment_state = PromptAttachmentState::default();
        attachment_state.attachments.push(PromptAttachment::new(
            1,
            PathBuf::from("/tmp/first-image.png"),
        ));

        // Act
        attachment_state.remember_current_revision(&input);

        // Assert
        assert_eq!(
            attachment_state.attachments[0].valid_locations,
            [] as [crate::domain::composer::AttachmentRevision; 0]
        );
    }

    #[test]
    fn test_prompt_attachment_ignores_edits_while_not_in_input() {
        // Arrange
        let mut attachment = PromptAttachment::new(1, PathBuf::from("/tmp/first-image.png"));

        // Act
        attachment.apply_edit(0, 0, 1);

        // Assert
        assert_eq!(attachment.current_start, None);
    }

    #[test]
    fn test_prompt_attachment_state_sync_restores_undone_attachment() {
        // Arrange
        let mut input = InputState::default();
        let mut attachment_state = PromptAttachmentState::default();
        let placeholder = insert_test_attachment(
            &mut attachment_state,
            &mut input,
            PathBuf::from("/tmp/first-image.png"),
        );
        attachment_state.remember_current_revision(&input);
        input.replace_range(0, placeholder.chars().count(), "");
        attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);

        // Act
        input.undo();
        attachment_state.sync_after_history_restore(&input);

        // Assert
        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(
            attachment_state.archived_attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.next_attachment_number, 2);
    }

    #[test]
    fn test_prompt_attachment_state_does_not_activate_manually_entered_placeholder() {
        // Arrange
        let mut input = InputState::default();
        let mut attachment_state = PromptAttachmentState::default();
        let placeholder = insert_test_attachment(
            &mut attachment_state,
            &mut input,
            PathBuf::from("/tmp/first-image.png"),
        );
        attachment_state.remember_current_revision(&input);
        input.replace_range(0, placeholder.chars().count(), "");
        attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);

        // Act
        let insert_start = input.cursor;
        input.insert_text(&placeholder);
        attachment_state.sync_after_edit(&input, insert_start, insert_start, input.cursor);

        // Assert
        assert_eq!(
            attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(attachment_state.archived_attachments.len(), 1);
        let submission = drain_prompt_submission(&mut attachment_state, &mut input);
        assert_eq!(
            submission.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(submission.text, placeholder);
    }

    #[test]
    fn test_prompt_attachment_state_prunes_attachment_after_revision_eviction() {
        // Arrange
        let mut input = InputState::default();
        let mut attachment_state = PromptAttachmentState::default();
        let placeholder = insert_test_attachment(
            &mut attachment_state,
            &mut input,
            PathBuf::from("/tmp/first-image.png"),
        );
        attachment_state.remember_current_revision(&input);
        input.replace_range(0, placeholder.chars().count(), "");
        attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);
        for _ in 0..INPUT_HISTORY_LIMIT {
            let insert_start = input.cursor;
            input.insert_char('x');
            attachment_state.sync_after_edit(&input, insert_start, insert_start, input.cursor);
        }

        // Act
        let unreachable = attachment_state.prune_unreachable(&input);

        // Assert
        assert_eq!(
            attachment_state.archived_attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(unreachable.len(), 1);
        assert_eq!(
            unreachable[0].local_image_path,
            PathBuf::from("/tmp/first-image.png")
        );
    }

    #[test]
    fn test_prompt_slash_state_replace_available_agent_kinds_clears_unavailable_selection() {
        // Arrange
        let mut slash_state =
            PromptSlashState::with_available_agent_kinds(vec![AgentKind::Claude, AgentKind::Codex]);
        slash_state.selected_agent = Some(AgentKind::Claude);
        slash_state.selected_index = 2;
        slash_state.stage = PromptSlashStage::Model;

        // Act
        slash_state.replace_available_agent_kinds(vec![AgentKind::Codex]);

        // Assert
        assert_eq!(slash_state.available_agent_kinds, vec![AgentKind::Codex]);
        assert_eq!(slash_state.selected_agent, None);
        assert_eq!(slash_state.selected_index, 0);
        assert_eq!(slash_state.stage, PromptSlashStage::Agent);
    }

    #[test]
    fn test_slash_suggestion_list_for_command_stage_has_description() {
        // Arrange
        let composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/pers".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(
            suggestion_list,
            PromptSuggestionList {
                items: vec![PromptSuggestionItem {
                    badge: None,
                    detail: Some(
                        "List: .agents/agents/. Choose a personality for this session.".to_string(),
                    ),
                    label: "/personality".to_string(),
                    metadata: None,
                }],
                selected_index: 0,
                title: "Slash Command (j/k move, Enter select)".to_string(),
            }
        );
    }

    #[test]
    fn test_slash_suggestion_list_for_command_stage_contains_matches_non_prefix_input() {
        // Arrange
        let composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/o".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        let labels = suggestion_list
            .items
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec!["/mode", "/model", "/personality", "/reasoning"]
        );
    }

    #[test]
    fn test_mode_is_selected_for_m_shortcut() {
        // Arrange
        let slash_state = PromptSlashState::default();

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/m", &slash_state, AgentKind::Codex, false)
                .expect("expected suggestion list");
        let selection = resolve_prompt_slash_selection("/m", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(
            suggestion_list
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["/mode", "/model"]
        );
        assert_eq!(selection, Some(PromptSuggestionSelection::Command("/mode")));
    }

    #[test]
    fn test_personality_slash_stage_lists_default_and_workspace_profiles() {
        // Arrange
        let reviewer = PersonalitySummary {
            description: "Reviews code".to_string(),
            id: "reviewer".to_string(),
            name: "Code Reviewer".to_string(),
        };
        let mut slash_state = PromptSlashState {
            personalities: vec![reviewer.clone()],
            stage: PromptSlashStage::Personality,
            ..PromptSlashState::default()
        };

        // Act
        let default_selection =
            resolve_prompt_slash_selection("/personality", &slash_state, AgentKind::Codex, false);
        slash_state.selected_index = 1;
        let suggestion_list = build_prompt_slash_suggestion_list(
            "/personality",
            &slash_state,
            AgentKind::Codex,
            false,
        )
        .expect("personality suggestions should render");
        let selection =
            resolve_prompt_slash_selection("/personality", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(
            suggestion_list.title,
            "/personality (j/k move, Enter select)"
        );
        assert_eq!(suggestion_list.items[0].label, "None (default)");
        assert_eq!(suggestion_list.items[1].label, "Code Reviewer");
        assert_eq!(
            suggestion_list.items[1].detail.as_deref(),
            Some("Reviews code")
        );
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Personality(Some(reviewer)))
        );
        assert_eq!(
            default_selection,
            Some(PromptSuggestionSelection::Personality(None))
        );
    }

    #[test]
    fn test_personality_slash_stage_shows_non_actionable_empty_hint() {
        // Arrange
        let slash_state = PromptSlashState {
            stage: PromptSlashStage::Personality,
            ..PromptSlashState::default()
        };

        // Act
        let suggestion_list = build_prompt_slash_suggestion_list(
            "/personality",
            &slash_state,
            AgentKind::Codex,
            false,
        )
        .expect("empty personality hint should render");
        let selection =
            resolve_prompt_slash_selection("/personality", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(
            suggestion_list.items[0].label,
            "No personalities found in `.agents/agents`."
        );
        assert_eq!(selection, None);
    }

    #[test]
    fn test_selected_slash_action_uses_fuzzy_matched_command() {
        // Arrange
        let composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/rsn".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Command("/reasoning"))
        );
    }

    #[test]
    fn test_slash_suggestion_list_for_agent_stage_uses_available_agent_kinds() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Claude],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Agent;

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(suggestion_list.items.len(), 1);
        assert_eq!(suggestion_list.items[0].label, "claude");
    }

    #[test]
    fn test_selected_slash_action_returns_selected_model() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Claude],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Model;
        composer.slash_state.selected_agent = Some(AgentKind::Claude);

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Model(AgentSelection::new(
                AgentKind::Claude,
                AgentModel::ClaudeFable5,
            )))
        );
    }

    #[test]
    fn test_mode_stage_lists_modes_and_returns_selected_mode() {
        // Arrange
        let slash_state = PromptSlashState {
            selected_index: 1,
            stage: PromptSlashStage::Mode,
            ..PromptSlashState::default()
        };

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/mode", &slash_state, AgentKind::Codex, false)
                .expect("mode suggestions should render");
        let selection =
            resolve_prompt_slash_selection("/mode", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(suggestion_list.title, "/mode (j/k move, Enter select)");
        assert_eq!(
            suggestion_list
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Auto Edit",
                "Auto Edit + Auto Address Comments",
                "Read Only"
            ]
        );
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Mode(
                PermissionMode::AutoEditAddressComments
            ))
        );
    }

    #[test]
    fn test_selected_slash_action_returns_selected_reasoning_level() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/reasoning".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Reasoning;
        composer.slash_state.selected_index = 2;

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Reasoning(ReasoningLevel::High))
        );
    }

    #[test]
    fn test_speed_stage_lists_modes_and_returns_selected_speed() {
        // Arrange
        let slash_state = PromptSlashState {
            selected_index: 1,
            stage: PromptSlashStage::Speed,
            ..PromptSlashState::default()
        };

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/speed", &slash_state, AgentKind::Codex, false)
                .expect("speed suggestions should render");
        let selection =
            resolve_prompt_slash_selection("/speed", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(
            suggestion_list
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Normal", "Fast"]
        );
        assert_eq!(
            suggestion_list.items[1].detail.as_deref(),
            Some("Faster responses at a higher provider cost.")
        );
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Speed(SpeedMode::Fast))
        );
    }

    #[test]
    fn test_selected_slash_action_clamps_stale_command_index() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/s".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );
        composer.slash_state.selected_index = 9;

        // Act
        let selection = composer.selected_slash_action(AgentKind::Codex);

        // Assert
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Command("/reasoning"))
        );
    }

    #[test]
    fn test_model_stage_suggestion_list_prefers_available_session_agent_when_unset() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/model".to_string()),
            vec![AgentKind::Antigravity, AgentKind::Codex],
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Model;

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        let labels = suggestion_list
            .items
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "gpt-5.6-sol".to_string(),
                "gpt-5.6-terra".to_string(),
                "gpt-5.6-luna".to_string(),
                "gpt-5.3-codex-spark".to_string(),
            ]
        );
    }

    #[test]
    fn test_reasoning_stage_suggestion_list_omits_default_option() {
        // Arrange
        let mut composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/reasoning".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );
        composer.slash_state.stage = PromptSlashStage::Reasoning;

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");
        let labels = suggestion_list
            .items
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(labels, vec!["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn test_prompt_composer_delete_range_removes_whole_image_token() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.insert_text("Review ");
        composer.insert_local_image(PathBuf::from("/tmp/image.png"));
        composer.insert_text(" now");

        // Act
        composer.delete_range(10, 11);

        // Assert
        assert_eq!(composer.input.text(), "Review  now");
        assert_eq!(
            composer.attachment_state.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
        assert_eq!(composer.attachment_state.archived_attachments.len(), 1);
        assert_eq!(composer.attachment_state.next_attachment_number, 2);
    }

    #[test]
    fn test_prompt_composer_insert_char_keeps_attachment_position_synchronized() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.insert_local_image(PathBuf::from("/tmp/image.png"));
        composer.input.cursor = 0;

        // Act
        composer.insert_char('x');
        let submission = composer.take_submission();

        // Assert
        assert_eq!(submission.text, "x[Image #1]");
        assert_eq!(submission.attachments.len(), 1);
        assert_eq!(submission.attachments[0].current_start, Some(1));
    }

    #[test]
    fn test_take_submission_filters_deleted_attachment_placeholders() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.insert_text("One ");
        composer.insert_local_image(PathBuf::from("/tmp/one.png"));
        composer.insert_text(" two ");
        composer.insert_local_image(PathBuf::from("/tmp/two.png"));
        composer.delete_range(4, 15);

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(submission.text, "One two [Image #2]");
        assert_eq!(submission.attachments.len(), 1);
        assert_eq!(submission.attachments[0].placeholder, "[Image #2]");
    }

    #[test]
    fn test_drain_prompt_submission_keeps_raw_at_lookup_text() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.input =
            InputState::with_text("Check @src/main.rs and @docs/guide.md before @".to_string());

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(
            submission.text,
            "Check @src/main.rs and @docs/guide.md before @"
        );
        assert_eq!(submission.attachments.len(), 0);
    }

    #[test]
    fn test_drain_prompt_submission_preserves_email_lookalikes() {
        // Arrange
        let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
        composer.input = InputState::with_text("Notify user@example.com and @!".to_string());

        // Act
        let submission = composer.take_submission();

        // Assert
        assert_eq!(submission.text, "Notify user@example.com and @!");
        assert_eq!(
            submission.attachments,
            [] as [crate::domain::composer::PromptAttachment; 0]
        );
    }

    #[test]
    fn test_render_prompt_text_for_agent_quotes_user_at_lookups() {
        // Arrange
        let prompt_text = "Check @src/main.rs and (@docs/guide.md)";

        // Act
        let rendered_text = render_prompt_text_for_agent(prompt_text);

        // Assert
        assert_eq!(
            rendered_text,
            "Check \"src/main.rs\" and (\"docs/guide.md\")"
        );
    }

    /// Ensures prompt preparation does not special-case literal `looked/up/`
    /// text beyond ordinary `@` lookup quoting.
    #[test]
    fn test_render_prompt_text_for_agent_preserves_literal_looked_up_paths() {
        // Arrange
        let prompt_text = "Check looked/up/README.md, @looked/up/Cargo.toml, \
                           \"looked/up/src/main.rs\", or `looked/up/lib.rs`";

        // Act
        let rendered_text = render_prompt_text_for_agent(prompt_text);

        // Assert
        assert_eq!(
            rendered_text,
            "Check looked/up/README.md, \"looked/up/Cargo.toml\", \"looked/up/src/main.rs\", or \
             `looked/up/lib.rs`"
        );
    }

    #[test]
    fn test_render_prompt_text_for_agent_preserves_non_lookup_at_tokens() {
        // Arrange
        let prompt_text = "Notify user@example.com and leave @ alone";

        // Act
        let rendered_text = render_prompt_text_for_agent(prompt_text);

        // Assert
        assert_eq!(rendered_text, "Notify user@example.com and leave @ alone");
    }

    #[test]
    fn test_current_line_delete_range_returns_first_line_range() {
        // Arrange
        let mut input = InputState::with_text("first line\nsecond line".to_string());
        input.cursor = 0;

        // Act
        let delete_range = current_line_delete_range(&input);

        // Assert
        assert_eq!(delete_range, Some((0, 11)));
    }

    #[test]
    fn test_slash_suggestion_list_includes_apply_command() {
        // Arrange
        let composer = PromptComposerState::with_input_and_history(
            InputState::with_text("/a".to_string()),
            AgentKind::ALL.to_vec(),
            Vec::new(),
        );

        // Act
        let suggestion_list = composer
            .slash_suggestion_list(AgentKind::Codex)
            .expect("expected suggestion list");

        // Assert
        assert_eq!(suggestion_list.items[0].label, "/apply");
        assert_eq!(
            suggestion_list.items[0].detail.as_deref(),
            Some("Verify focused-review suggestions, then apply the correct ones.")
        );
    }

    #[test]
    fn test_prompt_slash_command_list_omits_apply_when_disabled() {
        // Arrange
        let slash_state = PromptSlashState::default();

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/", &slash_state, AgentKind::Codex, false)
                .expect("expected suggestion list");
        let labels = suggestion_list
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            labels,
            vec![
                "/mode",
                "/model",
                "/personality",
                "/reasoning",
                "/style",
                "/speed"
            ]
        );
        assert_eq!(suggestion_list.selected_index, 0);
    }

    #[test]
    fn test_prompt_slash_command_list_omits_speed_for_unsupported_agent() {
        // Arrange
        let slash_state = PromptSlashState::default();

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/", &slash_state, AgentKind::Gemini, false)
                .expect("expected suggestion list");

        // Assert
        assert_eq!(
            suggestion_list
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["/mode", "/model", "/personality", "/reasoning", "/style"]
        );
    }

    #[test]
    fn test_prompt_slash_selection_uses_filtered_command_indexes() {
        // Arrange
        let slash_state = PromptSlashState {
            selected_index: 0,
            ..PromptSlashState::default()
        };

        // Act
        let selection = resolve_prompt_slash_selection("/", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(selection, Some(PromptSuggestionSelection::Command("/mode")));
    }

    #[test]
    fn test_style_stage_lists_descriptions_and_returns_selected_style() {
        // Arrange
        let slash_state = PromptSlashState {
            selected_index: 2,
            stage: PromptSlashStage::Style,
            ..PromptSlashState::default()
        };

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/style", &slash_state, AgentKind::Codex, false)
                .expect("expected style suggestions");
        let selection =
            resolve_prompt_slash_selection("/style", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(
            suggestion_list.title,
            "/style Response style (j/k move, Enter select)"
        );
        assert_eq!(
            suggestion_list
                .items
                .iter()
                .map(|item| (item.label.as_str(), item.detail.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "Concise",
                    Some("Compact answers with essential results, caveats, and verification.")
                ),
                (
                    "Balanced",
                    Some("Enough context to understand and verify without exhaustive detail.")
                ),
                (
                    "Detailed",
                    Some("Thorough decisions, trade-offs, effects, and verification.")
                ),
            ]
        );
        assert_eq!(suggestion_list.selected_index, 2);
        assert_eq!(
            selection,
            Some(PromptSuggestionSelection::Style(ResponseStyle::Detailed))
        );
    }
}

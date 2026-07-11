use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::hash::Hasher;
use std::sync::Arc;

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::{Session, SessionId, Status};
use crate::domain::session_message::{
    SessionMessage, SessionMessageKind, SessionMessageState, SessionTranscript,
};
use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::icon::{Icon, TACHYON_LOADER_WIDTH};
use crate::ui::input_layout::{bottom_pinned_scroll_offset, panel_inner_width};
use crate::ui::markdown::{self, render_markdown};
use crate::ui::prompt_block::{self, USER_PROMPT_PREFIX, USER_PROMPT_RIGHT_GUTTER_WIDTH};
use crate::ui::{Component, session_format, style};

const DRAFT_PREVIEW_HEADER: &str = "## Draft Session";
const DRAFT_PREVIEW_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to stage the \
                                        first draft locally, then press `s` in session view to \
                                        start the bundle.";
const DRAFT_PREVIEW_STACKED_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to \
                                                stage the first draft locally. The `s` start \
                                                action appears after the parent is review-ready.";
const DRAFT_PREVIEW_STAGED_NOTE: &str =
    "Draft messages stay local until you press `s` in session view to start the staged bundle.";
const DRAFT_PREVIEW_STACKED_STAGED_NOTE: &str =
    "Draft messages stay local until the parent is review-ready and you press `s` in session view \
     to start the stacked bundle from its parent branch.";
const SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;
const USER_PROMPT_TAB_WIDTH: usize = 4;

/// Cache key for one fully assembled session-output layout.
///
/// The key is intentionally tied to the session identifier plus observable
/// update version and `updated_at` timestamp instead of hashing the full
/// transcript on every frame. Width, active prompt, queued messages, progress
/// text, and markdown style version cover the transient inputs that can alter
/// rendered lines without changing the stored session row.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionOutputLayoutCacheKey {
    active_progress: TextFingerprint,
    active_prompt_output: TextFingerprint,
    draft_prompt: TextFingerprint,
    /// Whether the draft preview should render stacked-session start guidance.
    is_stacked_child: bool,
    markdown_render_version: u64,
    output_width: u16,
    queued_messages: TextFingerprint,
    session_id: SessionId,
    session_update_version: u64,
    session_updated_at: i64,
    status: Status,
    theme_cache_version: u64,
    transcript: TranscriptFingerprint,
}

/// Compact optional-text identity used by the layout cache key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TextFingerprint {
    content_hash: u64,
    content_len: usize,
    is_some: bool,
}

impl TextFingerprint {
    /// Builds a cheap identity for optional render inputs without retaining
    /// borrowed text in the cache key.
    fn from_text(text: Option<&str>) -> Self {
        let Some(text) = text else {
            return Self {
                content_hash: 0,
                content_len: 0,
                is_some: false,
            };
        };

        let mut hasher = FxHasher::default();
        hasher.write(text.as_bytes());

        Self {
            content_hash: hasher.finish(),
            content_len: text.len(),
            is_some: true,
        }
    }

    /// Builds a cheap identity for a list of render inputs without joining
    /// strings or retaining borrowed text in the cache key.
    fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut content_len = 0;
        let mut content_count = 0;
        let mut hasher = FxHasher::default();

        for text in texts {
            hasher.write(text.as_bytes());
            hasher.write_u8(0xff);
            content_len += text.len();
            content_count += 1;
        }

        Self {
            content_hash: hasher.finish(),
            content_len,
            is_some: content_count > 0,
        }
    }
}

/// Compact identity for a typed transcript snapshot in the layout cache key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptFingerprint {
    content_len: usize,
    is_some: bool,
    last_kind: &'static str,
    last_position: i64,
    message_count: usize,
}

impl TranscriptFingerprint {
    /// Builds a cheap identity for the optional typed transcript without
    /// hashing the full transcript on every frame.
    fn from_session(session: &Session) -> Self {
        let Some(transcript) = session.transcript.as_ref() else {
            return Self {
                content_len: 0,
                is_some: false,
                last_kind: "",
                last_position: 0,
                message_count: 0,
            };
        };
        let messages = transcript.messages();
        let Some(last_message) = messages.last() else {
            return Self {
                content_len: 0,
                is_some: false,
                last_kind: "",
                last_position: 0,
                message_count: 0,
            };
        };

        Self {
            content_len: transcript.total_content_len(),
            is_some: true,
            last_kind: last_message.kind.as_str(),
            last_position: last_message.position,
            message_count: messages.len(),
        }
    }
}

/// Cached result for one fully assembled session-output layout.
#[derive(Clone)]
pub(crate) struct SessionOutputLayout {
    /// Index of the active Tachyon loader row within `lines`, when present.
    pub(crate) active_loader_line_index: Option<usize>,
    /// Number of rendered lines, saturated for scroll metric arithmetic.
    pub(crate) line_count: u16,
    /// Indexes of pending timeline rows that receive loader effects.
    pub(crate) timeline_loader_line_indices: Arc<[usize]>,
    /// Rendered lines shared between scroll metrics and frame painting.
    pub(crate) lines: Arc<[Line<'static>]>,
}

/// Fully assembled session-output lines plus metadata derived during assembly.
struct SessionOutputLines {
    active_loader_line_index: Option<usize>,
    lines: Vec<Line<'static>>,
    timeline_loader_line_indices: Vec<usize>,
}

/// One logical output block in the assembled session transcript panel.
#[derive(Clone, Copy)]
enum SessionOutputBlock {
    ActiveTurn,
    CompletedTranscript,
    QueuedMessage,
    SessionTail,
    TrailingTranscriptNotice(TrailingTranscriptNoticePlacement),
}

/// Render placement for trailing transcript notices split from persisted
/// output.
#[derive(Clone, Copy)]
enum TrailingTranscriptNoticePlacement {
    AfterCompletedTurn,
    BeforeActiveTurn,
}

/// Controls whether a block separator is always emitted or only separates
/// previously rendered content.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionOutputSeparator {
    Always,
    AfterPreviousContent,
}

const SESSION_OUTPUT_BLOCK_ORDER: [SessionOutputBlock; 6] = [
    SessionOutputBlock::CompletedTranscript,
    SessionOutputBlock::TrailingTranscriptNotice(
        TrailingTranscriptNoticePlacement::BeforeActiveTurn,
    ),
    SessionOutputBlock::ActiveTurn,
    SessionOutputBlock::QueuedMessage,
    SessionOutputBlock::TrailingTranscriptNotice(
        TrailingTranscriptNoticePlacement::AfterCompletedTurn,
    ),
    SessionOutputBlock::SessionTail,
];

/// Mutable state for assembling session-output blocks in display order.
struct SessionOutputAssembly<'a> {
    active_loader_line_index: Option<usize>,
    active_progress: Option<&'a str>,
    active_turn_has_visible_text: bool,
    active_turn_section: SessionOutputTranscriptSection<'a>,
    completed_turn_section: SessionOutputTranscriptSection<'a>,
    inner_width: usize,
    lines: Vec<Line<'static>>,
    markdown_render_cache: Option<&'a markdown::MarkdownRenderCache>,
    timeline_loader_line_indices: Vec<usize>,
    session: &'a Session,
    status: Status,
    trailing_notice_section: SessionOutputTranscriptSection<'a>,
}

/// Transcript text split into the visual sections understood by the output
/// assembly.
struct SessionOutputTextSections<'a> {
    active_turn: SessionOutputTranscriptSection<'a>,
    completed_turn: SessionOutputTranscriptSection<'a>,
    trailing_notice: SessionOutputTranscriptSection<'a>,
}

/// One renderable transcript section split from persisted session output.
enum SessionOutputTranscriptSection<'a> {
    Empty,
    Markdown(String),
    Messages(&'a [SessionMessage]),
}

impl SessionOutputTranscriptSection<'_> {
    /// Returns whether this transcript section contains no visible content.
    fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Markdown(text) => text.trim().is_empty(),
            Self::Messages(messages) => messages
                .iter()
                .all(|message| message.content.trim().is_empty()),
        }
    }
}

impl SessionOutputAssembly<'_> {
    /// Appends all known output blocks in the canonical display order.
    fn into_output_lines(mut self) -> SessionOutputLines {
        for block in SESSION_OUTPUT_BLOCK_ORDER {
            self.append_block(block);
        }

        SessionOutputLines {
            active_loader_line_index: self.active_loader_line_index,
            lines: self.lines,
            timeline_loader_line_indices: self.timeline_loader_line_indices,
        }
    }

    /// Appends one optional output block when its current inputs are visible.
    fn append_block(&mut self, block: SessionOutputBlock) {
        match block {
            SessionOutputBlock::CompletedTranscript => self.append_completed_transcript(),
            SessionOutputBlock::TrailingTranscriptNotice(placement) => {
                self.append_trailing_transcript_notice(placement);
            }
            SessionOutputBlock::ActiveTurn => self.append_active_turn(),
            SessionOutputBlock::QueuedMessage => self.append_queued_messages(),
            SessionOutputBlock::SessionTail => self.append_session_tail(),
        }
    }

    fn append_completed_transcript(&mut self) {
        SessionOutput::append_transcript_section_lines(
            &mut self.lines,
            &mut self.timeline_loader_line_indices,
            &self.completed_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_trailing_transcript_notice(&mut self, placement: TrailingTranscriptNoticePlacement) {
        let should_append = match placement {
            TrailingTranscriptNoticePlacement::BeforeActiveTurn => {
                self.active_turn_has_visible_text
            }
            TrailingTranscriptNoticePlacement::AfterCompletedTurn => {
                !self.active_turn_has_visible_text
            }
        };
        if !should_append {
            return;
        }

        SessionOutput::append_transcript_section_lines(
            &mut self.lines,
            &mut self.timeline_loader_line_indices,
            &self.trailing_notice_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_active_turn(&mut self) {
        SessionOutput::append_transcript_section_lines(
            &mut self.lines,
            &mut self.timeline_loader_line_indices,
            &self.active_turn_section,
            self.inner_width,
            self.markdown_render_cache,
        );
    }

    fn append_queued_messages(&mut self) {
        SessionOutput::append_queued_message_lines(&mut self.lines, &self.session.queued_messages);
    }

    fn append_session_tail(&mut self) {
        self.active_loader_line_index = SessionOutput::append_session_tail_lines(
            &mut self.lines,
            self.status,
            self.active_progress,
        );
    }
}

/// Cached session-output layout entry.
struct SessionOutputLayoutCacheEntry {
    key: SessionOutputLayoutCacheKey,
    layout: SessionOutputLayout,
}

/// Bounded LRU cache for the fully assembled session output panel.
///
/// This sits above [`markdown::MarkdownRenderCache`] so the scroll-metric path
/// and render path share one derivation for the same session/update version,
/// width, active prompt, queued messages, and progress text. Entries are
/// invalidated by key changes; the markdown render-cache version and active
/// theme are part of the key so style-bearing lines are not reused after
/// markdown cache invalidation or theme switches. Per-session Tachyonfx state
/// is bounded by the same layout LRU and is removed once no cached layout
/// remains for that session.
pub struct SessionOutputLayoutCache {
    entries: RefCell<VecDeque<SessionOutputLayoutCacheEntry>>,
    tachyon_loader_effects: RefCell<HashMap<SessionId, TachyonLoaderEffect>>,
}

impl Default for SessionOutputLayoutCache {
    fn default() -> Self {
        Self {
            entries: RefCell::new(VecDeque::with_capacity(
                SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT,
            )),
            tachyon_loader_effects: RefCell::new(HashMap::new()),
        }
    }
}

impl SessionOutputLayoutCache {
    /// Returns cached layout lines when all render-affecting inputs match, or
    /// derives and stores a fresh layout otherwise.
    pub(crate) fn layout(
        &self,
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> SessionOutputLayout {
        let key = SessionOutput::layout_cache_key(
            session,
            output_area,
            context,
            markdown_render_cache.map_or(0, markdown::MarkdownRenderCache::version),
        );
        if let Some(layout) = self.cached_layout(&key) {
            return layout;
        }

        let layout =
            SessionOutput::derive_layout(session, output_area, context, markdown_render_cache);
        self.store_entry(SessionOutputLayoutCacheEntry {
            key,
            layout: layout.clone(),
        });

        layout
    }

    /// Returns cached layout for a matching entry and promotes it to the
    /// front of the LRU queue.
    fn cached_layout(&self, key: &SessionOutputLayoutCacheKey) -> Option<SessionOutputLayout> {
        let mut entries = self.entries.borrow_mut();
        let entry_index = entries.iter().position(|entry| &entry.key == key)?;
        let entry = entries.remove(entry_index)?;
        let layout = entry.layout.clone();
        entries.push_front(entry);

        Some(layout)
    }

    /// Stores one freshly rendered entry and evicts old entries plus orphaned
    /// Tachyonfx state over the bounded capacity.
    fn store_entry(&self, entry: SessionOutputLayoutCacheEntry) {
        let mut evicted_session_ids = Vec::new();
        {
            let mut entries = self.entries.borrow_mut();
            entries.push_front(entry);

            while entries.len() > SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
                let Some(evicted_entry) = entries.pop_back() else {
                    continue;
                };
                let evicted_session_id = evicted_entry.key.session_id;
                if !entries
                    .iter()
                    .any(|entry| entry.key.session_id == evicted_session_id)
                {
                    evicted_session_ids.push(evicted_session_id);
                }
            }
        }

        if evicted_session_ids.is_empty() {
            return;
        }

        let mut tachyon_loader_effects = self.tachyon_loader_effects.borrow_mut();
        for session_id in evicted_session_ids {
            tachyon_loader_effects.remove(&session_id);
        }
    }

    /// Applies the cached Tachyonfx loader effect to the current frame,
    /// cloning the session id only when a new per-session effect is needed.
    pub(crate) fn apply_tachyon_loader_effect(
        &self,
        session_id: &SessionId,
        buffer: &mut Buffer,
        area: Rect,
        spinner_frame: usize,
    ) {
        let mut tachyon_loader_effects = self.tachyon_loader_effects.borrow_mut();
        if let Some(effect) = tachyon_loader_effects.get_mut(session_id) {
            effect.apply(buffer, area, spinner_frame);

            return;
        }

        let mut effect = TachyonLoaderEffect::new();
        effect.apply(buffer, area, spinner_frame);
        tachyon_loader_effects.insert(session_id.clone(), effect);
    }
}

/// Session chat output panel renderer.
pub struct SessionOutput<'a> {
    active_prompt_output: Option<&'a str>,
    active_progress: Option<&'a str>,
    /// Shared render cache that avoids re-parsing unchanged markdown each
    /// frame.
    markdown_render_cache: Option<&'a markdown::MarkdownRenderCache>,
    /// Shared layout cache that avoids rebuilding the full rendered transcript
    /// for scroll metrics and frame painting within the same session/update
    /// version.
    output_layout_cache: Option<&'a SessionOutputLayoutCache>,
    scroll_offset: Option<u16>,
    session: &'a Session,
    session_update_version: u64,
}

/// Borrowed inputs that control how session output lines are derived from one
/// session snapshot.
#[derive(Clone, Copy)]
pub(crate) struct SessionOutputLineContext<'a> {
    /// Exact prompt transcript block for the currently active turn, when one
    /// has been submitted in this app process.
    pub(crate) active_prompt_output: Option<&'a str>,
    /// Transient progress text rendered in the active-status loader row.
    pub(crate) active_progress: Option<&'a str>,
    /// Current observable update version for this session snapshot.
    pub(crate) session_update_version: u64,
}

impl<'a> SessionOutput<'a> {
    /// Creates a new session output component.
    pub fn new(session: &'a Session) -> Self {
        Self {
            active_prompt_output: None,
            active_progress: None,
            markdown_render_cache: None,
            output_layout_cache: None,
            scroll_offset: None,
            session,
            session_update_version: 0,
        }
    }

    /// Sets the exact prompt transcript block for the currently active turn.
    #[must_use]
    pub fn active_prompt_output(mut self, active_prompt_output: Option<&'a str>) -> Self {
        self.active_prompt_output = active_prompt_output;
        self
    }

    /// Sets transient progress text rendered in the loader row.
    #[must_use]
    pub fn active_progress(mut self, active_progress: &'a str) -> Self {
        self.active_progress = Some(active_progress);
        self
    }

    /// Sets the shared markdown render cache used to avoid re-parsing
    /// unchanged transcript content each frame.
    #[must_use]
    pub fn markdown_render_cache(mut self, cache: &'a markdown::MarkdownRenderCache) -> Self {
        self.markdown_render_cache = Some(cache);
        self
    }

    /// Sets the shared output-layout cache used by scroll metrics and frame
    /// rendering to avoid rebuilding unchanged transcript layouts.
    #[must_use]
    pub fn output_layout_cache(mut self, cache: &'a SessionOutputLayoutCache) -> Self {
        self.output_layout_cache = Some(cache);
        self
    }

    /// Sets the vertical scroll offset.
    #[must_use]
    pub fn scroll_offset(mut self, offset: u16) -> Self {
        self.scroll_offset = Some(offset);
        self
    }

    /// Sets the observable session update version used to invalidate cached
    /// output layouts when live session handles change.
    #[must_use]
    pub fn session_update_version(mut self, version: u64) -> Self {
        self.session_update_version = version;
        self
    }

    /// Returns the rendered output line count for chat content at a given
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering so scroll math can stay in sync with what users see.
    pub(crate) fn rendered_line_count(
        session: &Session,
        output_width: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
        output_layout_cache: Option<&SessionOutputLayoutCache>,
    ) -> u16 {
        let output_area = Rect::new(0, 0, output_width, 0);

        Self::rendered_layout(
            session,
            output_area,
            context,
            markdown_render_cache,
            output_layout_cache,
        )
        .line_count
    }

    /// Returns the rendered output layout for the current session state,
    /// sharing cached layout lines when a compatible cache is available.
    fn rendered_layout(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
        output_layout_cache: Option<&SessionOutputLayoutCache>,
    ) -> SessionOutputLayout {
        if let Some(cache) = output_layout_cache {
            return cache.layout(session, output_area, context, markdown_render_cache);
        }

        Self::derive_layout(session, output_area, context, markdown_render_cache)
    }

    /// Derives rendered layout lines and line count from the current session
    /// snapshot without consulting the higher-level layout cache.
    fn derive_layout(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> SessionOutputLayout {
        let output_lines =
            Self::output_lines_with_metadata(session, output_area, context, markdown_render_cache);
        let line_count = u16::try_from(output_lines.lines.len()).unwrap_or(u16::MAX);

        SessionOutputLayout {
            active_loader_line_index: output_lines.active_loader_line_index,
            line_count,
            lines: Arc::<[Line<'static>]>::from(output_lines.lines),
            timeline_loader_line_indices: Arc::<[usize]>::from(
                output_lines.timeline_loader_line_indices,
            ),
        }
    }

    /// Builds the cache key for a fully assembled session-output layout.
    fn layout_cache_key(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_version: u64,
    ) -> SessionOutputLayoutCacheKey {
        let inner_width =
            panel_inner_width(output_area, session_format::session_output_panel_borders());

        SessionOutputLayoutCacheKey {
            active_progress: TextFingerprint::from_text(context.active_progress),
            active_prompt_output: TextFingerprint::from_text(context.active_prompt_output),
            draft_prompt: Self::draft_prompt_fingerprint(session),
            is_stacked_child: session.is_stacked_child(),
            markdown_render_version,
            output_width: u16::try_from(inner_width).unwrap_or(u16::MAX),
            queued_messages: TextFingerprint::from_texts(
                session.queued_messages.iter().map(String::as_str),
            ),
            session_id: session.id.clone(),
            session_update_version: context.session_update_version,
            session_updated_at: session.updated_at,
            status: session.status,
            theme_cache_version: style::active_theme_cache_version(),
            transcript: TranscriptFingerprint::from_session(session),
        }
    }

    /// Returns the staged-draft prompt identity when the draft preview reads
    /// from `session.prompt`.
    fn draft_prompt_fingerprint(session: &Session) -> TextFingerprint {
        if session.status == Status::Draft && session.is_draft_session() {
            return TextFingerprint::from_text(Some(session.prompt.as_str()));
        }

        TextFingerprint::from_text(None)
    }

    /// Builds rendered markdown lines, contextual status/help rows, and
    /// metadata for rows that receive post-render effects.
    ///
    /// `Status::Done` includes an inline continuation hint. Active statuses
    /// append only the generic loader row so transcript text stays stable until
    /// the turn completes.
    /// Wrapping width follows the configured output panel borders so line
    /// metrics stay in sync with rendered content. Transcript-derived content
    /// always renders completed content before the currently active prompt
    /// block. Turn-scoped summary and review entries remain anchored to their
    /// owning turn when a later prompt becomes active.
    /// Queued follow-up messages render beneath the running turn so users can
    /// see staged local input without mixing it into completed transcript
    /// content. Trailing transcript notices that belong to a completed turn
    /// stay above any active prompt so in-progress sessions remain
    /// chronological. Every persisted entry is assembled through the same
    /// message stream, including replace-in-place pending workflow entries.
    fn output_lines_with_metadata(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> SessionOutputLines {
        let SessionOutputLineContext {
            active_prompt_output: _,
            active_progress,
            session_update_version: _,
        } = context;
        let status = session.status;
        let transcript_sections = Self::output_text_sections(session, status);
        let inner_width =
            panel_inner_width(output_area, session_format::session_output_panel_borders());
        let active_turn_has_visible_text = !transcript_sections.active_turn.is_empty();
        SessionOutputAssembly {
            active_loader_line_index: None,
            active_progress,
            active_turn_has_visible_text,
            active_turn_section: transcript_sections.active_turn,
            completed_turn_section: transcript_sections.completed_turn,
            inner_width,
            lines: Vec::new(),
            markdown_render_cache,
            timeline_loader_line_indices: Vec::new(),
            session,
            status,
            trailing_notice_section: transcript_sections.trailing_notice,
        }
        .into_output_lines()
    }

    /// Trims trailing blank rows before appending a block separator.
    fn append_block_separator(lines: &mut Vec<Line<'static>>, separator: SessionOutputSeparator) {
        Self::trim_trailing_blank_lines(lines);

        if separator == SessionOutputSeparator::Always || !lines.is_empty() {
            lines.push(Line::from(""));
        }
    }

    /// Removes blank rows from the end of already-assembled output blocks.
    fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }
    }

    /// Appends the trailing status, done, or spacer rows and returns the
    /// active-loader line index when the appended status uses animation.
    fn append_session_tail_lines(
        lines: &mut Vec<Line<'static>>,
        status: Status,
        active_progress: Option<&str>,
    ) -> Option<usize> {
        if let Some(status_line) =
            session_format::session_output_status_line(status, active_progress)
        {
            Self::append_block_separator(lines, SessionOutputSeparator::Always);
            let active_loader_line_index =
                Self::status_uses_tachyon_loader(status).then_some(lines.len());
            lines.push(status_line);

            return active_loader_line_index;
        }

        if status == Status::Done {
            lines.push(Line::from(""));
            lines.push(session_format::session_output_done_line());
            lines.push(Line::from(""));

            return None;
        }

        lines.push(Line::from(""));

        None
    }

    /// Returns transcript sections from draft-preview text or typed message
    /// rows.
    fn output_text_sections(session: &Session, status: Status) -> SessionOutputTextSections<'_> {
        let is_draft_preview = session.status == Status::Draft && session.is_draft_session();
        if is_draft_preview {
            return SessionOutputTextSections {
                active_turn: SessionOutputTranscriptSection::Empty,
                completed_turn: SessionOutputTranscriptSection::Markdown(
                    Self::render_draft_session_preview(session),
                ),
                trailing_notice: SessionOutputTranscriptSection::Empty,
            };
        }

        if let Some(transcript) = session
            .transcript
            .as_ref()
            .filter(|transcript| !transcript.is_empty())
        {
            return Self::typed_transcript_sections(status, transcript);
        }

        SessionOutputTextSections {
            active_turn: SessionOutputTranscriptSection::Empty,
            completed_turn: SessionOutputTranscriptSection::Empty,
            trailing_notice: SessionOutputTranscriptSection::Empty,
        }
    }

    /// Splits a typed transcript without rediscovering user prompts or
    /// workflow notices from rendered text prefixes.
    fn typed_transcript_sections(
        status: Status,
        transcript: &SessionTranscript,
    ) -> SessionOutputTextSections<'_> {
        let messages = transcript.messages();
        let active_prompt_index =
            Self::active_prompt_message_index(status, messages).unwrap_or(messages.len());
        let (completed_messages, active_messages) = messages.split_at(active_prompt_index);
        let trailing_notice_start = Self::trailing_workflow_notice_start(completed_messages)
            .unwrap_or(completed_messages.len());
        let (completed_messages, trailing_notice_messages) =
            completed_messages.split_at(trailing_notice_start);

        SessionOutputTextSections {
            active_turn: Self::messages_section(active_messages),
            completed_turn: Self::messages_section(completed_messages),
            trailing_notice: Self::messages_section(trailing_notice_messages),
        }
    }

    /// Returns a renderable section for a typed message slice.
    fn messages_section(messages: &[SessionMessage]) -> SessionOutputTranscriptSection<'_> {
        if messages.is_empty() {
            return SessionOutputTranscriptSection::Empty;
        }

        SessionOutputTranscriptSection::Messages(messages)
    }

    /// Returns the start index for the latest active user prompt message.
    fn active_prompt_message_index(status: Status, messages: &[SessionMessage]) -> Option<usize> {
        if !matches!(
            status,
            Status::InProgress | Status::Queued | Status::Rebasing | Status::Merging
        ) {
            return None;
        }

        messages
            .iter()
            .rposition(|message| message.kind == SessionMessageKind::UserPrompt)
    }

    /// Returns the first index of the trailing workflow-notice suffix.
    fn trailing_workflow_notice_start(messages: &[SessionMessage]) -> Option<usize> {
        if messages.is_empty() {
            return None;
        }

        let Some(first_non_notice_from_end) = messages
            .iter()
            .rposition(|message| message.kind != SessionMessageKind::WorkflowNotice)
        else {
            return Some(0);
        };
        let notice_start = first_non_notice_from_end.saturating_add(1);

        (notice_start < messages.len()).then_some(notice_start)
    }

    /// Renders the staged-draft guidance shown while a draft session remains
    /// in `Draft`.
    fn render_draft_session_preview(session: &Session) -> String {
        let mut output = String::from(DRAFT_PREVIEW_HEADER);

        if session.has_staged_drafts() {
            let draft_note = if session.is_stacked_child() {
                DRAFT_PREVIEW_STACKED_STAGED_NOTE
            } else {
                DRAFT_PREVIEW_STAGED_NOTE
            };
            let _ = write!(output, "\n\n{draft_note}\n\n");
            output.push_str(&Self::staged_draft_transcript_block(&session.prompt));
        } else {
            let draft_note = if session.is_stacked_child() {
                DRAFT_PREVIEW_STACKED_EMPTY_NOTE
            } else {
                DRAFT_PREVIEW_EMPTY_NOTE
            };
            let _ = write!(output, "\n\n{draft_note}\n");
        }

        if let Some(transcript_text) = session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
        {
            let _ = write!(output, "\n\n{transcript_text}");
        }

        output
    }

    /// Formats the staged draft-session prompt using the same transcript
    /// prompt markers used for persisted user-turn output.
    fn staged_draft_transcript_block(prompt_text: &str) -> String {
        let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(prompt_lines.len());
        let continuation_prefix = prompt_block::user_prompt_continuation_prefix();

        for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
            let prefix = if index == 0 {
                USER_PROMPT_PREFIX
            } else {
                continuation_prefix.as_str()
            };

            formatted_lines.push(format!("{prefix}{prompt_line}"));
        }

        format!("{}\n\n", formatted_lines.join("\n"))
    }

    /// Appends review fallback text without interpreting markdown
    /// metacharacters in status strings.
    ///
    /// The caller owns separator trimming and spacing so this helper remains
    /// purely additive.
    fn append_plain_review_status_lines(
        lines: &mut Vec<Line<'static>>,
        status_message: &str,
        inner_width: usize,
    ) {
        let rendered_lines = text_util::wrap_lines(status_message, inner_width)
            .into_iter()
            .map(|line| Line::from(line.to_string()));

        lines.extend(rendered_lines);
    }

    /// Appends one split transcript section while preserving typed message
    /// boundaries for user prompts.
    fn append_transcript_section_lines(
        lines: &mut Vec<Line<'static>>,
        timeline_loader_line_indices: &mut Vec<usize>,
        section: &SessionOutputTranscriptSection<'_>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        match section {
            SessionOutputTranscriptSection::Empty => {}
            SessionOutputTranscriptSection::Markdown(markdown) => {
                Self::append_markdown_lines(lines, markdown, inner_width, markdown_render_cache);
            }
            SessionOutputTranscriptSection::Messages(messages) => {
                Self::append_transcript_message_lines(
                    lines,
                    timeline_loader_line_indices,
                    messages,
                    inner_width,
                    markdown_render_cache,
                );
            }
        }
    }

    /// Appends typed transcript messages with user prompts rendered from raw
    /// markdown content and assistant/workflow rows rendered normally.
    fn append_transcript_message_lines(
        lines: &mut Vec<Line<'static>>,
        timeline_loader_line_indices: &mut Vec<usize>,
        messages: &[SessionMessage],
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        for message in messages {
            if message.state == SessionMessageState::Pending {
                Self::append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
                timeline_loader_line_indices.push(lines.len());
                lines.push(Line::from(format!(
                    "{} {}",
                    Icon::TachyonLoader.as_str(),
                    message.content.trim()
                )));

                continue;
            }

            match message.kind {
                SessionMessageKind::UserPrompt => Self::append_user_prompt_markdown_lines(
                    lines,
                    &message.content,
                    inner_width,
                    markdown_render_cache,
                ),
                SessionMessageKind::AssistantAnswer | SessionMessageKind::WorkflowNotice => {
                    Self::append_markdown_lines(
                        lines,
                        &message.content,
                        inner_width,
                        markdown_render_cache,
                    );
                }
                SessionMessageKind::TurnSummary => Self::append_markdown_lines(
                    lines,
                    &session_format::session_output_summary_markdown(&message.content),
                    inner_width,
                    markdown_render_cache,
                ),
                SessionMessageKind::FocusedReview => {
                    if message.state == SessionMessageState::Failed {
                        Self::append_block_separator(
                            lines,
                            SessionOutputSeparator::AfterPreviousContent,
                        );
                        Self::append_plain_review_status_lines(
                            lines,
                            &message.content,
                            inner_width,
                        );
                    } else {
                        Self::append_markdown_lines(
                            lines,
                            &session_format::annotate_review_suggestions_header(&message.content),
                            inner_width,
                            markdown_render_cache,
                        );
                    }
                }
            }
        }
    }

    /// Appends one transcript row per chat message currently queued for
    /// dispatch.
    ///
    /// Queued rows render in submission order beneath the running turn with
    /// a muted style and a `queued ›` prefix so users can distinguish staged
    /// follow-ups from completed transcript content while the active turn is
    /// still running.
    fn append_queued_message_lines(lines: &mut Vec<Line<'static>>, queued_messages: &[String]) {
        if queued_messages.is_empty() {
            return;
        }

        Self::append_block_separator(lines, SessionOutputSeparator::Always);

        let queued_style = ratatui::style::Style::default()
            .fg(style::palette::text_subtle())
            .add_modifier(ratatui::style::Modifier::ITALIC);
        for queued_text in queued_messages {
            if queued_text.trim().is_empty() {
                continue;
            }
            for (line_index, message_line) in queued_text.split('\n').enumerate() {
                let prefix = if line_index == 0 {
                    "queued › "
                } else {
                    "        "
                };

                lines.push(Line::styled(
                    format!("{prefix}{message_line}"),
                    queued_style,
                ));
            }
        }
        lines.push(Line::from(""));
    }

    /// Appends one typed user prompt block with its content rendered as
    /// markdown while retaining the visible prompt marker and shaded prompt
    /// rows.
    fn append_user_prompt_markdown_lines(
        lines: &mut Vec<Line<'static>>,
        prompt_text: &str,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        if prompt_text.trim().is_empty() {
            return;
        }

        let prompt_prefix_width = USER_PROMPT_PREFIX.chars().count();
        let prompt_content_width = inner_width
            .saturating_sub(prompt_prefix_width)
            .saturating_sub(USER_PROMPT_RIGHT_GUTTER_WIDTH)
            .max(1);
        let (protected_prompt_text, indent_marker) =
            Self::protect_user_prompt_indentation(prompt_text);
        let rendered_lines = Self::rendered_markdown_lines(
            &protected_prompt_text,
            prompt_content_width,
            markdown_render_cache,
        );
        if rendered_lines.is_empty() {
            return;
        }

        Self::append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
        lines.push(prompt_block::user_prompt_padding_line(inner_width));

        let mut has_rendered_content_line = false;
        let continuation_prefix = prompt_block::user_prompt_continuation_prefix();
        for rendered_line in rendered_lines.iter() {
            if rendered_line.width() == 0 {
                lines.push(prompt_block::user_prompt_padding_line(inner_width));

                continue;
            }

            let prefix = if has_rendered_content_line {
                continuation_prefix.as_str()
            } else {
                USER_PROMPT_PREFIX
            };
            let prefix_style = if has_rendered_content_line {
                prompt_block::user_prompt_content_style()
            } else {
                prompt_block::user_prompt_prefix_style()
            };
            lines.push(prompt_block::user_prompt_markdown_line(
                Self::restored_user_prompt_spans(rendered_line, indent_marker),
                prefix,
                prefix_style,
                inner_width,
            ));
            has_rendered_content_line = true;
        }

        lines.push(prompt_block::user_prompt_padding_line(inner_width));
    }

    /// Replaces leading prompt spaces with a visible-width non-whitespace
    /// marker so Markdown wrapping cannot discard indentation.
    fn protect_user_prompt_indentation(prompt_text: &str) -> (String, Option<char>) {
        let Some(indent_marker) = Self::unused_private_use_character(prompt_text) else {
            return (prompt_text.to_string(), None);
        };

        let mut protected_text = String::with_capacity(prompt_text.len());
        let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
        let preservation_mask = markdown::markdown_block_preservation_mask(prompt_text);

        for (line_index, line) in prompt_lines.into_iter().enumerate() {
            if line_index > 0 {
                protected_text.push('\n');
            }

            if preservation_mask[line_index] {
                protected_text.push_str(line);
            } else {
                let (content_start, indentation_width) = Self::leading_indentation(line);
                let content = &line[content_start..];
                protected_text.extend(std::iter::repeat_n(indent_marker, indentation_width));
                protected_text.push_str(content);
            }
        }

        (protected_text, Some(indent_marker))
    }

    /// Chooses a private-use character absent from the prompt so literal user
    /// content can never collide with the temporary indentation marker.
    fn unused_private_use_character(prompt_text: &str) -> Option<char> {
        const DEFAULT_INDENT_MARKER: char = '\u{e000}';

        if !prompt_text.contains(DEFAULT_INDENT_MARKER) {
            return Some(DEFAULT_INDENT_MARKER);
        }

        let used_characters = prompt_text
            .chars()
            .filter(|character| {
                matches!(
                    u32::from(*character),
                    0xe000..=0xf8ff | 0x000f_0000..=0x000f_fffd | 0x0010_0000..=0x0010_fffd
                )
            })
            .collect::<HashSet<_>>();
        [
            0xe000..=0xf8ff,
            0x000f_0000..=0x000f_fffd,
            0x0010_0000..=0x0010_fffd,
        ]
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .find(|character| !used_characters.contains(character))
    }

    /// Returns the byte offset after leading horizontal whitespace and its
    /// terminal width, expanding tabs to four-column tab stops.
    fn leading_indentation(line: &str) -> (usize, usize) {
        let mut content_start = 0;
        let mut indentation_width = 0;

        for (byte_index, character) in line.char_indices() {
            match character {
                ' ' => indentation_width += 1,
                '\t' => {
                    indentation_width +=
                        USER_PROMPT_TAB_WIDTH - (indentation_width % USER_PROMPT_TAB_WIDTH);
                }
                _ => break,
            }
            content_start = byte_index + character.len_utf8();
        }

        (content_start, indentation_width)
    }

    /// Lazily restores protected indent markers while yielding spans to the
    /// final prompt line, avoiding an intermediate `Line` and `Vec` allocation.
    fn restored_user_prompt_spans<'line>(
        rendered_line: &'line Line<'static>,
        indent_marker: Option<char>,
    ) -> impl Iterator<Item = ratatui::text::Span<'static>> + 'line {
        rendered_line.spans.iter().cloned().map(move |mut span| {
            if let Some(indent_marker) = indent_marker
                && span.content.contains(indent_marker)
            {
                span.content = span.content.replace(indent_marker, " ").into();
            }

            span
        })
    }

    /// Appends rendered markdown with one blank separator while trimming any
    /// existing trailing blank lines from `lines`.
    ///
    /// When a shared render cache is available, every appended markdown block
    /// reuses it so transcript sections do not evict each other between
    /// frames.
    fn append_markdown_lines(
        lines: &mut Vec<Line<'static>>,
        markdown: &str,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        let rendered_lines =
            Self::rendered_markdown_lines(markdown, inner_width, markdown_render_cache);
        if rendered_lines.is_empty() {
            return;
        }

        Self::append_block_separator(lines, SessionOutputSeparator::AfterPreviousContent);
        lines.extend(rendered_lines.iter().cloned());
    }

    /// Returns rendered markdown as a shared slice so cache hits avoid cloning
    /// the entire rendered block.
    fn rendered_markdown_lines(
        markdown: &str,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> Arc<[Line<'static>]> {
        match markdown_render_cache {
            Some(cache) => cache.render(markdown, inner_width),
            None => Arc::from(render_markdown(markdown, inner_width)),
        }
    }

    /// Returns the screen area occupied by a loader glyph when its row is
    /// currently visible inside the scrolled output panel.
    fn loader_area(
        output_area: Rect,
        loader_line_index: Option<usize>,
        final_scroll: u16,
    ) -> Option<Rect> {
        if output_area.width < TACHYON_LOADER_WIDTH {
            return None;
        }

        let inner_area = Self::session_output_inner_area(output_area);
        if inner_area.height == 0 {
            return None;
        }

        let status_line_index = loader_line_index?;
        let first_visible_line_index = usize::from(final_scroll);
        let last_visible_line_index =
            first_visible_line_index.saturating_add(usize::from(inner_area.height));
        if status_line_index < first_visible_line_index
            || status_line_index >= last_visible_line_index
        {
            return None;
        }

        let row_offset = u16::try_from(status_line_index - first_visible_line_index).ok()?;

        Some(Rect::new(
            inner_area.x,
            inner_area.y.saturating_add(row_offset),
            TACHYON_LOADER_WIDTH,
            1,
        ))
    }

    /// Returns the paragraph content area used by the session-output block.
    fn session_output_inner_area(output_area: Rect) -> Rect {
        Rect::new(
            output_area.x,
            output_area.y.saturating_add(1),
            output_area.width,
            output_area.height.saturating_sub(2),
        )
    }

    /// Returns whether a status row should receive the Tachyonfx loader
    /// treatment.
    fn status_uses_tachyon_loader(status: Status) -> bool {
        matches!(
            status,
            Status::InProgress | Status::Rebasing | Status::Merging
        )
    }

    /// Applies one deterministic Tachyonfx pulse frame to the loader glyph.
    ///
    /// Live rendering provides `output_layout_cache` so the Tachyonfx phase is
    /// retained across frames. Callers without that cache receive only a
    /// stateless frame paint for the requested spinner offset.
    fn apply_tachyon_loader_effect(&self, buffer: &mut Buffer, area: Rect, spinner_frame: usize) {
        if let Some(cache) = self.output_layout_cache {
            cache.apply_tachyon_loader_effect(&self.session.id, buffer, area, spinner_frame);

            return;
        }

        // This fallback intentionally does not retain Tachyonfx phase between
        // renders; it exists for isolated component tests and ad hoc renders.
        TachyonLoaderEffect::apply_stateless(buffer, area, spinner_frame);
    }
}

impl Component for SessionOutput<'_> {
    /// Renders bordered output content for the active session.
    ///
    /// Session status/title headers are rendered by the page layer so this
    /// component keeps the output border title-free.
    fn render(&self, f: &mut Frame, output_area: Rect) {
        let status = self.session.status;
        let spinner_frame = Icon::current_spinner_frame();
        let layout = Self::rendered_layout(
            self.session,
            output_area,
            SessionOutputLineContext {
                active_prompt_output: self.active_prompt_output,
                active_progress: self.active_progress,
                session_update_version: self.session_update_version,
            },
            self.markdown_render_cache,
            self.output_layout_cache,
        );
        let final_scroll = bottom_pinned_scroll_offset(
            output_area,
            session_format::session_output_panel_borders(),
            layout.lines.len(),
            self.scroll_offset,
        );
        let active_loader_area = if Self::status_uses_tachyon_loader(status) {
            Self::loader_area(output_area, layout.active_loader_line_index, final_scroll)
        } else {
            None
        };
        let timeline_loader_areas = layout
            .timeline_loader_line_indices
            .iter()
            .filter_map(|line_index| {
                Self::loader_area(output_area, Some(*line_index), final_scroll)
            })
            .collect::<Vec<_>>();

        let paint_lines = text_util::borrowed_paint_lines(&layout.lines);
        let paragraph = Paragraph::new(paint_lines)
            .block(
                Block::default()
                    .borders(session_format::session_output_panel_borders())
                    .border_style(session_format::session_output_panel_border_style(status)),
            )
            .scroll((final_scroll, 0));

        f.render_widget(paragraph, output_area);

        if let Some(loader_area) = active_loader_area {
            self.apply_tachyon_loader_effect(f.buffer_mut(), loader_area, spinner_frame);
        }
        for loader_area in timeline_loader_areas {
            TachyonLoaderEffect::apply_stateless(f.buffer_mut(), loader_area, spinner_frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use ag_protocol::AgentResponseSummary;
    use ratatui::layout::Alignment;
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;
    use serde_json;

    use super::*;
    use crate::domain::theme::ColorTheme;

    /// Builds one output-line context with defaults suitable for tests.
    fn line_context() -> SessionOutputLineContext<'static> {
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: 0,
        }
    }

    fn summary_fixture() -> String {
        serde_json::to_string(&AgentResponseSummary {
            turn: "- Added the structured protocol summary.".to_string(),
            session: "- Session output now renders persisted summary markdown.".to_string(),
        })
        .expect("summary fixture should serialize")
    }

    fn session_fixture() -> Session {
        crate::test_support::SessionFixtureBuilder::new()
            .status(Status::Draft)
            .build()
    }

    /// Builds rendered lines without exposing row metadata to tests that only
    /// assert text content.
    fn output_lines(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> Vec<Line<'static>> {
        SessionOutput::output_lines_with_metadata(
            session,
            output_area,
            context,
            markdown_render_cache,
        )
        .lines
    }

    fn table_header_background(layout: &SessionOutputLayout) -> Option<Color> {
        layout
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref().contains("Input"))
            .and_then(|span| span.style.bg)
    }

    fn set_assistant_transcript(session: &mut Session, output: &str) {
        let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            output,
        )]);
        session.transcript = Some(transcript);
    }

    fn set_summary(session: &mut Session, summary: impl Into<String>) {
        let turn_id = session
            .transcript
            .as_ref()
            .map_or(0, SessionTranscript::current_turn_id);
        set_summary_for_turn(session, turn_id, summary);
    }

    fn set_summary_for_turn(session: &mut Session, turn_id: i64, summary: impl Into<String>) {
        let transcript = session
            .transcript
            .get_or_insert_with(SessionTranscript::default);
        let position = transcript
            .messages()
            .iter()
            .map(|message| message.position)
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        transcript.upsert_timeline_message(SessionMessage::timeline(
            position,
            turn_id,
            format!("turn_summary:{turn_id}"),
            SessionMessageKind::TurnSummary,
            SessionMessageState::Resolved,
            summary,
        ));
    }

    fn set_focused_review_for_turn(
        session: &mut Session,
        turn_id: i64,
        state: SessionMessageState,
        review: impl Into<String>,
    ) {
        let transcript = session
            .transcript
            .get_or_insert_with(SessionTranscript::default);
        let position = transcript
            .messages()
            .iter()
            .map(|message| message.position)
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        transcript.upsert_timeline_message(SessionMessage::timeline(
            position,
            turn_id,
            format!("focused_review:{turn_id}"),
            SessionMessageKind::FocusedReview,
            state,
            review,
        ));
    }

    fn set_conversation_transcript(
        session: &mut Session,
        messages: Vec<(SessionMessageKind, &str)>,
    ) {
        let mut turn_id = 0_i64;
        let transcript = SessionTranscript::new(
            messages
                .into_iter()
                .enumerate()
                .map(|(position, (kind, content))| {
                    let position = i64::try_from(position).unwrap_or(i64::MAX);
                    if kind == SessionMessageKind::UserPrompt {
                        turn_id = turn_id.saturating_add(1);
                    }
                    let mut message = if kind.is_conversation_message() {
                        SessionMessage::conversation(position, kind, content)
                    } else {
                        SessionMessage::new(position, kind, content)
                    };
                    message.turn_id = turn_id;

                    message
                })
                .collect(),
        );
        session.transcript = Some(transcript);
    }

    #[test]
    fn test_rendered_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, &"word ".repeat(40));
        let raw_line_count = u16::try_from(
            session
                .transcript
                .as_ref()
                .and_then(SessionTranscript::replay_text)
                .unwrap_or_default()
                .lines()
                .count(),
        )
        .unwrap_or(u16::MAX);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();

        // Act
        let rendered_line_count = SessionOutput::rendered_line_count(
            &session,
            20,
            line_context(),
            Some(&markdown_render_cache),
            Some(&output_layout_cache),
        );

        // Assert
        assert!(rendered_line_count > raw_line_count);
    }

    #[test]
    fn test_output_layout_cache_reuses_lines_for_matching_update_key() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "## Heading\n\ncached body");
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context()
        };

        // Act
        let first_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        let second_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );

        // Assert
        assert_eq!(first_layout.line_count, second_layout.line_count);
        assert!(Arc::ptr_eq(&first_layout.lines, &second_layout.lines));
    }

    #[test]
    fn test_output_layout_cache_keys_active_theme() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                concat!(
                    "Use **bold** and `code`.\n\n",
                    "| Input | Meaning |\n",
                    "| --- | --- |\n",
                    "| User prompt | Markdown |",
                ),
            )],
        );
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();

        // Act
        let current_layout = {
            let _theme_scope = style::scoped_active_theme(ColorTheme::Current);
            output_layout_cache.layout(
                &session,
                Rect::new(0, 0, 80, 8),
                context,
                Some(&markdown_render_cache),
            )
        };
        let dark_horizon_layout = {
            let _theme_scope = style::scoped_active_theme(ColorTheme::DarkHorizon);
            output_layout_cache.layout(
                &session,
                Rect::new(0, 0, 80, 8),
                context,
                Some(&markdown_render_cache),
            )
        };

        // Assert
        assert!(!Arc::ptr_eq(
            &current_layout.lines,
            &dark_horizon_layout.lines
        ));
        assert_eq!(
            table_header_background(&dark_horizon_layout),
            Some(Color::Rgb(33, 36, 48))
        );
    }

    #[test]
    fn test_output_layout_cache_keys_staged_draft_prompt() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();

        // Act
        let empty_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        session.prompt = "First staged draft".to_string();
        let staged_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        let staged_text = staged_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(!Arc::ptr_eq(&empty_layout.lines, &staged_layout.lines));
        assert!(staged_text.contains("First staged draft"));
    }

    #[test]
    fn test_output_layout_cache_keys_stacked_draft_preview() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        session.prompt = "First staged draft".to_string();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();

        // Act
        let root_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 12),
            context,
            Some(&markdown_render_cache),
        );
        session.parent_session_id = Some(SessionId::from("parent-session"));
        let stacked_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 12),
            context,
            Some(&markdown_render_cache),
        );
        let stacked_text = stacked_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(!Arc::ptr_eq(&root_layout.lines, &stacked_layout.lines));
        assert!(stacked_text.contains("start the stacked"));
        assert!(stacked_text.contains("bundle from its parent"));
        assert!(stacked_text.contains("parent"));
    }

    #[test]
    /// Verifies queued chat rows invalidate the output layout cache so
    /// in-progress replies appear as soon as they are staged.
    fn test_output_layout_cache_keys_queued_messages() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;
        set_assistant_transcript(&mut session, " › running prompt");
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();

        // Act
        let empty_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        session.queued_messages = vec!["queued reply".to_string()];
        let queued_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        let queued_text = queued_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(!Arc::ptr_eq(&empty_layout.lines, &queued_layout.lines));
        assert!(queued_text.contains("queued › queued reply"));
    }

    #[test]
    /// Verifies persisted workflow notices invalidate layout cache entries and
    /// render from the unified transcript.
    fn test_output_layout_cache_keys_workflow_notice() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "implemented the feature");
        session.status = Status::Review;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();

        // Act
        let base_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        session
            .transcript
            .get_or_insert_with(SessionTranscript::default)
            .append_message(
                SessionMessageKind::WorkflowNotice,
                "[Commit] No changes to commit.",
            );
        let notice_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            Some(&markdown_render_cache),
        );
        let notice_text = notice_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let transcript_text = session
            .transcript
            .as_ref()
            .and_then(SessionTranscript::replay_text)
            .unwrap_or_default();

        // Assert
        assert!(transcript_text.contains("[Commit] No changes to commit."));
        assert!(!Arc::ptr_eq(&base_layout.lines, &notice_layout.lines));
        assert!(notice_text.contains("[Commit] No changes to commit."));
    }

    #[test]
    fn test_output_layout_cache_reuses_active_loader_layout_across_frames() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "active output");
        session.status = Status::InProgress;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let first_frame_context = line_context();

        // Act
        let first_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            first_frame_context,
            Some(&markdown_render_cache),
        );
        let repeated_first_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            first_frame_context,
            Some(&markdown_render_cache),
        );
        let repeated_frame_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            first_frame_context,
            Some(&markdown_render_cache),
        );

        // Assert
        assert!(Arc::ptr_eq(
            &first_layout.lines,
            &repeated_first_layout.lines
        ));
        assert!(Arc::ptr_eq(
            &first_layout.lines,
            &repeated_frame_layout.lines
        ));
        assert!(
            first_layout
                .lines
                .iter()
                .any(|line| line.to_string().contains(Icon::TachyonLoader.as_str()))
        );
    }

    #[test]
    fn test_output_layout_cache_keeps_tachyon_effect_state_per_session() {
        // Arrange
        let output_layout_cache = SessionOutputLayoutCache::default();
        let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
        let mut first_buffer = Buffer::empty(area);
        let mut second_buffer = Buffer::empty(area);
        for column in 0..TACHYON_LOADER_WIDTH {
            first_buffer[(column, 0)]
                .set_symbol("▌")
                .set_fg(style::palette::text_muted());
            second_buffer[(column, 0)]
                .set_symbol("▌")
                .set_fg(style::palette::text_muted());
        }
        let first_session_id = SessionId::from("first-loader-session");
        let second_session_id = SessionId::from("second-loader-session");

        // Act
        output_layout_cache.apply_tachyon_loader_effect(
            &first_session_id,
            &mut first_buffer,
            area,
            4,
        );
        output_layout_cache.apply_tachyon_loader_effect(
            &second_session_id,
            &mut second_buffer,
            area,
            4,
        );

        // Assert
        assert_eq!(output_layout_cache.tachyon_loader_effects.borrow().len(), 2);
        assert!(
            (0..TACHYON_LOADER_WIDTH)
                .any(|column| second_buffer[(column, 0)].fg == style::palette::warning())
        );
    }

    #[test]
    fn test_output_layout_cache_evicts_tachyon_effects_with_layout_lru() {
        // Arrange
        let output_layout_cache = SessionOutputLayoutCache::default();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);

        // Act
        for session_index in 0..=SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
            let mut session = session_fixture();
            session.id = SessionId::from(format!("loader-session-{session_index:02}"));
            set_assistant_transcript(&mut session, &format!("active output {session_index}"));
            session.status = Status::InProgress;

            output_layout_cache.layout(
                &session,
                Rect::new(0, 0, 80, 8),
                line_context(),
                Some(&markdown_render_cache),
            );

            let mut buffer = Buffer::empty(area);
            for column in 0..TACHYON_LOADER_WIDTH {
                buffer[(column, 0)].set_symbol("▌");
            }
            output_layout_cache.apply_tachyon_loader_effect(&session.id, &mut buffer, area, 4);
        }

        // Assert
        let tachyon_loader_effects = output_layout_cache.tachyon_loader_effects.borrow();
        assert_eq!(
            tachyon_loader_effects.len(),
            SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT
        );
        assert!(!tachyon_loader_effects.contains_key(&SessionId::from("loader-session-00")));
        assert!(tachyon_loader_effects.contains_key(&SessionId::from("loader-session-16")));
    }

    #[test]
    fn test_output_lines_metadata_marks_status_loader_not_user_text() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(
            &mut session,
            &format!("{} pasted transcript glyph", Icon::TachyonLoader),
        );
        session.status = Status::InProgress;
        let context = line_context();

        // Act
        let output_lines = SessionOutput::output_lines_with_metadata(
            &session,
            Rect::new(0, 0, 80, 8),
            context,
            None,
        );

        // Assert
        let loader_line_index = output_lines
            .active_loader_line_index
            .expect("active loader status row should be tracked");
        let loader_line = output_lines.lines[loader_line_index].to_string();
        assert!(loader_line.contains("Working..."));
        assert!(!loader_line.contains("pasted transcript glyph"));
    }

    #[test]
    fn test_loader_area_tracks_scrolled_row() {
        // Arrange
        let output_area = Rect::new(2, 3, 80, 10);

        // Act
        let loader_area = SessionOutput::loader_area(output_area, Some(19), 12);

        // Assert
        assert_eq!(loader_area, Some(Rect::new(2, 11, TACHYON_LOADER_WIDTH, 1)));
    }

    #[test]
    fn test_loader_area_locates_row_before_following_hint() {
        // Arrange
        let output_area = Rect::new(0, 0, 80, 8);

        // Act
        let loader_area = SessionOutput::loader_area(output_area, Some(1), 0);

        // Assert
        assert_eq!(loader_area, Some(Rect::new(0, 2, TACHYON_LOADER_WIDTH, 1)));
    }

    #[test]
    fn test_loader_area_skips_missing_line_index() {
        // Arrange
        let output_area = Rect::new(0, 0, 80, 10);

        // Act
        let loader_area = SessionOutput::loader_area(output_area, None, 0);

        // Assert
        assert_eq!(loader_area, None);
    }

    #[test]
    fn test_tachyon_loader_effect_emphasizes_loader_cells() {
        // Arrange
        let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
        let mut buffer = Buffer::empty(area);
        for column in 0..TACHYON_LOADER_WIDTH {
            buffer[(column, 0)]
                .set_symbol("▌")
                .set_fg(style::palette::text_muted());
        }

        // Act
        let mut loader_effect = TachyonLoaderEffect::new();
        loader_effect.apply(&mut buffer, area, 4);

        // Assert
        let foreground_colors = (0..TACHYON_LOADER_WIDTH)
            .map(|column| buffer[(column, 0)].fg)
            .collect::<Vec<_>>();
        assert!(foreground_colors.contains(&style::palette::warning()));
        assert!(foreground_colors.contains(&style::palette::warning_soft()));
    }

    #[test]
    fn test_borrowed_paint_lines_reuse_cached_span_content() {
        // Arrange
        let cached_lines = [Line {
            alignment: Some(Alignment::Center),
            spans: vec![Span {
                content: Cow::Owned("cached span text".to_string()),
                style: Style::default(),
            }],
            style: Style::default(),
        }];

        // Act
        let paint_lines = text_util::borrowed_paint_lines(&cached_lines);

        // Assert
        assert_eq!(paint_lines[0].alignment, Some(Alignment::Center));
        assert!(matches!(
            paint_lines[0].spans[0].content,
            Cow::Borrowed("cached span text")
        ));
    }

    #[test]
    fn test_output_lines_done_summary_mode_keeps_transcript_with_summary() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "streamed output");
        set_summary(&mut session, summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Added the structured protocol summary."));
        assert!(text.contains("Session output now renders persisted summary markdown."));
        assert!(text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_render_staged_draft_preview_for_new_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        session.prompt = "First draft\n\nSecond draft".to_string();

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Draft Session"));
        assert!(text.contains("Draft messages stay local until you press s in session view"));
        assert!(text.contains("First draft"));
        assert!(text.contains("Second draft"));
    }

    #[test]
    fn test_output_lines_render_draft_preview_with_status_lines() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        set_assistant_transcript(
            &mut session,
            "[Paste Image Error] Clipboard is unavailable.",
        );
        session.prompt = "First draft".to_string();

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Draft Session"));
        assert!(text.contains("First draft"));
        assert!(text.contains("Paste Image Error"));
        assert!(text.contains("Clipboard is unavailable"));
    }

    #[test]
    fn test_output_lines_render_staged_draft_preview_for_stacked_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        session.parent_session_id = Some(SessionId::from("parent-session"));
        session.prompt = "Stacked draft".to_string();

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Draft Session"));
        assert!(text.contains("start the stacked"));
        assert!(text.contains("bundle from its parent"));
        assert!(text.contains("parent"));
        assert!(text.contains("Stacked draft"));
    }

    #[test]
    fn test_output_lines_render_empty_draft_preview_for_new_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Draft Session"));
        assert!(text.contains("No draft messages staged yet."));
        assert!(text.contains("Use Enter to stage the first draft locally"));
    }

    #[test]
    fn test_output_lines_render_empty_draft_preview_for_stacked_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        session.parent_session_id = Some(SessionId::from("parent-session"));

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Draft Session"));
        assert!(text.contains("No draft messages staged yet."));
        assert!(text.contains("start action appears after the parent is review-ready"));
    }

    #[test]
    fn test_output_lines_done_output_mode_appends_structured_summary() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::AssistantAnswer, "streamed output"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
            ],
        );
        set_summary(&mut session, summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let output_index = text
            .find("streamed output")
            .expect("streamed output should be rendered");
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit footer should be rendered");

        // Assert
        assert!(text.contains("streamed output"));
        assert!(text.contains("Added the structured protocol summary."));
        assert!(text.contains("Session output now renders persisted summary markdown."));
        assert!(output_index < summary_index);
        assert!(summary_index < commit_index);
    }

    /// Verifies later workflow notices stay below the summary that belongs to
    /// the completed agent turn.
    #[test]
    fn test_output_lines_places_summary_before_trailing_workflow_notices() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::AssistantAnswer, "streamed output"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync Assist] Attempt 1/3. Resolving conflicts in:\n- \
                     crates/agentty/src/runtime/worker.rs\n",
                ),
            ],
        );
        set_summary(&mut session, summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let output_index = text
            .find("streamed output")
            .expect("streamed output should be rendered");
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit notice should be rendered");
        let sync_index = text
            .find("[Sync Assist] Attempt 1/3.")
            .expect("sync notice should be rendered");

        // Assert
        assert!(output_index < summary_index);
        assert!(summary_index < commit_index);
        assert!(commit_index < sync_index);
    }

    /// Verifies typed assistant answers that begin with a workflow-notice
    /// prefix remain grouped with assistant output instead of being moved
    /// below the summary.
    #[test]
    fn test_output_lines_typed_assistant_notice_prefix_stays_before_summary() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "summarize merge"),
                (
                    SessionMessageKind::AssistantAnswer,
                    "Assistant output.\n[Merge] this is literal assistant text.",
                ),
            ],
        );
        set_summary(&mut session, summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let assistant_notice_index = text
            .find("[Merge] this is literal assistant text.")
            .expect("assistant notice-looking line should be rendered");
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");

        // Assert
        assert!(assistant_notice_index < summary_index);
    }

    /// Verifies active-turn splitting uses typed user-prompt rows instead of
    /// assistant text that happens to contain the prompt marker.
    #[test]
    fn test_typed_transcript_sections_ignore_assistant_prompt_markers() {
        // Arrange
        let mut transcript = SessionTranscript::default();
        transcript.append_message(SessionMessageKind::UserPrompt, "previous prompt");
        transcript.append_message(
            SessionMessageKind::AssistantAnswer,
            "previous answer\n › quoted assistant marker",
        );
        transcript.append_message(
            SessionMessageKind::WorkflowNotice,
            "\n[Commit] No changes to commit.\n",
        );
        transcript.append_message(SessionMessageKind::UserPrompt, "actual prompt");
        transcript.append_message(
            SessionMessageKind::AssistantAnswer,
            "streaming answer\n › quoted active output",
        );

        // Act
        let sections = SessionOutput::typed_transcript_sections(Status::InProgress, &transcript);
        let completed_turn = match sections.completed_turn {
            SessionOutputTranscriptSection::Messages(messages) => {
                SessionTranscript::display_text_for_messages(messages)
            }
            _ => String::new(),
        };
        let trailing_notice = match sections.trailing_notice {
            SessionOutputTranscriptSection::Messages(messages) => {
                SessionTranscript::display_text_for_messages(messages)
            }
            _ => String::new(),
        };
        let active_turn = match sections.active_turn {
            SessionOutputTranscriptSection::Messages(messages) => {
                SessionTranscript::display_text_for_messages(messages)
            }
            _ => String::new(),
        };

        // Assert
        assert!(completed_turn.contains(" › quoted assistant marker"));
        assert!(trailing_notice.contains("[Commit] No changes to commit."));
        assert!(active_turn.starts_with(" › actual prompt"));
    }

    /// Verifies merge failures render after focused review content, so the
    /// review summary remains visually grouped with the completed turn.
    #[test]
    fn test_output_lines_places_review_before_trailing_workflow_notices() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::AssistantAnswer, "implemented fix"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Merge Error] Cannot merge branch\n",
                ),
            ],
        );
        set_summary_for_turn(&mut session, 0, summary_fixture());
        set_focused_review_for_turn(
            &mut session,
            0,
            SessionMessageState::Resolved,
            "## Review\n\n### Project Impact\n\n- Documentation-only change.",
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let output_index = text
            .find("implemented fix")
            .expect("completed output should be rendered");
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let review_index = text
            .find("Project Impact")
            .expect("review should be rendered");
        let merge_error_index = text
            .find("[Merge Error] Cannot merge branch")
            .expect("merge error should be rendered");

        // Assert
        assert!(output_index < summary_index);
        assert!(summary_index < review_index);
        assert!(review_index < merge_error_index);
    }

    /// Verifies completed published-branch pushes render through transcript
    /// notices instead of appending a sticky synthetic status row.
    #[test]
    fn test_output_lines_uses_transcript_for_completed_published_branch_push() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::WorkflowNotice,
                "\n[Branch Push] Auto-pushed published branch after completed turn.\n",
            )],
        );
        session.published_upstream_ref = Some("origin/wt/session-id".to_string());
        session.status = Status::Review;

        // Act
        let lines = SessionOutput::output_lines_with_metadata(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(),
            None,
        );
        let text = lines
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(lines.timeline_loader_line_indices.is_empty());
        assert!(text.contains("[Branch Push]"));
        assert_eq!(
            text.matches("Auto-pushed published branch after completed turn.")
                .count(),
            1
        );
    }

    #[test]
    fn test_output_lines_review_session_appends_structured_summary() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "implemented the feature");
        set_summary(&mut session, summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("implemented the feature"));
        assert!(text.contains("Added the structured protocol summary."));
        assert!(text.contains("Session output now renders persisted summary markdown."));
    }

    /// Verifies structured summaries render a blank row after their top-level
    /// `Change Summary` heading just like focused-review output.
    #[test]
    fn test_output_lines_structured_summary_spaces_change_summary_header() {
        // Arrange
        let mut session = session_fixture();
        set_summary(&mut session, summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        let summary_header_index = rendered_lines
            .iter()
            .position(|line| line == "Change Summary")
            .expect("structured summary header should be rendered");

        // Assert
        assert_eq!(
            rendered_lines
                .get(summary_header_index + 1)
                .map(String::as_str),
            Some("")
        );
        assert_eq!(
            rendered_lines
                .get(summary_header_index + 2)
                .map(String::as_str),
            Some("Current Turn")
        );
    }

    /// Verifies completed-turn summaries remain before a running reply.
    #[test]
    fn test_output_lines_in_progress_session_keeps_summary_before_active_prompt() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "hi"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (SessionMessageKind::UserPrompt, "add hello world"),
            ],
        );
        set_summary_for_turn(&mut session, 1, summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › add hello world\n\n"),
                ..line_context()
            },
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit footer should be rendered");
        let prompt_index = text
            .find(" › add hello world")
            .expect("active prompt should be rendered");

        let summary_index = text
            .find("Change Summary")
            .expect("completed-turn summary should be rendered");

        // Assert
        assert!(summary_index < commit_index);
        assert!(commit_index < prompt_index);
    }

    /// Verifies queued follow-up messages render after the active turn while
    /// keeping completed-turn summaries anchored above it.
    #[test]
    fn test_output_lines_in_progress_session_shows_queued_messages_after_active_turn() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "hi"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (SessionMessageKind::UserPrompt, "add hello world"),
                (SessionMessageKind::AssistantAnswer, "working"),
            ],
        );
        session.queued_messages = vec!["follow up\nwith context".to_string()];
        set_summary_for_turn(&mut session, 1, summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › add hello world\n\n"),
                ..line_context()
            },
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit footer should be rendered");
        let prompt_index = text
            .find(" › add hello world")
            .expect("active prompt should be rendered");
        let queued_index = text
            .find("queued › follow up")
            .expect("queued message should be rendered");

        let summary_index = text
            .find("Change Summary")
            .expect("completed-turn summary should be rendered");

        // Assert
        assert!(summary_index < commit_index);
        assert!(commit_index < prompt_index);
        assert!(prompt_index < queued_index);
        assert!(text.contains("        with context"));
    }

    /// Verifies the latest user prompt is detected when the exact active
    /// prompt capture is unavailable.
    #[test]
    fn test_output_lines_in_progress_without_active_capture_keeps_summary_before_last_prompt() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "hi"),
                (SessionMessageKind::AssistantAnswer, "Hello!"),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (SessionMessageKind::UserPrompt, "review project"),
            ],
        );
        set_summary_for_turn(&mut session, 1, summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit footer should be rendered");
        let prompt_index = text
            .find(" › review project")
            .expect("latest prompt should be rendered");

        let summary_index = text
            .find("Change Summary")
            .expect("completed-turn summary should be rendered");

        // Assert
        assert!(summary_index < commit_index);
        assert!(commit_index < prompt_index);
    }

    /// Verifies active-turn splitting uses the captured prompt block so
    /// assistant output that resembles a prompt remains in the active block.
    #[test]
    fn test_output_lines_in_progress_ignores_assistant_lines_that_look_like_prompts() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "hi"),
                (SessionMessageKind::AssistantAnswer, "previous answer"),
                (SessionMessageKind::UserPrompt, "actual prompt"),
                (
                    SessionMessageKind::AssistantAnswer,
                    "streaming answer\n › quoted output",
                ),
            ],
        );
        set_summary_for_turn(&mut session, 1, summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › actual prompt\n\n"),
                ..line_context()
            },
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let prompt_index = text
            .find(" › actual prompt")
            .expect("active prompt should be rendered");
        let quoted_output_index = text
            .find(" › quoted output")
            .expect("assistant output that looks like a prompt should be rendered");

        // Assert
        let summary_index = text
            .find("Change Summary")
            .expect("completed-turn summary should be rendered");
        assert!(summary_index < prompt_index);
        assert!(prompt_index < quoted_output_index);
    }

    #[test]
    fn test_output_lines_review_session_without_summary_keeps_transcript_only() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "implemented the feature");
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("implemented the feature"));
        assert!(!text.contains("No changes"));
        assert!(!text.contains("Current Turn"));
        assert!(!text.contains("Session Changes"));
    }

    #[test]
    fn test_output_lines_render_user_prompt_markdown() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (
                    SessionMessageKind::UserPrompt,
                    concat!(
                        "Use **bold** and `code`.\n\n",
                        "| Input | Meaning |\n",
                        "| --- | --- |\n",
                        "| User prompt | Markdown |",
                    ),
                ),
                (SessionMessageKind::AssistantAnswer, "assistant response"),
            ],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let inline_line = lines
            .iter()
            .find(|line| line.to_string().contains("Use bold and code."))
            .expect("inline markdown line should render");
        let table_header_line = lines
            .iter()
            .find(|line| line.to_string().contains("Input"))
            .expect("table header line should render");

        // Assert
        assert!(text.contains(" › Use bold and code."));
        assert!(text.contains("┌"));
        assert!(text.contains("User prompt"));
        assert!(!text.contains("**bold**"));
        assert!(!text.contains("`code`"));
        assert!(!text.contains("| --- | --- |"));
        assert!(inline_line.spans.iter().any(|span| {
            span.content.as_ref() == "bold"
                && span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        }));
        assert!(table_header_line.spans.iter().any(|span| {
            span.content.as_ref().contains("Input")
                && span.style.bg == Some(style::palette::surface_elevated())
        }));
    }

    #[test]
    fn test_output_lines_preserve_user_prompt_indentation() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                "    if ready {\n        run();\n    }",
            )],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let rendered_lines = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();

        // Assert
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == " ›     if ready {"),
            "rendered lines: {rendered_lines:#?}"
        );
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == "           run();")
        );
        assert!(rendered_lines.iter().any(|line| line == "       }"));
    }

    #[test]
    fn test_output_lines_expand_user_prompt_tab_indentation() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                "\tif ready {\n\t\trun();\n\t}",
            )],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let rendered_lines = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();

        // Assert
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == " ›     if ready {")
        );
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == "           run();")
        );
        assert!(rendered_lines.iter().any(|line| line == "       }"));
    }

    #[test]
    fn test_output_lines_preserve_literal_private_use_character() {
        // Arrange
        let prompt = "  before \u{e000} after";
        let mut session = session_fixture();
        set_conversation_transcript(&mut session, vec![(SessionMessageKind::UserPrompt, prompt)]);
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 6), line_context(), None);
        let rendered_text = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(rendered_text.contains(" ›   before \u{e000} after"));
    }

    #[test]
    fn test_output_lines_render_indented_user_prompt_table_and_horizontal_rule() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                concat!(
                    "  | Input | Meaning |\n",
                    "  | --- | --- |\n",
                    "  | Prompt | Indented |\n",
                    "\n",
                    "  ---",
                ),
            )],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let rendered_lines = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();
        let text = rendered_lines.join("\n");

        // Assert
        assert!(text.contains('┌'));
        assert!(text.contains("Prompt"));
        assert!(text.contains("Indented"));
        assert!(!text.contains("| --- | --- |"));
        assert!(rendered_lines.iter().any(|line| {
            line.strip_prefix(prompt_block::user_prompt_continuation_prefix().as_str())
                .is_some_and(|content| {
                    content.len() > 10 && content.chars().all(|character| character == '-')
                })
        }));
    }

    #[test]
    fn test_append_user_prompt_markdown_lines_keeps_right_gutter() {
        // Arrange
        let mut lines = Vec::new();

        // Act
        SessionOutput::append_user_prompt_markdown_lines(&mut lines, "one two", 10, None);
        let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        let continuation_prefix = prompt_block::user_prompt_continuation_prefix();
        let prompt_lines = lines
            .iter()
            .filter(|line| {
                let text = line.to_string();

                !text.trim().is_empty()
                    && (text.starts_with(USER_PROMPT_PREFIX)
                        || text.starts_with(continuation_prefix.as_str()))
            })
            .collect::<Vec<_>>();

        // Assert
        assert!(
            !rendered_lines
                .iter()
                .any(|line| line.trim_end() == " › one two")
        );
        assert_eq!(prompt_lines.len(), 2);
        for line in prompt_lines {
            let rendered_text = line.to_string();
            let trimmed_width = rendered_text.trim_end().chars().count();

            assert_eq!(line.width(), 10);
            assert!(trimmed_width < line.width());
            assert!(
                line.spans
                    .last()
                    .is_some_and(|span| span.content.chars().all(char::is_whitespace)
                        && span.style.bg == Some(style::palette::surface_prompt()))
            );
        }
    }

    #[test]
    fn test_append_user_prompt_markdown_lines_wraps_code_blocks_on_word_boundaries() {
        // Arrange
        let mut lines = Vec::new();
        let prompt = "```text\nformatted blocks in user messages without words breaking\n```";

        // Act
        SessionOutput::append_user_prompt_markdown_lines(&mut lines, prompt, 36, None);
        let rendered_lines = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>();

        // Assert
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == " › formatted blocks in user")
        );
        assert!(
            rendered_lines
                .iter()
                .any(|line| line == "   messages without words breaking")
        );
        assert!(!rendered_lines.iter().any(|line| line.ends_with("message")));
        assert!(!rendered_lines.iter().any(|line| line.starts_with("   s ")));
    }

    #[test]
    fn test_output_lines_render_user_prompt_markdown_with_minimum_content_width() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(SessionMessageKind::UserPrompt, "alpha beta")],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 3, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("..."));
        assert!(lines.iter().all(|line| line.width() <= 3));
    }

    #[test]
    fn test_output_lines_render_user_prompt_mermaid_with_uniform_background() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                concat!(
                    "```mermaid {theme=default}\n",
                    "flowchart TD\n",
                    "    A[Start] --> B[Finish]\n",
                    "```",
                ),
            )],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let start_line = lines
            .iter()
            .find(|line| line.to_string().contains("Start"))
            .expect("Mermaid diagram label should render");
        let border_line = lines
            .iter()
            .find(|line| line.to_string().contains('┌'))
            .expect("Mermaid diagram border should render");

        // Assert
        assert!(text.contains("Start"));
        assert!(text.contains("Finish"));
        assert!(text.contains("▼"));
        assert!(!text.contains("flowchart TD"));
        assert!(!text.contains("```"));
        assert_eq!(start_line.width(), 80);
        assert_eq!(
            start_line.spans[0].style.bg,
            Some(style::palette::surface_prompt())
        );
        assert!(start_line.spans.iter().any(|span| {
            span.content.as_ref().trim().is_empty()
                && span.style.bg == Some(style::palette::surface_prompt())
        }));
        assert!(
            start_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_prompt()))
        );
        assert!(border_line.spans.iter().any(|span| {
            span.content.as_ref().contains('┌')
                && span.style.fg == Some(style::palette::text())
                && span.style.bg == Some(style::palette::surface_prompt())
        }));
        assert!(
            border_line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(style::palette::surface_prompt()))
        );
    }

    #[test]
    fn test_output_lines_keep_prompt_shading_for_mermaid_prefix_language() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![(
                SessionMessageKind::UserPrompt,
                concat!(
                    "```mermaids\n",
                    "flowchart TD\n",
                    "    A[Start] --> B[Finish]\n",
                    "```",
                ),
            )],
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let source_line = lines
            .iter()
            .find(|line| line.to_string().contains("flowchart TD"))
            .expect("non-Mermaid source should remain visible");

        // Assert
        assert!(text.contains("flowchart TD"));
        assert!(text.contains("A[Start] --> B[Finish]"));
        assert!(!text.contains("▼"));
        assert_eq!(source_line.width(), 80);
        assert!(
            source_line
                .spans
                .iter()
                .any(|span| span.style.bg == Some(style::palette::surface_prompt()))
        );
    }

    #[test]
    fn test_output_lines_render_markdown_tables() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(
            &mut session,
            concat!(
                "| Message kind | Storage |\n",
                "| --- | --- |\n",
                "| User prompt | Session.output |",
            ),
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Message kind"));
        assert!(text.contains("Storage"));
        assert!(text.contains("User prompt"));
        assert!(text.contains("Session.output"));
        assert!(text.contains("┌"));
        assert!(!text.contains("| --- | --- |"));
    }

    #[test]
    fn test_output_lines_render_mermaid_diagrams() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(
            &mut session,
            concat!(
                "```mermaid\n",
                "graph TD\n",
                "    A[Start] --> B[Finish]\n",
                "```",
            ),
        );
        session.status = Status::Review;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Start"));
        assert!(text.contains("Finish"));
        assert!(text.contains("┌"));
        assert!(text.contains("▼"));
        assert!(!text.contains("graph TD"));
    }

    /// Verifies the done-summary transition renders the rewritten summary
    /// payload while preserving the completed transcript.
    #[test]
    fn test_output_lines_done_summary_transition_preserves_transcript() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "streamed output");
        set_summary(&mut session, summary_fixture());
        session.status = Status::Review;
        let review_lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let review_text = review_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        set_summary(
            &mut session,
            "# Summary\n\nSession now greets users on startup.\n\n# Commit\n\nRefine session \
             summary"
                .to_string(),
        );
        session.status = Status::Done;

        // Act
        let done_lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let done_text = done_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(review_text.contains("Change Summary"));
        assert!(review_text.contains("Added the structured protocol summary."));
        assert!(done_text.contains("Summary"));
        assert!(done_text.contains("Session now greets users on startup."));
        assert!(done_text.contains("Commit"));
        assert!(done_text.contains("Refine session summary"));
        assert!(done_text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_agent_review_mode_shows_pending_timeline_entry() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::AgentReview;
        set_focused_review_for_turn(
            &mut session,
            0,
            SessionMessageState::Pending,
            "Reviewing changes with gpt-5.5",
        );

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Reviewing changes with gpt-5.5"));
    }

    #[test]
    fn test_output_lines_uses_transcript_for_canceled_session() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "streamed output");
        set_summary(&mut session, summary_fixture());
        session.status = Status::Canceled;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Added the structured protocol summary."));
        assert!(text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_use_generic_in_progress_loader() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "some output");
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Working..."));
        assert!(text.contains(Icon::TachyonLoader.as_str()));
    }
}

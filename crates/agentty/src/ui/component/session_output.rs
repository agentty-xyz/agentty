use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hasher;
use std::sync::Arc;

use ag_tui_text::text_util;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::{QueuedMessage, Session, SessionId, Status};
use crate::ui::component::queue_pulse::QueuePulseEffect;
use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
use crate::ui::icon::{QUEUED_ACTION_WIDTH, TACHYON_LOADER_WIDTH};
use crate::ui::input_layout::{bottom_pinned_scroll_offset, panel_inner_width};
use crate::ui::session_output_assembly::{self, SessionOutputBody};
use crate::ui::{Component, markdown, session_format, style};

const SCROLLBAR_PADDING_WIDTH: u16 = 1;

const SCROLLBAR_WIDTH: u16 = 1;

const SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;

/// Cache key for one fully assembled session-output layout.
///
/// The key is intentionally tied to the session identifier plus observable
/// update version and `updated_at` timestamp instead of hashing the full
/// transcript on every frame. Width, active prompt, queued messages, review
/// state fingerprint, progress text, and markdown style version cover the
/// transient inputs that can alter rendered lines without changing the stored
/// session row.
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
    transient_message_fingerprint: u64,
    transient_message_version: u64,
}

/// Cache key for the stable transcript body assembled above the dynamic
/// session-status tail.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionOutputBodyCacheKey {
    draft_prompt: TextFingerprint,
    has_active_turn: bool,
    is_stacked_child: bool,
    markdown_render_version: u64,
    output_width: u16,
    queued_messages: TextFingerprint,
    session_id: SessionId,
    theme_cache_version: u64,
    transcript: TranscriptFingerprint,
    transient_message_fingerprint: u64,
    transient_message_version: u64,
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
    content_hash: u64,
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
                content_hash: 0,
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
                content_hash: 0,
                content_len: 0,
                is_some: false,
                last_kind: "",
                last_position: 0,
                message_count: 0,
            };
        };

        Self {
            content_hash: transcript.content_hash(),
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
    /// Rendered lines shared between scroll metrics and frame painting.
    pub(crate) lines: Arc<SessionOutputLayoutLines>,
    /// Indices of queued rows whose leading glyph receives a calm pulse.
    pub(crate) queued_line_indices: Arc<[usize]>,
    /// Index of an explicit transient loader row within `lines`, when present.
    pub(crate) transient_loader_line_index: Option<usize>,
}

/// Shared body plus a small status tail. Status updates allocate only the tail.
/// `body_line_count` excludes trailing blank body rows when a status separator
/// replaces them, without copying or changing the cached body.
pub(crate) struct SessionOutputLayoutLines {
    body: Arc<[Line<'static>]>,
    body_line_count: usize,
    tail: Vec<Line<'static>>,
}

impl SessionOutputLayoutLines {
    fn len(&self) -> usize {
        self.body_line_count + self.tail.len()
    }

    /// Borrows only the visible slices on either side of the body/tail
    /// boundary.
    fn paint_lines(&self, first: usize, height: usize) -> Vec<Line<'_>> {
        let body_start = first.min(self.body_line_count);
        let body_end = first.saturating_add(height).min(self.body_line_count);
        let tail_start = first
            .saturating_sub(self.body_line_count)
            .min(self.tail.len());
        let tail_end = first
            .saturating_add(height)
            .saturating_sub(self.body_line_count)
            .min(self.tail.len());
        let mut lines = text_util::borrowed_paint_lines(&self.body[body_start..body_end]);
        lines.extend(text_util::borrowed_paint_lines(
            &self.tail[tail_start..tail_end],
        ));

        lines
    }
}

/// Final session-output layout selected for the current viewport and
/// scrollbar state.
#[derive(Clone)]
struct SessionOutputResolvedLayout {
    layout: SessionOutputLayout,
    show_scrollbar: bool,
}

/// Cached session-output layout entry.
struct SessionOutputLayoutCacheEntry {
    key: SessionOutputLayoutCacheKey,
    layout: SessionOutputLayout,
}

/// One resolved viewport, shared by measurement and painting.
struct SessionOutputResolvedCacheEntry {
    key: SessionOutputLayoutCacheKey,
    resolved: SessionOutputResolvedLayout,
    viewport_height: u16,
}

/// Cached stable output-body entry.
struct SessionOutputBodyCacheEntry {
    body: SessionOutputBody,
    key: SessionOutputBodyCacheKey,
}

impl Default for SessionOutputLayoutCache {
    fn default() -> Self {
        Self {
            body_entries: RefCell::new(VecDeque::with_capacity(
                SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT,
            )),
            entries: RefCell::new(VecDeque::with_capacity(
                SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT,
            )),
            resolved_entries: RefCell::new(VecDeque::new()),
            tachyon_loader_effects: RefCell::new(HashMap::new()),
        }
    }
}

/// Bounded LRU cache for the fully assembled session output panel.
///
/// This sits above [`markdown::MarkdownRenderCache`] so the scroll-metric path
/// and render path share one derivation for the same session/update version,
/// width, active prompt, review text/status, and progress text. Entries are
/// invalidated by key changes; the markdown render-cache version and active
/// theme are part of the key so style-bearing lines are not reused after
/// markdown cache invalidation or theme switches. Per-session Tachyonfx state
/// is bounded by the same layout LRU and is removed once no cached layout
/// remains for that session.
pub struct SessionOutputLayoutCache {
    body_entries: RefCell<VecDeque<SessionOutputBodyCacheEntry>>,
    entries: RefCell<VecDeque<SessionOutputLayoutCacheEntry>>,
    resolved_entries: RefCell<VecDeque<SessionOutputResolvedCacheEntry>>,
    tachyon_loader_effects: RefCell<HashMap<SessionId, TachyonLoaderEffect>>,
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

        let body_key = SessionOutput::body_cache_key(
            session,
            output_area,
            markdown_render_cache.map_or(0, markdown::MarkdownRenderCache::version),
        );
        let body = self.cached_body(&body_key).unwrap_or_else(|| {
            let inner_width =
                panel_inner_width(output_area, session_format::session_output_panel_borders());
            let body =
                session_output_assembly::output_body(session, inner_width, markdown_render_cache);
            self.store_body_entry(SessionOutputBodyCacheEntry {
                body: body.clone(),
                key: body_key,
            });

            body
        });
        let layout = SessionOutput::layout_from_body(session, context.active_progress, &body);
        self.store_entry(SessionOutputLayoutCacheEntry {
            key,
            layout: layout.clone(),
        });

        layout
    }

    /// Reuses the selected width and lines for identical viewport inputs.
    /// Content, theme, progress and width changes invalidate this bounded LRU.
    fn resolved_layout(
        &self,
        session: &Session,
        output_area: Rect,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> SessionOutputResolvedLayout {
        let key = SessionOutput::layout_cache_key(
            session,
            output_area,
            context,
            markdown_render_cache.map_or(0, markdown::MarkdownRenderCache::version),
        );
        {
            let mut entries = self.resolved_entries.borrow_mut();
            if let Some(index) = entries
                .iter()
                .position(|entry| entry.key == key && entry.viewport_height == viewport_height)
                && let Some(entry) = entries.remove(index)
            {
                let resolved = entry.resolved.clone();
                entries.push_front(entry);

                return resolved;
            }
        }
        let resolved = SessionOutput::derive_resolved_layout(
            session,
            output_area,
            viewport_height,
            context,
            markdown_render_cache,
            Some(self),
        );
        let mut entries = self.resolved_entries.borrow_mut();
        entries.retain(|entry| {
            entry.key.session_id != key.session_id || entry.key.output_width != key.output_width
        });
        entries.push_front(SessionOutputResolvedCacheEntry {
            key,
            resolved: resolved.clone(),
            viewport_height,
        });
        entries.truncate(SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT);

        resolved
    }

    /// Returns a matching stable output body and promotes it in the body LRU.
    fn cached_body(&self, key: &SessionOutputBodyCacheKey) -> Option<SessionOutputBody> {
        let mut entries = self.body_entries.borrow_mut();
        let entry_index = entries.iter().position(|entry| &entry.key == key)?;
        let entry = entries.remove(entry_index)?;
        let body = entry.body.clone();
        entries.push_front(entry);

        Some(body)
    }

    /// Stores one stable output body within the same bound as full layouts.
    fn store_body_entry(&self, entry: SessionOutputBodyCacheEntry) {
        let mut entries = self.body_entries.borrow_mut();
        entries.retain(|cached| {
            cached.key.session_id != entry.key.session_id
                || cached.key.output_width != entry.key.output_width
        });
        entries.push_front(entry);

        while entries.len() > SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }
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
            entries.retain(|cached| {
                cached.key.session_id != entry.key.session_id
                    || cached.key.output_width != entry.key.output_width
            });
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

/// Borrowed inputs that control how session output lines are derived from one
/// session snapshot.
#[derive(Clone, Copy)]
pub(crate) struct SessionOutputLineContext<'a> {
    /// Transient progress text rendered in the active-status loader row.
    pub(crate) active_progress: Option<&'a str>,
    /// Exact prompt transcript block for the currently active turn, when one
    /// has been submitted in this app process.
    pub(crate) active_prompt_output: Option<&'a str>,
    /// Current observable update version for this session snapshot.
    pub(crate) session_update_version: u64,
}

/// Session chat output panel renderer.
pub struct SessionOutput<'a> {
    active_progress: Option<&'a str>,
    active_prompt_output: Option<&'a str>,
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
    spinner_frame: usize,
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
            spinner_frame: 0,
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

    /// Sets the deterministic animation frame for active loader effects.
    #[must_use]
    pub fn spinner_frame(mut self, spinner_frame: usize) -> Self {
        self.spinner_frame = spinner_frame;

        self
    }

    /// Returns the rendered output line count for chat content at a given
    /// width.
    ///
    /// This mirrors the exact wrapping and footer line rules used during
    /// rendering, including conditional scrollbar gutter reservation, so
    /// scroll math can stay in sync with what users see.
    pub(crate) fn rendered_line_count(
        session: &Session,
        output_width: u16,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
        output_layout_cache: Option<&SessionOutputLayoutCache>,
    ) -> u16 {
        Self::resolved_layout(
            session,
            Rect::new(0, 0, output_width, 0),
            viewport_height,
            context,
            markdown_render_cache,
            output_layout_cache,
        )
        .layout
        .line_count
    }

    /// Returns the full-width layout when it fits, or derives a second layout
    /// with the scrollbar gutter reserved when the viewport overflows.
    fn resolved_layout(
        session: &Session,
        output_area: Rect,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
        output_layout_cache: Option<&SessionOutputLayoutCache>,
    ) -> SessionOutputResolvedLayout {
        if let Some(cache) = output_layout_cache {
            return cache.resolved_layout(
                session,
                output_area,
                viewport_height,
                context,
                markdown_render_cache,
            );
        }

        Self::derive_resolved_layout(
            session,
            output_area,
            viewport_height,
            context,
            markdown_render_cache,
            None,
        )
    }

    fn derive_resolved_layout(
        session: &Session,
        output_area: Rect,
        viewport_height: u16,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
        output_layout_cache: Option<&SessionOutputLayoutCache>,
    ) -> SessionOutputResolvedLayout {
        let layout_without_scrollbar = Self::rendered_layout(
            session,
            output_area,
            context,
            markdown_render_cache,
            output_layout_cache,
        );
        if !Self::has_scrollable_overflow(layout_without_scrollbar.lines.len(), viewport_height) {
            return SessionOutputResolvedLayout {
                layout: layout_without_scrollbar,
                show_scrollbar: false,
            };
        }

        let layout_with_scrollbar = Self::rendered_layout(
            session,
            Self::scrollbar_layout_area(output_area),
            context,
            markdown_render_cache,
            output_layout_cache,
        );
        let show_scrollbar =
            Self::has_scrollable_overflow(layout_with_scrollbar.lines.len(), viewport_height);

        SessionOutputResolvedLayout {
            layout: layout_with_scrollbar,
            show_scrollbar,
        }
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
        let inner_width =
            panel_inner_width(output_area, session_format::session_output_panel_borders());
        let body =
            session_output_assembly::output_body(session, inner_width, markdown_render_cache);

        Self::layout_from_body(session, context.active_progress, &body)
    }

    /// Shares the stable body and allocates only the dynamic tail.
    fn layout_from_body(
        session: &Session,
        active_progress: Option<&str>,
        body: &SessionOutputBody,
    ) -> SessionOutputLayout {
        let tail = session_output_assembly::output_tail(session, active_progress);
        let body_line_count = if tail.trim_body {
            body.lines
                .iter()
                .rposition(|line| line.width() > 0)
                .map_or(0, |index| index + 1)
        } else {
            body.lines.len()
        };
        let line_count = u16::try_from(body_line_count + tail.lines.len()).unwrap_or(u16::MAX);

        SessionOutputLayout {
            active_loader_line_index: tail
                .active_loader_line_index
                .map(|index| body_line_count + index),
            line_count,
            lines: Arc::new(SessionOutputLayoutLines {
                body: Arc::clone(&body.lines),
                body_line_count,
                tail: tail.lines,
            }),
            queued_line_indices: Arc::clone(&body.queued_line_indices),
            transient_loader_line_index: body.transient_loader_line_index,
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
                session
                    .queued_messages
                    .iter()
                    .map(QueuedMessage::transcript_text),
            ),
            session_id: session.id.clone(),
            session_update_version: context.session_update_version,
            session_updated_at: session.updated_at,
            status: session.status,
            theme_cache_version: style::active_theme_cache_version(),
            transient_message_fingerprint: session.transient_messages.fingerprint(),
            transient_message_version: session.transient_messages.version(),
            transcript: TranscriptFingerprint::from_session(session),
        }
    }

    /// Builds the cache key for transcript content that remains stable while
    /// workflow statuses and progress labels change below it.
    fn body_cache_key(
        session: &Session,
        output_area: Rect,
        markdown_render_version: u64,
    ) -> SessionOutputBodyCacheKey {
        let inner_width =
            panel_inner_width(output_area, session_format::session_output_panel_borders());

        SessionOutputBodyCacheKey {
            draft_prompt: Self::draft_prompt_fingerprint(session),
            has_active_turn: session_output_assembly::status_has_active_turn(session.status),
            is_stacked_child: session.is_stacked_child(),
            markdown_render_version,
            output_width: u16::try_from(inner_width).unwrap_or(u16::MAX),
            queued_messages: TextFingerprint::from_texts(
                session
                    .queued_messages
                    .iter()
                    .map(QueuedMessage::transcript_text),
            ),
            session_id: session.id.clone(),
            theme_cache_version: style::active_theme_cache_version(),
            transient_message_fingerprint: session.transient_messages.fingerprint(),
            transient_message_version: session.transient_messages.version(),
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

    /// Returns the screen area occupied by a leading indicator when its row
    /// is currently visible inside the scrolled output panel.
    fn indicator_area(
        output_area: Rect,
        line_index: usize,
        final_scroll: u16,
        indicator_width: u16,
    ) -> Option<Rect> {
        if output_area.width < indicator_width {
            return None;
        }

        let inner_area = Self::session_output_inner_area(output_area);
        if inner_area.height == 0 {
            return None;
        }

        let first_visible_line_index = usize::from(final_scroll);
        let last_visible_line_index =
            first_visible_line_index.saturating_add(usize::from(inner_area.height));
        if line_index < first_visible_line_index || line_index >= last_visible_line_index {
            return None;
        }

        let row_offset = u16::try_from(line_index - first_visible_line_index).ok()?;

        Some(Rect::new(
            inner_area.x,
            inner_area.y.saturating_add(row_offset),
            indicator_width,
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

    /// Builds borrowed paint lines only for transcript rows in the viewport.
    ///
    /// The cached lines are already wrapped to the output width, so handing
    /// the full transcript back to `Paragraph` would rebuild borrowed line and
    /// span vectors for every off-screen row on each scroll frame. Slicing
    /// before borrowing keeps paint preparation proportional to terminal
    /// height while retaining `Paragraph`'s established cell semantics.
    fn visible_paint_lines(
        output_area: Rect,
        lines: &SessionOutputLayoutLines,
        final_scroll: u16,
    ) -> Vec<Line<'_>> {
        let inner_area = Self::session_output_inner_area(output_area);
        lines.paint_lines(usize::from(final_scroll), usize::from(inner_area.height))
    }

    /// Returns the width used to wrap output while leaving padding before the
    /// scrollbar in the final panel column.
    fn scrollbar_layout_area(output_area: Rect) -> Rect {
        Rect {
            width: output_area
                .width
                .saturating_sub(SCROLLBAR_PADDING_WIDTH)
                .saturating_sub(SCROLLBAR_WIDTH),
            ..output_area
        }
    }

    /// Returns whether the output extends beyond the visible transcript rows.
    fn has_scrollable_overflow(line_count: usize, viewport_height: u16) -> bool {
        viewport_height > 0 && line_count > usize::from(viewport_height)
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
        let spinner_frame = self.spinner_frame;
        let viewport_height = Self::session_output_inner_area(output_area).height;
        let resolved_layout = Self::resolved_layout(
            self.session,
            output_area,
            viewport_height,
            SessionOutputLineContext {
                active_prompt_output: self.active_prompt_output,
                active_progress: self.active_progress,
                session_update_version: self.session_update_version,
            },
            self.markdown_render_cache,
            self.output_layout_cache,
        );
        let layout = resolved_layout.layout;
        let final_scroll = bottom_pinned_scroll_offset(
            output_area,
            session_format::session_output_panel_borders(),
            layout.lines.len(),
            self.scroll_offset,
        );
        let active_loader_area = if session_format::session_output_uses_tachyon_loader(status) {
            layout.active_loader_line_index.and_then(|line_index| {
                Self::indicator_area(output_area, line_index, final_scroll, TACHYON_LOADER_WIDTH)
            })
        } else {
            None
        };
        let transient_loader_area = layout.transient_loader_line_index.and_then(|line_index| {
            Self::indicator_area(output_area, line_index, final_scroll, TACHYON_LOADER_WIDTH)
        });
        let queued_indicator_areas = layout
            .queued_line_indices
            .iter()
            .filter_map(|line_index| {
                Self::indicator_area(output_area, *line_index, final_scroll, QUEUED_ACTION_WIDTH)
            })
            .collect::<Vec<_>>();

        let paint_lines = Self::visible_paint_lines(output_area, &layout.lines, final_scroll);
        let paragraph = Paragraph::new(paint_lines).block(
            Block::default()
                .borders(session_format::session_output_panel_borders())
                .border_style(session_format::session_output_panel_border_style(status)),
        );

        f.render_widget(paragraph, output_area);

        if resolved_layout.show_scrollbar {
            let scrollbar_area = Rect::new(
                output_area
                    .x
                    .saturating_add(output_area.width.saturating_sub(SCROLLBAR_WIDTH)),
                output_area.y.saturating_add(1),
                SCROLLBAR_WIDTH,
                viewport_height,
            );

            VerticalScrollbar::new(final_scroll, layout.lines.len()).render(f, scrollbar_area);
        }

        if let Some(loader_area) = active_loader_area {
            self.apply_tachyon_loader_effect(f.buffer_mut(), loader_area, spinner_frame);
        }
        if let Some(loader_area) = transient_loader_area {
            TachyonLoaderEffect::apply_stateless(f.buffer_mut(), loader_area, spinner_frame);
        }
        for queued_indicator_area in queued_indicator_areas {
            QueuePulseEffect::apply_stateless(f.buffer_mut(), queued_indicator_area, spinner_frame);
        }
    }
}

#[cfg(test)]
#[path = "session_output_test.rs"]
mod tests;

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

use crate::domain::session::{Session, SessionId, Status};
use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::component::vertical_scrollbar::VerticalScrollbar;
#[cfg(test)]
use crate::ui::component::vertical_scrollbar::{SCROLLBAR_THUMB_SYMBOL, SCROLLBAR_TRACK_SYMBOL};
#[cfg(test)]
use crate::ui::icon::Icon;
use crate::ui::icon::TACHYON_LOADER_WIDTH;
use crate::ui::input_layout::{bottom_pinned_scroll_offset, panel_inner_width};
use crate::ui::session_output_assembly::{self, SessionOutputBody, SessionOutputLines};
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
    transient_message_fingerprint: u64,
    transient_message_version: u64,
    transcript: TranscriptFingerprint,
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
    transient_message_fingerprint: u64,
    transient_message_version: u64,
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
    pub(crate) lines: Arc<[Line<'static>]>,
    /// Index of an explicit transient loader row within `lines`, when present.
    pub(crate) transient_loader_line_index: Option<usize>,
}

/// Final session-output layout selected for the current viewport and
/// scrollbar state.
struct SessionOutputResolvedLayout {
    layout: SessionOutputLayout,
    show_scrollbar: bool,
}

/// Cached session-output layout entry.
struct SessionOutputLayoutCacheEntry {
    key: SessionOutputLayoutCacheKey,
    layout: SessionOutputLayout,
}

/// Cached stable output-body entry.
struct SessionOutputBodyCacheEntry {
    body: SessionOutputBody,
    key: SessionOutputBodyCacheKey,
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
    tachyon_loader_effects: RefCell<HashMap<SessionId, TachyonLoaderEffect>>,
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
        let layout = SessionOutput::layout_from_assembled_lines(
            session_output_assembly::layout_from_body(session, context.active_progress, &body),
        );
        self.store_entry(SessionOutputLayoutCacheEntry {
            key,
            layout: layout.clone(),
        });

        layout
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
    spinner_frame: usize,
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
        let output_lines = session_output_assembly::output_lines(
            session,
            inner_width,
            context.active_progress,
            markdown_render_cache,
        );

        Self::layout_from_assembled_lines(output_lines)
    }

    /// Converts pure assembled lines into a cacheable layout for scrolling and
    /// Ratatui painting.
    fn layout_from_assembled_lines(output_lines: SessionOutputLines) -> SessionOutputLayout {
        let line_count = u16::try_from(output_lines.lines.len()).unwrap_or(u16::MAX);

        SessionOutputLayout {
            active_loader_line_index: output_lines.active_loader_line_index,
            line_count,
            lines: Arc::from(output_lines.lines),
            transient_loader_line_index: output_lines.transient_loader_line_index,
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
                session.queued_messages.iter().map(String::as_str),
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
            Self::loader_area(output_area, layout.active_loader_line_index, final_scroll)
        } else {
            None
        };
        let transient_loader_area = Self::loader_area(
            output_area,
            layout.transient_loader_line_index,
            final_scroll,
        );

        let paint_lines = text_util::borrowed_paint_lines(&layout.lines);
        let paragraph = Paragraph::new(paint_lines)
            .block(
                Block::default()
                    .borders(session_format::session_output_panel_borders())
                    .border_style(session_format::session_output_panel_border_style(status)),
            )
            .scroll((final_scroll, 0));

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
    use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
    use crate::domain::theme::ColorTheme;
    use crate::domain::transient_message::{
        TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
        TransientMessageSlot,
    };
    use crate::ui::prompt_block::{self, USER_PROMPT_PREFIX};

    /// Builds one output-line context with defaults suitable for tests.
    fn line_context() -> SessionOutputLineContext<'static> {
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress: None,
            session_update_version: 0,
        }
    }

    /// Posts one focused-review slot for renderer tests.
    fn set_review_transient(session: &mut Session, body: TransientMessageBody) {
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body,
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: session.latest_user_prompt_position(),
        });
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
        let inner_width =
            panel_inner_width(output_area, session_format::session_output_panel_borders());
        session_output_assembly::output_lines(
            session,
            inner_width,
            context.active_progress,
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

    fn set_conversation_transcript(
        session: &mut Session,
        messages: Vec<(SessionMessageKind, &str)>,
    ) {
        let transcript = SessionTranscript::new(
            messages
                .into_iter()
                .enumerate()
                .map(|(position, (kind, content))| {
                    let position = i64::try_from(position).unwrap_or(i64::MAX);
                    if kind.is_conversation_message() {
                        SessionMessage::conversation(position, kind, content)
                    } else {
                        SessionMessage::new(position, kind, content)
                    }
                })
                .collect(),
        );
        session.transcript = Some(transcript);
    }

    #[test]
    fn test_spinner_frame_uses_injected_render_time() {
        // Arrange
        let session = session_fixture();

        // Act
        let output = SessionOutput::new(&session).spinner_frame(42);

        // Assert
        assert_eq!(output.spinner_frame, 42);
    }

    #[test]
    fn test_render_shows_scrollbar_for_overflowing_output() {
        // Arrange
        let mut session = session_fixture();
        let output = (0..40)
            .map(|line_index| format!("output line {line_index}"))
            .collect::<Vec<_>>()
            .join("\n");
        set_assistant_transcript(&mut session, &output);
        let backend = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

        // Act
        terminal
            .draw(|frame| {
                let output = SessionOutput::new(&session).scroll_offset(12);
                output.render(frame, frame.area());
            })
            .expect("failed to draw session output");

        // Assert
        let rendered_text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered_text.contains(SCROLLBAR_TRACK_SYMBOL));
        assert!(rendered_text.contains(SCROLLBAR_THUMB_SYMBOL));
    }

    #[test]
    fn test_scrollbar_layout_reserves_padding_before_track() {
        // Arrange
        let output_area = Rect::new(2, 3, 40, 10);

        // Act
        let content_area = SessionOutput::scrollbar_layout_area(output_area);

        // Assert
        assert_eq!(content_area.x, output_area.x);
        assert_eq!(content_area.y, output_area.y);
        assert_eq!(content_area.width, 38);
        assert_eq!(content_area.height, output_area.height);
    }

    #[test]
    fn test_resolved_layout_keeps_full_width_when_output_fits() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "word word");
        let output_area = Rect::new(0, 0, 9, 3);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context();
        let full_width_layout = SessionOutput::rendered_layout(
            &session,
            output_area,
            context,
            Some(&markdown_render_cache),
            Some(&output_layout_cache),
        );
        let gutter_layout = SessionOutput::rendered_layout(
            &session,
            SessionOutput::scrollbar_layout_area(output_area),
            context,
            Some(&markdown_render_cache),
            Some(&output_layout_cache),
        );
        // Act
        let resolved_layout = SessionOutput::resolved_layout(
            &session,
            output_area,
            full_width_layout.line_count,
            context,
            Some(&markdown_render_cache),
            Some(&output_layout_cache),
        );

        // Assert
        assert!(gutter_layout.line_count > full_width_layout.line_count);
        assert!(!resolved_layout.show_scrollbar);
        assert!(Arc::ptr_eq(
            &resolved_layout.layout.lines,
            &full_width_layout.lines
        ));
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
            5,
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
    fn test_output_layout_cache_tracks_manual_branch_publish_loader() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Publishing review request...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: None,
        });
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();

        // Act
        let layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(),
            Some(&markdown_render_cache),
        );
        let loader_line_index = layout
            .transient_loader_line_index
            .expect("manual publish loader should be tracked through the layout cache");

        // Assert
        assert!(
            layout.lines[loader_line_index]
                .to_string()
                .contains("Publishing review request...")
        );
    }

    /// Verifies workflow-only status changes reuse the stable transcript body
    /// and keep completed output ahead of its summary during a rebase.
    #[test]
    fn test_output_layout_cache_reuses_completed_body_during_rebase() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "implement cache reuse"),
                (
                    SessionMessageKind::AssistantAnswer,
                    "Completed answer stays stable.",
                ),
            ],
        );
        session.status = Status::Review;
        session.summary = Some(summary_fixture());
        session.reconcile_transient_messages();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let review_context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context()
        };
        let review_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            review_context,
            Some(&markdown_render_cache),
        );

        // Act
        session.status = Status::Rebasing;
        let rebase_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_progress: Some("Rebasing branch"),
                session_update_version: 8,
                ..line_context()
            },
            Some(&markdown_render_cache),
        );
        let rebase_text = rebase_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let answer_index = rebase_text
            .find("Completed answer stays stable.")
            .expect("completed answer should remain visible");
        let summary_index = rebase_text
            .find("Change Summary")
            .expect("completed summary should remain visible");

        // Assert
        assert!(!Arc::ptr_eq(&review_layout.lines, &rebase_layout.lines));
        assert_eq!(output_layout_cache.body_entries.borrow().len(), 1);
        assert_eq!(output_layout_cache.entries.borrow().len(), 2);
        assert!(answer_index < summary_index);
        assert!(rebase_text.contains("Rebasing..."));
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
    /// Verifies transient workflow notices invalidate layout cache entries and
    /// render outside the persisted transcript text.
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
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("[Commit] No changes to commit.".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::WorkflowNotice,
            turn_position: None,
        });
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
        assert!(!transcript_text.contains("[Commit] No changes to commit."));
        assert!(!Arc::ptr_eq(&base_layout.lines, &notice_layout.lines));
        assert!(notice_text.contains("[Commit] No changes to commit."));
    }

    #[test]
    fn test_output_layout_cache_keys_review_text() {
        // Arrange
        let mut session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let base_context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context()
        };
        // Act
        let base_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            base_context,
            Some(&markdown_render_cache),
        );
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("## Review\n\n- Cached finding".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
        let review_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            base_context,
            Some(&markdown_render_cache),
        );

        // Assert
        assert!(review_layout.line_count > base_layout.line_count);
        assert!(!Arc::ptr_eq(&base_layout.lines, &review_layout.lines));
    }

    #[test]
    fn test_output_cache_distinguishes_rebuilt_review_states_with_matching_versions() {
        // Arrange
        let mut loading_session = session_fixture();
        loading_session.status = Status::AgentReview;
        loading_session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Reviewing changes".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
        let mut ready_session = session_fixture();
        ready_session.status = Status::Review;
        set_review_transient(
            &mut ready_session,
            TransientMessageBody::Markdown("## Review\n\n- Stable finding".to_string()),
        );
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let loading_then_ready_cache = SessionOutputLayoutCache::default();
        let ready_then_loading_cache = SessionOutputLayoutCache::default();
        let output_area = Rect::new(0, 0, 80, 8);

        // Act
        loading_then_ready_cache.layout(
            &loading_session,
            output_area,
            line_context(),
            Some(&markdown_render_cache),
        );
        let refreshed_ready_layout = loading_then_ready_cache.layout(
            &ready_session,
            output_area,
            line_context(),
            Some(&markdown_render_cache),
        );
        ready_then_loading_cache.layout(
            &ready_session,
            output_area,
            line_context(),
            Some(&markdown_render_cache),
        );
        let regenerated_loading_layout = ready_then_loading_cache.layout(
            &loading_session,
            output_area,
            line_context(),
            Some(&markdown_render_cache),
        );
        let refreshed_ready_text = refreshed_ready_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let regenerated_loading_text = regenerated_loading_layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert_eq!(loading_session.id, ready_session.id);
        assert_eq!(
            loading_session.transient_messages.version(),
            ready_session.transient_messages.version()
        );
        assert!(refreshed_ready_text.contains("Stable finding"));
        assert!(!regenerated_loading_text.contains("Stable finding"));
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
        let output_lines =
            session_output_assembly::output_lines(&session, 78, context.active_progress, None);

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
        session.summary = Some(summary_fixture());
        session.status = Status::Done;
        session.reconcile_transient_messages();
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
        session.summary = Some(summary_fixture());
        session.status = Status::Done;
        session.reconcile_transient_messages();
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
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        session.reconcile_transient_messages();
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
        let transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "summarize merge"),
            SessionMessage::conversation(
                1,
                SessionMessageKind::AssistantAnswer,
                "Assistant output.\n[Merge] this is literal assistant text.",
            ),
        ]);
        let mut session = session_fixture();
        session.summary = Some(summary_fixture());
        session.transcript = Some(transcript);
        session.status = Status::Review;
        session.reconcile_transient_messages();
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
        let transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "previous prompt"),
            SessionMessage::conversation(
                1,
                SessionMessageKind::AssistantAnswer,
                "previous answer\n › quoted assistant marker",
            ),
            SessionMessage::new(
                2,
                SessionMessageKind::WorkflowNotice,
                "\n[Commit] No changes to commit.\n",
            ),
            SessionMessage::conversation(3, SessionMessageKind::UserPrompt, "actual prompt"),
            SessionMessage::conversation(
                4,
                SessionMessageKind::AssistantAnswer,
                "streaming answer\n › quoted active output",
            ),
        ]);

        // Act
        let (completed_turn, active_turn, trailing_notice) =
            session_output_assembly::transcript_section_texts(Status::InProgress, &transcript);

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
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        let review_text = "## Review\n\n### Project Impact\n\n- Documentation-only change.\n\n### \
                           Suggestions\n\n- None.";
        session.reconcile_transient_messages();
        set_review_transient(
            &mut session,
            TransientMessageBody::Markdown(review_text.to_string()),
        );

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
        let review_index = text.find("Review").expect("review should be rendered");
        let merge_error_index = text
            .find("[Merge Error] Cannot merge branch")
            .expect("merge error should be rendered");

        // Assert
        assert!(output_index < summary_index);
        assert!(summary_index < review_index);
        assert!(review_index < merge_error_index);
        assert!(text.contains("Project Impact\n- Documentation-only change."));
        assert!(text.contains("Suggestions\n- None."));
        assert!(!text.contains("type \"/apply\" to verify and apply"));
    }

    /// Verifies post-sync work remains below the durable sync result that
    /// caused it, even while focused review starts in parallel.
    #[test]
    fn test_output_lines_orders_post_sync_statuses_chronologically() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (
                    SessionMessageKind::AssistantAnswer,
                    "implemented the change",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync] Successfully synced wt/session onto origin/main\n",
                ),
            ],
        );
        session.status = Status::Review;
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("[Commit] No changes to commit.".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::WorkflowNotice,
            turn_position: session.latest_user_prompt_position(),
        });
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading(
                "Auto-pushing published branch after completed turn...".to_string(),
            ),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::PublishedBranchSync,
            turn_position: session.latest_user_prompt_position(),
        });
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Markdown(
                "## Review\n\nChronological review result.".to_string(),
            ),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: session.latest_user_prompt_position(),
        });

        // Act
        let text = output_lines(&session, Rect::new(0, 0, 120, 12), line_context(), None)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit notice should render");
        let sync_index = text
            .find("[Sync] Successfully synced")
            .expect("sync result should render");
        let auto_push_index = text
            .find("Auto-pushing published branch")
            .expect("auto-push status should render");
        let review_index = text
            .find("Chronological review result.")
            .expect("focused-review status should render");

        // Assert
        assert!(commit_index < sync_index);
        assert!(sync_index < auto_push_index);
        assert!(auto_push_index < review_index);
    }

    /// Verifies focused-review failures remain visible after the transient
    /// loading status returns to `Review`.
    #[test]
    fn test_output_lines_review_session_shows_review_status_message_when_text_missing() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        let review_status_message = "Review assist unavailable: empty provider response";
        set_review_transient(
            &mut session,
            TransientMessageBody::Plain(review_status_message.to_string()),
        );

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(review_status_message));
    }

    /// Verifies a manual review-request publish renders as an animated
    /// session-chat row instead of requiring a modal loading popup.
    #[test]
    fn test_output_lines_tracks_manual_branch_publish_loader() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Publishing review request...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: None,
        });

        // Act
        let lines = session_output_assembly::output_lines(&session, 78, None, None);
        let loader_line_index = lines
            .transient_loader_line_index
            .expect("manual publish loader should be tracked");

        // Assert
        assert!(
            lines.lines[loader_line_index]
                .to_string()
                .contains("Publishing review request...")
        );
    }

    /// Verifies an orchestrator renders one animated child-status loader.
    #[test]
    fn test_output_lines_tracks_orchestration_loader() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading(
                "Orchestrating...\n- Protocol: running\n- UI: waiting".to_string(),
            ),
            lifecycle: crate::domain::transient_message::TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::Orchestration,
            turn_position: None,
        });

        // Act
        let lines = session_output_assembly::output_lines(&session, 78, None, None);
        let loader_line_index = lines
            .transient_loader_line_index
            .expect("orchestration loader should be tracked");

        // Assert
        assert!(
            lines.lines[loader_line_index]
                .to_string()
                .contains("Orchestrating...")
        );
        assert!(
            lines.lines[loader_line_index + 1]
                .to_string()
                .contains("- Protocol: running")
        );
        assert!(
            lines.lines[loader_line_index + 2]
                .to_string()
                .contains("- UI: waiting")
        );
    }

    /// Verifies persisted review-request creation renders as one logical
    /// transcript line.
    #[test]
    fn test_output_lines_renders_persisted_review_request_on_one_line() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::InProgress;
        let created_message =
            "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42";
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::AssistantAnswer, "Published the changes."),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Review Request] Created PR \
                     https://github.com/agentty-xyz/agentty/pull/42\n",
                ),
                (SessionMessageKind::UserPrompt, "continue the session"),
            ],
        );

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 120, 8), line_context(), None);

        // Assert
        let created_message_index = lines
            .iter()
            .position(|line| line.to_string() == created_message)
            .expect("review request notice should be rendered");
        let later_prompt_index = lines
            .iter()
            .position(|line| line.to_string().contains("continue the session"))
            .expect("later user prompt should be rendered");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.to_string() == created_message)
                .count(),
            1
        );
        assert!(created_message_index < later_prompt_index);
    }

    /// Verifies persisted message padding cannot add extra rows to the
    /// canonical one-empty-line transcript gap.
    #[test]
    fn test_output_lines_places_one_empty_line_between_messages() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        let visible_messages = [
            "Completed turn.",
            "[Commit] No changes to commit.",
            "[Review Request] Created PR 42",
            "[Sync] Successfully synced onto main",
            "[Branch Push] Auto-pushed published branch.",
            "queued › Verify the spacing.",
            "queued › Keep one empty line.",
        ];
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::AssistantAnswer, visible_messages[0]),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Review Request] Created PR 42\n",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync] Successfully synced onto main\n",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Branch Push] Auto-pushed published branch.\n",
                ),
            ],
        );
        session.queued_messages = vec![
            "\nVerify the spacing.\n".to_string(),
            " \nKeep one empty line.\n\t".to_string(),
        ];

        // Act
        let rendered_lines = output_lines(&session, Rect::new(0, 0, 120, 16), line_context(), None)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let message_rows = visible_messages
            .iter()
            .map(|message| {
                rendered_lines
                    .iter()
                    .position(|line| line == message)
                    .expect("message should be rendered")
            })
            .collect::<Vec<_>>();

        // Assert
        assert!(message_rows.windows(2).all(|rows| rows[1] == rows[0] + 2));
    }

    /// Verifies queued-message edge trimming preserves blank lines within a
    /// multiline message.
    #[test]
    fn test_append_queued_message_lines_trims_only_outer_empty_lines() {
        // Arrange
        let mut lines = vec![Line::from("Previous message.")];
        let queued_messages = vec![
            "\nFirst queued message.\n".to_string(),
            " \nSecond queued message.\n\nMore context.\n\t".to_string(),
        ];

        // Act
        session_output_assembly::append_queued_message_lines(&mut lines, &queued_messages);
        let rendered_lines = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

        // Assert
        assert_eq!(
            rendered_lines,
            vec![
                "Previous message.",
                "",
                "queued › First queued message.",
                "",
                "queued › Second queued message.",
                "        ",
                "        More context.",
                "",
            ]
        );
    }

    /// Verifies prompt edge trimming keeps internal blank rows while
    /// retaining one separator before the following transcript message.
    #[test]
    fn test_output_lines_trims_only_outer_user_prompt_empty_lines() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        set_conversation_transcript(
            &mut session,
            vec![
                (
                    SessionMessageKind::UserPrompt,
                    "\nfollow up\n\nwith context\n",
                ),
                (SessionMessageKind::AssistantAnswer, "Completed response."),
            ],
        );

        // Act
        let rendered_lines = output_lines(&session, Rect::new(0, 0, 120, 16), line_context(), None);
        let prompt_line_index = rendered_lines
            .iter()
            .position(|line| line.to_string().contains("follow up"))
            .expect("prompt should be rendered");
        let context_line_index = rendered_lines
            .iter()
            .position(|line| line.to_string().contains("with context"))
            .expect("prompt context should be rendered");
        let response_line_index = rendered_lines
            .iter()
            .position(|line| line.to_string() == "Completed response.")
            .expect("response should be rendered");

        // Assert
        assert_eq!(prompt_line_index, 1);
        assert_eq!(context_line_index, prompt_line_index + 2);
        assert!(rendered_lines[prompt_line_index + 1].width() > 0);
        assert!(
            rendered_lines[prompt_line_index + 1]
                .to_string()
                .trim()
                .is_empty()
        );
        assert_eq!(response_line_index, context_line_index + 3);
        assert!(rendered_lines[context_line_index + 1].width() > 0);
        assert_eq!(rendered_lines[context_line_index + 2].width(), 0);
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
        session.reconcile_transient_messages();
        // Act
        let lines = session_output_assembly::output_lines(&session, 78, None, None);
        let text = lines
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert_eq!(lines.transient_loader_line_index, None);
        assert!(text.contains("[Branch Push]"));
        assert_eq!(
            text.matches("Auto-pushed published branch after completed turn.")
                .count(),
            1
        );
    }

    /// Verifies focused-review fallback text is rendered literally instead of
    /// being interpreted as markdown.
    #[test]
    fn test_output_lines_review_status_message_preserves_markdown_characters() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        let review_status_message = "# Review *failed* for `tool`";
        set_review_transient(
            &mut session,
            TransientMessageBody::Plain(review_status_message.to_string()),
        );

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(review_status_message));
    }

    #[test]
    fn test_output_lines_review_session_appends_structured_summary() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "implemented the feature");
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        session.reconcile_transient_messages();
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
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        session.reconcile_transient_messages();
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

    /// Verifies old summaries are hidden once a reply prompt is running.
    #[test]
    fn test_output_lines_in_progress_session_hides_summary_before_active_prompt() {
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
        session.summary = Some(summary_fixture());
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

        // Assert
        assert!(!text.contains("Change Summary"));
        assert!(commit_index < prompt_index);
    }

    /// Verifies queued follow-up messages render after the active turn while
    /// keeping stale completed-turn summaries hidden.
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
        session.summary = Some(summary_fixture());
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

        // Assert
        assert!(!text.contains("Change Summary"));
        assert!(commit_index < prompt_index);
        assert!(prompt_index < queued_index);
        assert!(text.contains("        with context"));
    }

    #[test]
    fn test_output_lines_in_progress_session_places_notice_after_active_turn() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "previous turn"),
                (SessionMessageKind::AssistantAnswer, "previous answer"),
                (SessionMessageKind::UserPrompt, "current turn"),
                (SessionMessageKind::AssistantAnswer, "working"),
            ],
        );
        session.status = Status::InProgress;
        session.queued_messages = vec!["queued follow-up".to_string()];
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterActiveTurn,
            body: TransientMessageBody::Markdown(
                "[Sync] Queued until the current turn finishes.".to_string(),
            ),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::WorkflowNotice,
            turn_position: session.latest_user_prompt_position(),
        });

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let previous_answer_index = text
            .find("previous answer")
            .expect("previous answer should be rendered");
        let current_turn_index = text
            .find("current turn")
            .expect("current turn should be rendered");
        let notice_index = text
            .find("[Sync] Queued until the current turn finishes.")
            .expect("queued sync notice should be rendered");
        let queued_message_index = text
            .find("queued › queued follow-up")
            .expect("queued follow-up should be rendered");

        // Assert
        assert!(previous_answer_index < current_turn_index);
        assert!(current_turn_index < notice_index);
        assert!(notice_index < queued_message_index);
    }

    /// Verifies a queued follow-up remains below workflow notices that were
    /// already visible when a session sync accepted the message.
    #[test]
    fn test_output_lines_rebasing_session_places_queued_message_after_workflow_notices() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Commit] No changes to commit.\n",
                ),
                (
                    SessionMessageKind::WorkflowNotice,
                    "\n[Sync Assist] Attempt 1/3. Resolving conflicts.\n",
                ),
            ],
        );
        session.status = Status::Rebasing;
        session.queued_messages = vec!["address review comments".to_string()];

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 12), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let commit_index = text
            .find("[Commit] No changes to commit.")
            .expect("commit notice should be rendered");
        let sync_assist_index = text
            .find("[Sync Assist] Attempt 1/3.")
            .expect("sync-assist notice should be rendered");
        let queued_message_index = text
            .find("queued › address review comments")
            .expect("queued follow-up should be rendered");

        // Assert
        assert!(commit_index < sync_assist_index);
        assert!(sync_assist_index < queued_message_index);
    }

    /// Verifies an active prompt at the start of the transcript hides stale
    /// summary content.
    #[test]
    fn test_output_lines_in_progress_single_prompt_hides_summary() {
        // Arrange
        let mut session = session_fixture();
        set_conversation_transcript(
            &mut session,
            vec![
                (SessionMessageKind::UserPrompt, "add hello world"),
                (
                    SessionMessageKind::AssistantAnswer,
                    "I added the README change.",
                ),
            ],
        );
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some(" › add hello world\n\n"),
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
            .find(" › add hello world")
            .expect("prompt should be rendered");
        let answer_index = text
            .find("I added the README change.")
            .expect("answer should be rendered");

        // Assert
        assert!(!text.contains("Change Summary"));
        assert!(prompt_index < answer_index);
    }

    /// Verifies the latest user prompt is detected when the exact active
    /// prompt capture is unavailable.
    #[test]
    fn test_output_lines_in_progress_without_active_capture_hides_summary_before_last_prompt() {
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
        session.summary = Some(summary_fixture());
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

        // Assert
        assert!(!text.contains("Change Summary"));
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
        session.summary = Some(summary_fixture());
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
        assert!(!text.contains("Change Summary"));
        assert!(prompt_index < quoted_output_index);
    }

    #[test]
    fn test_output_lines_review_session_without_summary_keeps_transcript_only() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "implemented the feature");
        session.status = Status::Review;
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session_output_assembly::append_user_prompt_markdown_lines(&mut lines, "one two", 10, None);
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
        session_output_assembly::append_user_prompt_markdown_lines(&mut lines, prompt, 36, None);
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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
        session.reconcile_transient_messages();
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

    #[test]
    fn test_output_lines_render_cyclic_mermaid_flowchart() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(
            &mut session,
            concat!(
                "```mermaid\n",
                "flowchart LR\n",
                "    U[User and TUI] --> C[Orchestrator controller]\n",
                "    M[Agent model] --> P[Typed command response]\n",
                "    P --> C\n",
                "    C --> S[ag-session service]\n",
                "    S --> A[Agentty host adapter]\n",
                "    A --> W[Session workers]\n",
                "    W --> E[Session events]\n",
                "    E --> C\n",
                "    C --> M\n",
                "```",
            ),
        );
        session.status = Status::Review;
        session.reconcile_transient_messages();

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 48), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Orchestrator controller"));
        assert!(text.contains("Session events"));
        assert!(text.contains("Session events ───▶ Orchestrator controller"));
        assert!(text.contains("Orchestrator controller ───▶ Agent model"));
        assert!(!text.contains("flowchart LR"));
    }

    /// Verifies the done-summary transition renders the rewritten summary
    /// payload while preserving the completed transcript.
    #[test]
    fn test_output_lines_done_summary_transition_preserves_transcript() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "streamed output");
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        session.reconcile_transient_messages();
        let review_lines = output_lines(&session, Rect::new(0, 0, 80, 8), line_context(), None);
        let review_text = review_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        session.summary = Some(
            "# Summary\n\nSession now greets users on startup.\n\n# Commit\n\nRefine session \
             summary"
                .to_string(),
        );
        session.status = Status::Done;
        session.reconcile_transient_messages();
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
    fn test_output_lines_agent_review_mode_shows_assisted_text() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::AgentReview;
        let assisted_text = "## Review\n\n- Focused finding";
        set_review_transient(
            &mut session,
            TransientMessageBody::Markdown(assisted_text.to_string()),
        );

        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Focused finding"));
        assert!(!text.contains("Review is not available."));
    }

    #[test]
    fn test_output_lines_uses_transcript_for_canceled_session() {
        // Arrange
        let mut session = session_fixture();
        set_assistant_transcript(&mut session, "streamed output");
        session.summary = Some(summary_fixture());
        session.status = Status::Canceled;
        session.reconcile_transient_messages();
        // Act
        let lines = output_lines(&session, Rect::new(0, 0, 80, 5), line_context(), None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(!text.contains("Added the structured protocol summary."));
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

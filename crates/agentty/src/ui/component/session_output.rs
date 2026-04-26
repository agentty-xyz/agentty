use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::hash::Hasher;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use rustc_hash::FxHasher;

use crate::app;
use crate::domain::session::{Session, SessionId, Status};
use crate::icon::{Icon, TACHYON_LOADER_WIDTH};
use crate::ui::component::tachyon_loader::TachyonLoaderEffect;
use crate::ui::markdown::{self, render_markdown};
use crate::ui::util::{bottom_pinned_scroll_offset, panel_inner_width};
use crate::ui::{Component, layout, style, text_util};

const DRAFT_PREVIEW_HEADER: &str = "## Draft Session";
const DRAFT_PREVIEW_EMPTY_NOTE: &str = "No draft messages staged yet. Use `Enter` to stage the \
                                        first draft locally, then press `s` in session view to \
                                        start the bundle.";
const DRAFT_PREVIEW_STAGED_NOTE: &str =
    "Draft messages stay local until you press `s` in session view to start the staged bundle.";
const TRANSCRIPT_FOOTER_PREFIXES: &[&str] = &["[Commit]", "[Commit Error]"];
const USER_PROMPT_PREFIX: &str = " › ";
/// User prompt prefix when the prompt starts after a transcript newline.
const USER_PROMPT_LINE_PREFIX: &str = "\n › ";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";
const SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT: usize = 16;

/// Cache key for one fully assembled session-output layout.
///
/// The key is intentionally tied to the session identifier plus observable
/// update version and `updated_at` timestamp instead of hashing the full
/// transcript on every frame. Width, active prompt, review text/status,
/// progress text, and markdown style version cover the transient inputs that
/// can alter rendered lines without changing the stored session row.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionOutputLayoutCacheKey {
    active_progress: TextFingerprint,
    active_prompt_output: TextFingerprint,
    draft_prompt: TextFingerprint,
    markdown_render_version: u64,
    output_width: u16,
    review_status_message: TextFingerprint,
    review_text: TextFingerprint,
    session_id: SessionId,
    session_update_version: u64,
    session_updated_at: i64,
    status: Status,
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
}

/// Cached result for one fully assembled session-output layout.
#[derive(Clone)]
pub(crate) struct SessionOutputLayout {
    /// Index of the active Tachyon loader row within `lines`, when present.
    pub(crate) active_loader_line_index: Option<usize>,
    /// Number of rendered lines, saturated for scroll metric arithmetic.
    pub(crate) line_count: u16,
    /// Index of the published-branch sync loader row within `lines`, when
    /// present.
    pub(crate) published_loader_line_index: Option<usize>,
    /// Rendered lines shared between scroll metrics and frame painting.
    pub(crate) lines: Arc<[Line<'static>]>,
}

/// Fully assembled session-output lines plus metadata derived during assembly.
struct SessionOutputLines {
    active_loader_line_index: Option<usize>,
    lines: Vec<Line<'static>>,
    published_loader_line_index: Option<usize>,
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
/// width, active prompt, review text/status, and progress text. Entries are
/// invalidated by key changes; the markdown render-cache version is part of the
/// key so style-bearing lines are not reused after markdown cache invalidation.
/// Per-session Tachyonfx state is bounded by the same layout LRU and is
/// removed once no cached layout remains for that session.
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
    review_status_message: Option<&'a str>,
    review_text: Option<&'a str>,
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
    /// Review fallback text shown while review output is unavailable.
    pub(crate) review_status_message: Option<&'a str>,
    /// Review markdown generated by the agent, when available.
    pub(crate) review_text: Option<&'a str>,
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
            review_status_message: None,
            review_text: None,
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

    /// Sets the review status message rendered when review text is not
    /// available.
    #[must_use]
    pub fn review_status_message(mut self, status_message: Option<&'a str>) -> Self {
        self.review_status_message = status_message;
        self
    }

    /// Sets review text generated by an agent.
    #[must_use]
    pub fn review_text(mut self, review_text: Option<&'a str>) -> Self {
        self.review_text = review_text;
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
            published_loader_line_index: output_lines.published_loader_line_index,
            lines: Arc::<[Line<'static>]>::from(output_lines.lines),
        }
    }

    /// Builds the cache key for a fully assembled session-output layout.
    fn layout_cache_key(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_version: u64,
    ) -> SessionOutputLayoutCacheKey {
        let inner_width = panel_inner_width(output_area, layout::session_output_panel_borders());

        SessionOutputLayoutCacheKey {
            active_progress: TextFingerprint::from_text(context.active_progress),
            active_prompt_output: TextFingerprint::from_text(context.active_prompt_output),
            draft_prompt: Self::draft_prompt_fingerprint(session),
            markdown_render_version,
            output_width: u16::try_from(inner_width).unwrap_or(u16::MAX),
            review_status_message: TextFingerprint::from_text(context.review_status_message),
            review_text: TextFingerprint::from_text(context.review_text),
            session_id: session.id.clone(),
            session_update_version: context.session_update_version,
            session_updated_at: session.updated_at,
            status: session.status,
        }
    }

    /// Returns the staged-draft prompt identity when the draft preview reads
    /// from `session.prompt`.
    fn draft_prompt_fingerprint(session: &Session) -> TextFingerprint {
        if session.status == Status::New && session.is_draft_session() {
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
    /// block. When a new prompt is active, the previous-turn summary is hidden
    /// so the output stream no longer shows stale change metadata for work
    /// that is already being superseded.
    /// Queued follow-up messages render beneath the running turn so users can
    /// see staged local input without mixing it into completed transcript
    /// content. Focused-review output is appended for non-terminal review
    /// states so terminal views can keep their final transcript and summary
    /// display stable.
    fn output_lines_with_metadata(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> SessionOutputLines {
        let SessionOutputLineContext {
            active_prompt_output,
            active_progress,
            review_status_message,
            review_text,
            session_update_version: _,
        } = context;
        let status = session.status;
        let mut active_loader_line_index = None;
        let mut published_loader_line_index = None;
        let output_text = Self::output_text(session);
        let (completed_turn_text, active_turn_text) =
            Self::transcript_sections(status, output_text.as_ref(), active_prompt_output);
        let completed_turn_text =
            layout::session_output_text_with_spaced_user_input(completed_turn_text);
        let active_turn_text =
            active_turn_text.map(layout::session_output_text_with_spaced_user_input);
        let inner_width = panel_inner_width(output_area, layout::session_output_panel_borders());
        let (completed_turn_text, trailing_footer_text) =
            text_util::split_trailing_line_block(&completed_turn_text, TRANSCRIPT_FOOTER_PREFIXES);
        let mut lines = Vec::new();
        Self::append_markdown_lines(
            &mut lines,
            completed_turn_text,
            inner_width,
            markdown_render_cache,
        );
        let shows_summary_block = Self::shows_summary_block(
            session.status,
            active_prompt_output,
            active_turn_text.as_deref(),
        );
        if shows_summary_block {
            Self::append_summary_lines(
                &mut lines,
                session.summary.as_deref(),
                inner_width,
                markdown_render_cache,
            );
        }
        Self::append_transcript_footer_lines(
            &mut lines,
            trailing_footer_text,
            inner_width,
            markdown_render_cache,
        );
        Self::append_active_turn_lines(
            &mut lines,
            active_turn_text.as_deref(),
            inner_width,
            markdown_render_cache,
        );
        Self::append_queued_message_lines(&mut lines, &session.queued_messages);
        if Self::shows_review_lines(session.status, review_status_message, review_text) {
            Self::append_review_lines(
                &mut lines,
                review_status_message,
                review_text,
                inner_width,
                markdown_render_cache,
            );
        }
        if Self::append_published_branch_sync_lines(&mut lines, session) {
            published_loader_line_index = Some(lines.len().saturating_sub(1));
        }

        if let Some(status_line) =
            layout::session_output_status_line(status, active_progress, review_status_message)
        {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }

            lines.push(Line::from(""));
            if Self::status_uses_tachyon_loader(status) {
                active_loader_line_index = Some(lines.len());
            }
            lines.push(status_line);
        } else if status == Status::Done {
            lines.push(Line::from(""));
            lines.push(layout::session_output_done_line());
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(""));
        }

        SessionOutputLines {
            active_loader_line_index,
            lines,
            published_loader_line_index,
        }
    }

    /// Appends one automatic published-branch sync status row when the latest
    /// completed turn started, finished, or failed an auto-push.
    fn append_published_branch_sync_lines(
        lines: &mut Vec<Line<'static>>,
        session: &Session,
    ) -> bool {
        let Some(sync_line) = layout::session_output_published_branch_sync_line(session) else {
            return false;
        };

        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }

        lines.push(Line::from(""));
        lines.push(sync_line);

        true
    }

    /// Splits the transcript before the current active-turn prompt block.
    ///
    /// This keeps the current prompt-led block separate when it has already
    /// been appended to the persisted transcript.
    fn transcript_sections<'text>(
        status: Status,
        output_text: &'text str,
        active_prompt_output: Option<&str>,
    ) -> (&'text str, Option<&'text str>) {
        if !matches!(
            status,
            Status::InProgress | Status::Queued | Status::Rebasing | Status::Merging
        ) {
            return (output_text, None);
        }
        let Some(active_prompt_start) =
            Self::active_prompt_start(output_text, active_prompt_output)
        else {
            return (output_text, None);
        };
        (
            &output_text[..active_prompt_start],
            Some(&output_text[active_prompt_start..]),
        )
    }

    /// Returns the byte index where the active prompt block starts.
    ///
    /// The exact captured prompt block is preferred because it distinguishes
    /// user prompts from assistant text that happens to contain prompt-like
    /// markers. When that in-memory capture is unavailable, the renderer falls
    /// back to the last persisted user prompt marker so previous-turn summaries
    /// still render above the latest prompt after reloads or snapshot misses.
    fn active_prompt_start(output_text: &str, active_prompt_output: Option<&str>) -> Option<usize> {
        active_prompt_output
            .filter(|prompt_output| !prompt_output.is_empty())
            .and_then(|prompt_output| output_text.rfind(prompt_output))
            .or_else(|| Self::last_user_prompt_start(output_text))
    }

    /// Returns the byte index of the last line that starts a persisted user
    /// prompt block.
    fn last_user_prompt_start(output_text: &str) -> Option<usize> {
        output_text
            .rfind(USER_PROMPT_LINE_PREFIX)
            .map(|index| index + 1)
            .or_else(|| output_text.starts_with(USER_PROMPT_PREFIX).then_some(0))
    }

    /// Returns the source text shown in the output panel for the current
    /// status.
    ///
    /// New draft sessions render staged-draft guidance before their first
    /// live turn starts. Active and canceled sessions otherwise render the
    /// captured transcript output. Done sessions keep transcript and summary
    /// rendered together so both completed output and persisted summary remain
    /// visible.
    fn output_text(session: &Session) -> Cow<'_, str> {
        match session.status {
            Status::New if session.is_draft_session() => {
                return Cow::Owned(Self::render_draft_session_preview(session));
            }
            Status::Canceled => {
                return Cow::Borrowed(session.output.as_str());
            }
            Status::Review
            | Status::AgentReview
            | Status::Question
            | Status::New
            | Status::Done
            | Status::InProgress
            | Status::Queued
            | Status::Rebasing
            | Status::Merging => {}
        }

        Cow::Borrowed(session.output.as_str())
    }

    /// Renders the staged-draft guidance shown while a draft session remains
    /// in `New`.
    fn render_draft_session_preview(session: &Session) -> String {
        let mut output = String::from(DRAFT_PREVIEW_HEADER);

        if session.has_staged_drafts() {
            let _ = write!(output, "\n\n{DRAFT_PREVIEW_STAGED_NOTE}\n\n");
            output.push_str(&Self::staged_draft_transcript_block(&session.prompt));
        } else {
            let _ = write!(output, "\n\n{DRAFT_PREVIEW_EMPTY_NOTE}\n");
        }

        output
    }

    /// Formats the staged draft-session prompt using the same transcript
    /// prompt markers used for persisted user-turn output.
    fn staged_draft_transcript_block(prompt_text: &str) -> String {
        let prompt_lines = prompt_text.split('\n').collect::<Vec<_>>();
        let mut formatted_lines = Vec::with_capacity(prompt_lines.len());

        for (index, prompt_line) in prompt_lines.into_iter().enumerate() {
            let prefix = if index == 0 {
                USER_PROMPT_PREFIX
            } else {
                USER_PROMPT_CONTINUATION_PREFIX
            };

            formatted_lines.push(format!("{prefix}{prompt_line}"));
        }

        format!("{}\n\n", formatted_lines.join("\n"))
    }

    /// Returns whether the output panel should append the structured summary
    /// block outside the persisted transcript string.
    ///
    /// Summary markdown is rendered alongside transcript output for completed
    /// turns. Once a new prompt is active, the old summary is hidden until the
    /// next turn finishes and produces fresh summary metadata.
    /// Canceled sessions keep the raw transcript visible so interrupted turns
    /// do not render synthetic summary content that was never finalized.
    fn shows_summary_block(
        status: Status,
        active_prompt_output: Option<&str>,
        active_turn_text: Option<&str>,
    ) -> bool {
        if status == Status::Canceled {
            return false;
        }

        active_prompt_output.is_none() && active_turn_text.is_none()
    }

    /// Returns whether focused-review output belongs in a non-terminal session
    /// view.
    ///
    /// Terminal session views own their final summary or transcript display, so
    /// they intentionally avoid appending transient focused-review summaries.
    fn shows_review_lines(
        status: Status,
        review_status_message: Option<&str>,
        review_text: Option<&str>,
    ) -> bool {
        if matches!(status, Status::Done | Status::Canceled) {
            return false;
        }

        review_status_message
            .map(str::trim)
            .is_some_and(|status_message| !status_message.is_empty())
            || review_text
                .map(str::trim)
                .is_some_and(|review_text| !review_text.is_empty())
    }

    /// Appends focused-review output or non-loading fallback text to the
    /// transcript for the current session view.
    ///
    /// The `### Suggestions` header is annotated with a `(type "/apply" to
    /// verify and apply)` hint at render time so users discover the apply
    /// shortcut without polluting the persisted review markdown consumed by
    /// suggestion extraction.
    fn append_review_lines(
        lines: &mut Vec<Line<'static>>,
        review_status_message: Option<&str>,
        review_text: Option<&str>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        if let Some(review_markdown) = review_text
            .map(str::trim)
            .filter(|review_text| !review_text.is_empty())
            .map(layout::annotate_review_suggestions_header)
        {
            Self::append_markdown_lines(
                lines,
                &review_markdown,
                inner_width,
                markdown_render_cache,
            );

            return;
        }

        if let Some(status_message) = Self::visible_review_status_message(review_status_message) {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }

            if !lines.is_empty() {
                lines.push(Line::from(""));
            }

            Self::append_plain_review_status_lines(lines, status_message, inner_width);
        }
    }

    /// Returns non-loading focused-review fallback text suitable for plain
    /// rendering.
    fn visible_review_status_message(review_status_message: Option<&str>) -> Option<&str> {
        review_status_message
            .map(str::trim)
            .filter(|status_message| !status_message.is_empty())
            .filter(|status_message| !app::is_review_loading_status_message(status_message))
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

    /// Appends a rendered structured-summary section without mutating the
    /// persisted transcript string.
    fn append_summary_lines(
        lines: &mut Vec<Line<'static>>,
        summary_text: Option<&str>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        let Some(summary_text) = summary_text else {
            return;
        };
        if summary_text.trim().is_empty() {
            return;
        }

        Self::append_markdown_lines(
            lines,
            &layout::session_output_summary_markdown(summary_text),
            inner_width,
            markdown_render_cache,
        );
    }

    /// Appends one trailing transcript footer after any summary produced for
    /// the completed turn when a known footer block is present.
    fn append_transcript_footer_lines(
        lines: &mut Vec<Line<'static>>,
        trailing_footer_text: Option<&str>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        let Some(trailing_footer_text) = trailing_footer_text else {
            return;
        };

        Self::append_markdown_lines(
            lines,
            trailing_footer_text,
            inner_width,
            markdown_render_cache,
        );
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

        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }
        lines.push(Line::from(""));

        let queued_style = ratatui::style::Style::default()
            .fg(style::palette::text_subtle())
            .add_modifier(ratatui::style::Modifier::ITALIC);
        for queued_text in queued_messages {
            let trimmed = queued_text.trim();
            if trimmed.is_empty() {
                continue;
            }
            for (line_index, message_line) in trimmed.split('\n').enumerate() {
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

    /// Appends the currently active prompt-led transcript block after earlier
    /// transcript content so the rendered transcript remains chronological.
    fn append_active_turn_lines(
        lines: &mut Vec<Line<'static>>,
        active_turn_text: Option<&str>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        let Some(active_turn_text) = active_turn_text else {
            return;
        };
        if active_turn_text.trim().is_empty() {
            return;
        }

        Self::append_markdown_lines(lines, active_turn_text, inner_width, markdown_render_cache);
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

        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }

        if !lines.is_empty() {
            lines.push(Line::from(""));
        }

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

    /// Builds short-lived paint lines that borrow span text from cached
    /// `Line<'static>` entries instead of cloning owned strings.
    ///
    /// `Paragraph` takes ownership of a `Vec<Line<'a>>`, so this still creates
    /// lightweight line/span containers for the current paint pass, but each
    /// span content becomes `Cow::Borrowed` and avoids allocating transcript
    /// strings on every frame.
    fn borrowed_paint_lines<'line>(lines: &'line [Line<'static>]) -> Vec<Line<'line>> {
        lines
            .iter()
            .map(|line| Line {
                alignment: line.alignment,
                spans: line.spans.iter().map(Self::borrowed_paint_span).collect(),
                style: line.style,
            })
            .collect()
    }

    /// Builds one borrowed paint span from a cached static span.
    fn borrowed_paint_span<'span>(span: &'span Span<'static>) -> Span<'span> {
        Span {
            content: Cow::Borrowed(span.content.as_ref()),
            style: span.style,
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
            Status::InProgress | Status::AgentReview | Status::Rebasing | Status::Merging
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
                review_status_message: self.review_status_message,
                review_text: self.review_text,
                session_update_version: self.session_update_version,
            },
            self.markdown_render_cache,
            self.output_layout_cache,
        );
        let final_scroll = bottom_pinned_scroll_offset(
            output_area,
            layout::session_output_panel_borders(),
            layout.lines.len(),
            self.scroll_offset,
        );
        let active_loader_area = if Self::status_uses_tachyon_loader(status) {
            Self::loader_area(output_area, layout.active_loader_line_index, final_scroll)
        } else {
            None
        };
        let published_loader_area = Self::loader_area(
            output_area,
            layout.published_loader_line_index,
            final_scroll,
        );

        let paint_lines = SessionOutput::borrowed_paint_lines(&layout.lines);
        let paragraph = Paragraph::new(paint_lines)
            .block(
                Block::default()
                    .borders(layout::session_output_panel_borders())
                    .border_style(layout::session_output_panel_border_style(status)),
            )
            .scroll((final_scroll, 0));

        f.render_widget(paragraph, output_area);

        if let Some(loader_area) = active_loader_area {
            self.apply_tachyon_loader_effect(f.buffer_mut(), loader_area, spinner_frame);
        }
        if let Some(loader_area) = published_loader_area {
            TachyonLoaderEffect::apply_stateless(f.buffer_mut(), loader_area, spinner_frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Alignment;
    use ratatui::style::Style;
    use serde_json;

    use super::*;
    use crate::infra::agent::protocol::AgentResponseSummary;

    /// Builds one output-line context with defaults suitable for tests.
    fn line_context<'a>(
        review_status_message: Option<&'a str>,
        review_text: Option<&'a str>,
        active_progress: Option<&'a str>,
    ) -> SessionOutputLineContext<'a> {
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress,
            review_status_message,
            review_text,
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
        crate::domain::session::tests::SessionFixtureBuilder::new()
            .status(Status::New)
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

    #[test]
    fn test_rendered_line_count_counts_wrapped_content() {
        // Arrange
        let mut session = session_fixture();
        session.output = "word ".repeat(40);
        let raw_line_count = u16::try_from(session.output.lines().count()).unwrap_or(u16::MAX);
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();

        // Act
        let rendered_line_count = SessionOutput::rendered_line_count(
            &session,
            20,
            line_context(None, None, None),
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
        session.output = "## Heading\n\ncached body".to_string();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context(None, None, None)
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
    fn test_output_layout_cache_keys_staged_draft_prompt() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let context = line_context(None, None, None);

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
    fn test_output_layout_cache_keys_review_text() {
        // Arrange
        let session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let base_context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context(None, None, None)
        };
        let review_context = SessionOutputLineContext {
            review_text: Some("## Review\n\n- Cached finding"),
            ..base_context
        };

        // Act
        let base_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            base_context,
            Some(&markdown_render_cache),
        );
        let review_layout = output_layout_cache.layout(
            &session,
            Rect::new(0, 0, 80, 8),
            review_context,
            Some(&markdown_render_cache),
        );

        // Assert
        assert!(review_layout.line_count > base_layout.line_count);
        assert!(!Arc::ptr_eq(&base_layout.lines, &review_layout.lines));
    }

    #[test]
    fn test_output_layout_cache_reuses_active_loader_layout_across_frames() {
        // Arrange
        let mut session = session_fixture();
        session.output = "active output".to_string();
        session.status = Status::InProgress;
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let first_frame_context = line_context(None, None, None);

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
            session.output = format!("active output {session_index}");
            session.status = Status::InProgress;

            output_layout_cache.layout(
                &session,
                Rect::new(0, 0, 80, 8),
                line_context(None, None, None),
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
        session.output = format!("{} pasted transcript glyph", Icon::TachyonLoader);
        session.status = Status::InProgress;
        let context = line_context(None, None, None);

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
    fn test_output_text_borrows_transcript_for_output_mode() {
        // Arrange
        let mut session = session_fixture();
        session.output = "borrowed transcript".to_string();

        // Act
        let output_text = SessionOutput::output_text(&session);

        // Assert
        assert!(matches!(output_text, Cow::Borrowed("borrowed transcript")));
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
        let paint_lines = SessionOutput::borrowed_paint_lines(&cached_lines);

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
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
            None,
        );
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
    fn test_output_lines_render_empty_draft_preview_for_new_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
            None,
        );
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
    fn test_output_lines_done_output_mode_appends_structured_summary() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output\n\n[Commit] No changes to commit.\n".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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

    /// Verifies a terminal `Done` session keeps its final summary without
    /// appending transient focused-review output.
    #[test]
    fn test_output_lines_done_session_hides_review_text_when_available() {
        // Arrange
        let mut session = session_fixture();
        session.summary = Some("# Summary\n\nMerged session work.".to_string());
        session.status = Status::Done;
        let assisted_review = "## Review\n\n- Focused finding";

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(
                Some("Reviewing changes with gpt-5.4"),
                Some(assisted_review),
                None,
            ),
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Merged session work."));
        assert!(!text.contains("Focused finding"));
        assert!(!text.contains("Reviewing changes with gpt-5.4"));
    }

    /// Verifies a terminal `Canceled` session keeps its interrupted transcript
    /// without appending transient focused-review output.
    #[test]
    fn test_output_lines_canceled_session_hides_review_text_when_available() {
        // Arrange
        let mut session = session_fixture();
        session.output = "interrupted transcript".to_string();
        session.status = Status::Canceled;
        let assisted_review = "## Review\n\n- Focused finding";

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(
                Some("Reviewing changes with gpt-5.4"),
                Some(assisted_review),
                None,
            ),
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("interrupted transcript"));
        assert!(!text.contains("Focused finding"));
        assert!(!text.contains("Reviewing changes with gpt-5.4"));
    }

    /// Verifies focused-review failures remain visible after the transient
    /// loading status returns to `Review`.
    #[test]
    fn test_output_lines_review_session_shows_review_status_message_when_text_missing() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        let review_status_message = "Review assist unavailable: empty provider response";

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(Some(review_status_message), None, None),
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains(review_status_message));
    }

    /// Verifies focused-review fallback text is rendered literally instead of
    /// being interpreted as markdown.
    #[test]
    fn test_output_lines_review_status_message_preserves_markdown_characters() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::Review;
        let review_status_message = "# Review *failed* for `tool`";

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(Some(review_status_message), None, None),
            None,
        );
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
        session.output = "implemented the feature".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
            None,
        );
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
        session.output =
            " › hi\n\n[Commit] No changes to commit.\n\n › add hello world\n\n".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › add hello world\n\n"),
                ..line_context(None, None, None)
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
        session.output =
            " › hi\n\n[Commit] No changes to commit.\n\n › add hello world\n\nworking".to_string();
        session.queued_messages = vec!["follow up\nwith context".to_string()];
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › add hello world\n\n"),
                ..line_context(None, None, None)
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

    /// Verifies an active prompt at the start of the transcript hides stale
    /// summary content.
    #[test]
    fn test_output_lines_in_progress_single_prompt_hides_summary() {
        // Arrange
        let mut session = session_fixture();
        session.output = " › add hello world\n\nI added the README change.\n".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some(" › add hello world\n\n"),
                ..line_context(None, None, None)
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
        session.output = " › hi\n\nHello!\n\n[Commit] No changes to commit.\n\n › review \
                          project\n\n"
            .to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
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
        session.output = " › hi\n\nprevious answer\n\n › actual prompt\n\nstreaming answer\n › \
                          quoted output\n"
            .to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › actual prompt\n\n"),
                ..line_context(None, None, None)
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
        session.output = "implemented the feature".to_string();
        session.status = Status::Review;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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

    /// Verifies the done-summary transition renders the rewritten summary
    /// payload while preserving the completed transcript.
    #[test]
    fn test_output_lines_done_summary_transition_preserves_transcript() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        let review_lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
            None,
        );
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

        // Act
        let done_output_text = SessionOutput::output_text(&session);
        let done_lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(None, None, None),
            None,
        );
        let done_text = done_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(review_text.contains("Change Summary"));
        assert!(review_text.contains("Added the structured protocol summary."));
        assert_eq!(done_output_text, "streamed output");
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

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(
                Some("Reviewing changes with gpt-5.4"),
                Some(assisted_text),
                None,
            ),
            None,
        );
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
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Canceled;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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
        session.output = "some output".to_string();
        session.status = Status::InProgress;

        // Act
        let lines = output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(None, None, None),
            None,
        );
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

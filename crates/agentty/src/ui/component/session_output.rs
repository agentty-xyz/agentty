use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::hash::Hasher;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use rustc_hash::FxHasher;

use crate::domain::session::{Session, SessionId, Status};
use crate::ui::markdown::{self, render_markdown};
use crate::ui::state::app_mode::DoneSessionOutputMode;
use crate::ui::util::{bottom_pinned_scroll_offset, panel_inner_width};
use crate::ui::{Component, layout, text_util};

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
/// transcript on every frame. Width, selected done-session mode, active prompt,
/// review text/status, progress text, and markdown style version cover the
/// transient inputs that can alter rendered lines without changing the stored
/// session row.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionOutputLayoutCacheKey {
    active_progress: TextFingerprint,
    active_prompt_output: TextFingerprint,
    done_session_output_mode: DoneSessionOutputMode,
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
    /// Number of rendered lines, saturated for scroll metric arithmetic.
    pub(crate) line_count: u16,
    /// Rendered lines shared between scroll metrics and frame painting.
    pub(crate) lines: Arc<[Line<'static>]>,
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
/// width, output mode, active prompt, review text/status, and progress text.
/// Entries are invalidated by key changes; the markdown render-cache version is
/// part of the key so style-bearing lines are not reused after markdown cache
/// invalidation.
pub struct SessionOutputLayoutCache {
    entries: RefCell<VecDeque<SessionOutputLayoutCacheEntry>>,
}

impl Default for SessionOutputLayoutCache {
    fn default() -> Self {
        Self {
            entries: RefCell::new(VecDeque::with_capacity(
                SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT,
            )),
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

    /// Stores one freshly rendered entry and evicts old entries over the
    /// bounded capacity.
    fn store_entry(&self, entry: SessionOutputLayoutCacheEntry) {
        let mut entries = self.entries.borrow_mut();
        entries.push_front(entry);

        while entries.len() > SESSION_OUTPUT_LAYOUT_CACHE_ENTRY_LIMIT {
            entries.pop_back();
        }
    }
}

/// Session chat output panel renderer.
pub struct SessionOutput<'a> {
    active_prompt_output: Option<&'a str>,
    active_progress: Option<&'a str>,
    /// Selected panel content for the session output panel.
    done_session_output_mode: DoneSessionOutputMode,
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
    /// Completed-session panel mode currently selected by the user.
    pub(crate) done_session_output_mode: DoneSessionOutputMode,
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
            done_session_output_mode: DoneSessionOutputMode::Summary,
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

    /// Sets the output display mode for completed sessions.
    #[must_use]
    pub fn done_session_output_mode(mut self, mode: DoneSessionOutputMode) -> Self {
        self.done_session_output_mode = mode;
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
        let lines = Arc::<[Line<'static>]>::from(Self::output_lines(
            session,
            output_area,
            context,
            markdown_render_cache,
        ));
        let line_count = u16::try_from(lines.len()).unwrap_or(u16::MAX);

        SessionOutputLayout { line_count, lines }
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
            done_session_output_mode: context.done_session_output_mode,
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

    /// Builds rendered markdown lines and contextual status/help rows for the
    /// current session state.
    ///
    /// `Status::Done` includes an inline `t` toggle hint that switches between
    /// summary and full output views. Active statuses append only the generic
    /// loader row so transcript text stays stable until the turn completes.
    /// Wrapping width follows the configured output panel borders so line
    /// metrics stay in sync with rendered content. Transcript-derived content
    /// always renders completed content before the currently active prompt
    /// block. When a previous-turn summary is available during an active turn,
    /// that summary stays attached to completed content above the latest user
    /// prompt until the active turn finishes and persists its replacement.
    /// Focused-review output is appended after transcript content so it reads
    /// like agent-produced session output instead of replacing the transcript
    /// panel, but it is suppressed once the session reaches `Done` because
    /// merged sessions should only show final summary or transcript content.
    fn output_lines(
        session: &Session,
        output_area: Rect,
        context: SessionOutputLineContext<'_>,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) -> Vec<Line<'static>> {
        let SessionOutputLineContext {
            active_prompt_output,
            active_progress,
            done_session_output_mode,
            review_status_message,
            review_text,
            session_update_version: _,
        } = context;
        let status = session.status;
        let output_text = Self::output_text(session, done_session_output_mode);
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
        Self::append_transcript_footer_lines(
            &mut lines,
            trailing_footer_text,
            inner_width,
            markdown_render_cache,
        );
        let shows_summary_block =
            Self::shows_summary_block(session.status, done_session_output_mode);
        if shows_summary_block && active_turn_text.is_some() {
            Self::append_summary_lines(
                &mut lines,
                session.summary.as_deref(),
                inner_width,
                markdown_render_cache,
            );
        }
        Self::append_active_turn_lines(
            &mut lines,
            active_turn_text.as_deref(),
            inner_width,
            markdown_render_cache,
        );
        if shows_summary_block && active_turn_text.is_none() {
            Self::append_summary_lines(
                &mut lines,
                session.summary.as_deref(),
                inner_width,
                markdown_render_cache,
            );
        }
        if Self::shows_review_lines(session.status) {
            Self::append_review_lines(&mut lines, review_text, inner_width, markdown_render_cache);
        }
        Self::append_published_branch_sync_lines(&mut lines, session);

        if let Some(status_line) =
            layout::session_output_status_line(status, active_progress, review_status_message)
        {
            while lines.last().is_some_and(|line| line.width() == 0) {
                lines.pop();
            }

            lines.push(Line::from(""));
            lines.push(status_line);
        } else if status == Status::Done {
            lines.push(Line::from(""));
            lines.push(layout::session_output_done_toggle_line(
                done_session_output_mode,
            ));
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(""));
        }

        lines
    }

    /// Appends one automatic published-branch sync status row when the latest
    /// completed turn started, finished, or failed an auto-push.
    fn append_published_branch_sync_lines(lines: &mut Vec<Line<'static>>, session: &Session) {
        let Some(sync_line) = layout::session_output_published_branch_sync_line(session) else {
            return;
        };

        while lines.last().is_some_and(|line| line.width() == 0) {
            lines.pop();
        }

        lines.push(Line::from(""));
        lines.push(sync_line);
    }

    /// Splits the transcript before the current active-turn prompt block.
    ///
    /// This keeps the previous-turn summary grouped with completed transcript
    /// output even when the active prompt has already been appended to the
    /// persisted transcript.
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
    /// status and done-session display mode.
    ///
    /// Summary markdown is shown only for `Done` sessions after it has been
    /// persisted. New draft sessions render staged-draft guidance before
    /// their first live turn starts. Active and canceled sessions otherwise
    /// render transcript output only.
    fn output_text(
        session: &Session,
        done_session_output_mode: DoneSessionOutputMode,
    ) -> Cow<'_, str> {
        match session.status {
            Status::New if session.is_draft_session() => {
                return Cow::Owned(Self::render_draft_session_preview(session));
            }
            Status::Done if done_session_output_mode == DoneSessionOutputMode::Summary => {
                return Cow::Owned(layout::session_output_summary_markdown(
                    Self::session_summary_text(session),
                ));
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

    /// Returns the persisted raw summary payload or plain-text fallback.
    fn session_summary_text(session: &Session) -> &str {
        session
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .unwrap_or("")
    }

    /// Returns whether the output panel should append the structured summary
    /// block outside the persisted transcript string.
    ///
    /// Canceled sessions keep the raw transcript visible so interrupted turns
    /// do not render synthetic summary content that was never finalized.
    fn shows_summary_block(
        status: Status,
        done_session_output_mode: DoneSessionOutputMode,
    ) -> bool {
        if status == Status::Canceled {
            return false;
        }

        !(status == Status::Done && done_session_output_mode == DoneSessionOutputMode::Summary)
    }

    /// Returns whether focused-review output belongs in the current session
    /// view.
    ///
    /// Merged sessions enter `Status::Done`, and at that point any cached
    /// review text is stale next to the final summary or transcript view.
    fn shows_review_lines(status: Status) -> bool {
        status != Status::Done
    }

    /// Appends focused-review output to the transcript when review text is
    /// available for the current session view.
    ///
    /// The `### Suggestions` header is annotated with a `(type "/apply" to
    /// apply)` hint at render time so users discover the apply shortcut
    /// without polluting the persisted review markdown consumed by
    /// suggestion extraction.
    fn append_review_lines(
        lines: &mut Vec<Line<'static>>,
        review_text: Option<&str>,
        inner_width: usize,
        markdown_render_cache: Option<&markdown::MarkdownRenderCache>,
    ) {
        let review_markdown = review_text
            .map(str::trim)
            .filter(|review_text| !review_text.is_empty())
            .map(layout::annotate_review_suggestions_header);

        let Some(review_markdown) = review_markdown else {
            return;
        };

        Self::append_markdown_lines(lines, &review_markdown, inner_width, markdown_render_cache);
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

    /// Appends one trailing transcript footer immediately after completed-turn
    /// transcript content when a known footer block is present.
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
}

impl Component for SessionOutput<'_> {
    /// Renders bordered output content for the active session.
    ///
    /// Session status/title headers are rendered by the page layer so this
    /// component keeps the output border title-free.
    fn render(&self, f: &mut Frame, output_area: Rect) {
        let status = self.session.status;
        let layout = Self::rendered_layout(
            self.session,
            output_area,
            SessionOutputLineContext {
                active_prompt_output: self.active_prompt_output,
                active_progress: self.active_progress,
                done_session_output_mode: self.done_session_output_mode,
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

        let paint_lines = SessionOutput::borrowed_paint_lines(&layout.lines);
        let paragraph = Paragraph::new(paint_lines)
            .block(
                Block::default()
                    .borders(layout::session_output_panel_borders())
                    .border_style(layout::session_output_panel_border_style(status)),
            )
            .scroll((final_scroll, 0));

        f.render_widget(paragraph, output_area);
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
        done_session_output_mode: DoneSessionOutputMode,
        review_status_message: Option<&'a str>,
        review_text: Option<&'a str>,
        active_progress: Option<&'a str>,
    ) -> SessionOutputLineContext<'a> {
        SessionOutputLineContext {
            active_prompt_output: None,
            active_progress,
            done_session_output_mode,
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
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
            ..line_context(DoneSessionOutputMode::Summary, None, None, None)
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
    fn test_output_layout_cache_keys_review_text() {
        // Arrange
        let session = session_fixture();
        let markdown_render_cache = markdown::MarkdownRenderCache::default();
        let output_layout_cache = SessionOutputLayoutCache::default();
        let base_context = SessionOutputLineContext {
            session_update_version: 7,
            ..line_context(DoneSessionOutputMode::Summary, None, None, None)
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
    fn test_output_text_borrows_transcript_for_output_mode() {
        // Arrange
        let mut session = session_fixture();
        session.output = "borrowed transcript".to_string();

        // Act
        let output_text = SessionOutput::output_text(&session, DoneSessionOutputMode::Output);

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
    fn test_output_lines_uses_summary_for_done_session() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        assert!(!text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_render_staged_draft_preview_for_new_session() {
        // Arrange
        let mut session = session_fixture();
        session.is_draft = true;
        session.prompt = "First draft\n\nSecond draft".to_string();

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Done;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Output, None, None, None),
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("streamed output"));
        assert!(text.contains("Added the structured protocol summary."));
        assert!(text.contains("Session output now renders persisted summary markdown."));
    }

    #[test]
    fn test_output_lines_review_session_appends_structured_summary() {
        // Arrange
        let mut session = session_fixture();
        session.output = "implemented the feature".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Review;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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

    /// Verifies previous-turn summaries stay above the currently active user
    /// prompt while a reply is running.
    #[test]
    fn test_output_lines_in_progress_session_shows_summary_above_active_prompt() {
        // Arrange
        let mut session = session_fixture();
        session.output =
            " › hi\n\n[Commit] No changes to commit.\n\n › add hello world\n\n".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › add hello world\n\n"),
                ..line_context(DoneSessionOutputMode::Summary, None, None, None)
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
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let prompt_index = text
            .find(" › add hello world")
            .expect("active prompt should be rendered");

        // Assert
        assert!(commit_index < summary_index);
        assert!(summary_index < prompt_index);
    }

    /// Verifies an active prompt at the start of the transcript still renders
    /// below any previous summary.
    #[test]
    fn test_output_lines_in_progress_single_prompt_shows_summary_above_prompt() {
        // Arrange
        let mut session = session_fixture();
        session.output = " › add hello world\n\nI added the README change.\n".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some(" › add hello world\n\n"),
                ..line_context(DoneSessionOutputMode::Summary, None, None, None)
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
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");

        // Assert
        assert!(text.contains("I added the README change."));
        assert!(summary_index < prompt_index);
    }

    /// Verifies the latest user prompt is detected when the exact active
    /// prompt capture is unavailable.
    #[test]
    fn test_output_lines_in_progress_without_active_capture_shows_summary_above_last_prompt() {
        // Arrange
        let mut session = session_fixture();
        session.output = " › hi\n\nHello!\n\n[Commit] No changes to commit.\n\n › review \
                          project\n\n"
            .to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::InProgress;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let prompt_index = text
            .find(" › review project")
            .expect("latest prompt should be rendered");

        // Assert
        assert!(commit_index < summary_index);
        assert!(summary_index < prompt_index);
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
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            SessionOutputLineContext {
                active_prompt_output: Some("\n › actual prompt\n\n"),
                ..line_context(DoneSessionOutputMode::Summary, None, None, None)
            },
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let summary_index = text
            .find("Change Summary")
            .expect("structured summary should be rendered");
        let prompt_index = text
            .find(" › actual prompt")
            .expect("active prompt should be rendered");

        // Assert
        assert!(text.contains(" › quoted output"));
        assert!(summary_index < prompt_index);
    }

    #[test]
    fn test_output_lines_review_session_without_summary_keeps_transcript_only() {
        // Arrange
        let mut session = session_fixture();
        session.output = "implemented the feature".to_string();
        session.status = Status::Review;

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
    /// payload exactly once in summary mode.
    #[test]
    fn test_output_lines_done_summary_transition_renders_rewritten_summary() {
        // Arrange
        let mut session = session_fixture();
        session.output = "streamed output".to_string();
        session.summary = Some(summary_fixture());
        session.status = Status::Review;
        let review_lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        let done_output_text = SessionOutput::output_text(&session, DoneSessionOutputMode::Summary);
        let done_lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 8),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        assert_eq!(
            done_output_text,
            "# Summary\n\nSession now greets users on startup.\n\n# Commit\n\nRefine session \
             summary"
        );
        assert!(done_text.contains("Summary"));
        assert!(done_text.contains("Session now greets users on startup."));
        assert!(done_text.contains("Commit"));
        assert!(done_text.contains("Refine session summary"));
        assert!(!done_text.contains("Change Summary"));
        assert!(!done_text.contains("streamed output"));
    }

    #[test]
    fn test_output_lines_agent_review_mode_shows_assisted_text() {
        // Arrange
        let mut session = session_fixture();
        session.status = Status::AgentReview;
        let assisted_text = "## Review\n\n- Focused finding";

        // Act
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(
                DoneSessionOutputMode::Review,
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
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
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
        let lines = SessionOutput::output_lines(
            &session,
            Rect::new(0, 0, 80, 5),
            line_context(DoneSessionOutputMode::Summary, None, None, None),
            None,
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Assert
        assert!(text.contains("Working..."));
    }
}

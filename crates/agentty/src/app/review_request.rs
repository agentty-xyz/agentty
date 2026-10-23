//! Shared review-request helpers used by app workflows.

use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use ag_git::GitClient;
use ag_worker::RunClient;
use pulldown_cmark::{Event, Parser, Tag};

use crate::app::session::{SessionError, SessionTaskService};
use crate::domain::agent::AgentSelection;
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

/// Canonical review attribution, managed after generation rather than by a
/// model.
const AGENTTY_FOOTER: &str = "Co-Authored-By: [Agentty](https://github.com/agentty-xyz/agentty)";

/// Injected boundaries shared by creation, manual refresh, and automatic
/// updates.
pub(crate) struct ReviewRequestGenerator<'a> {
    pub(crate) db: &'a AppRepositories,
    pub(crate) folder: &'a Path,
    pub(crate) git_client: &'a Arc<dyn GitClient>,
    pub(crate) run_client: &'a dyn RunClient,
    pub(crate) session_agent: AgentSelection,
    pub(crate) session_id: &'a str,
    pub(crate) transcript: &'a Mutex<SessionTranscript>,
}

impl ReviewRequestGenerator<'_> {
    /// Generates PR/MR-specific prose from cumulative evidence through the
    /// worker comparing immutable commits acquired from the published source
    /// and effective review target independently of local tracking refs.
    /// Existing remote content is preserved conservatively and attribution
    /// is normalized according to the current project setting.
    pub(crate) async fn generate(
        &self,
        current: Option<&forge::ReviewRequestMetadata>,
        commit: &ReviewRequestCommitMessage,
        target_branch: &str,
    ) -> Result<forge::ReviewRequestMetadata, SessionError> {
        let published_ref = self
            .db
            .sessions()
            .load_session_published_upstream_ref(self.session_id)
            .await?
            .ok_or_else(|| {
                SessionError::Workflow("Published branch reference is unavailable".to_string())
            })?;
        let (remote, source_branch) = published_ref.split_once('/').ok_or_else(|| {
            SessionError::Workflow("Published branch reference has no remote".to_string())
        })?;
        let (base_commit, source_commit) = self
            .git_client
            .fetch_remote_branch_commits(
                self.folder.to_path_buf(),
                remote.to_string(),
                target_branch.to_string(),
                source_branch.to_string(),
            )
            .await?;
        let diff = self
            .git_client
            .committed_diff(self.folder.to_path_buf(), base_commit, source_commit)
            .await?;
        let evidence = SessionTaskService::review_request_evidence(
            &diff,
            commit.body.as_deref().unwrap_or_default(),
            self.transcript,
        );
        let initial = forge::ReviewRequestMetadata {
            body: String::new(),
            title: commit.title.clone(),
        };
        let mut metadata = SessionTaskService::review_request_metadata(
            current.unwrap_or(&initial),
            self.folder,
            &evidence,
            &commit.title,
            self.run_client,
            self.session_agent,
        )
        .await?;
        let include_attribution = SessionTaskService::load_include_coauthored_by_agentty_setting(
            self.db,
            self.session_id,
        )
        .await;
        metadata.body = normalize_attribution(&metadata.body, include_attribution)?;

        Ok(metadata)
    }
}

/// Removes recognized standalone Agentty footer lines outside Markdown code
/// spans and blocks, HTML code elements, and HTML comments. Literal examples,
/// other attribution, and text below footers remain intact.
pub(crate) fn strip_attribution(description: &str) -> String {
    markdown_lines(description)
        .filter_map(|(line, context)| {
            if context.is_literal() {
                return Some(line);
            }
            let Some((key, value)) = line.trim().split_once(':') else {
                return Some(line);
            };

            (!(key.eq_ignore_ascii_case("Co-Authored-By")
                && (value.trim().eq_ignore_ascii_case("Agentty")
                    || value.trim() == "[Agentty](https://github.com/agentty-xyz/agentty)")))
                .then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Code and comment spans, projected relative to each preserved line when
/// comparing rendering context. Intact blocks can move without changing it.
#[derive(Default, Eq, Hash, PartialEq)]
pub(crate) struct MarkdownRegions {
    code: Vec<Range<usize>>,
    comments: Vec<Range<usize>>,
    inline_code: Vec<Range<usize>>,
}

impl MarkdownRegions {
    fn is_literal(&self) -> bool {
        !self.code.is_empty() || !self.comments.is_empty() || !self.inline_code.is_empty()
    }

    /// Parses Markdown and scans its raw HTML regions for comments and literal
    /// `pre`/`code` elements. Quoted attributes are consumed as part of tags.
    fn parse(description: &str) -> Self {
        let mut regions = Self::default();
        let mut literal: Option<(&str, usize)> = None;
        let mut consumed = 0;
        for (event, range) in Parser::new(description).into_offset_iter() {
            if range.end <= consumed {
                continue;
            }
            match event {
                Event::Start(Tag::CodeBlock(_)) => {
                    regions.code.push(range);
                    continue;
                }
                Event::Code(_) => {
                    regions.inline_code.push(range);
                    continue;
                }
                Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {}
                _ => continue,
            }
            let mut offset = range.start.max(consumed);
            while offset < range.end {
                let Some(next) = description[offset..range.end].find('<') else {
                    break;
                };
                offset += next;
                let source = &description[offset..];
                if literal.is_none() && source.starts_with("<!--") {
                    let end = source[4..]
                        .find("-->")
                        .map_or(description.len(), |closing| offset + 4 + closing + 3);
                    regions.comments.push(offset..end);
                    offset = end;
                } else if let Some((name, closing, length)) = html_tag(source) {
                    let end = offset + length;
                    if let Some((tag, start)) = literal {
                        if closing && name.eq_ignore_ascii_case(tag) {
                            regions.code.push(start..end);
                            literal = None;
                        }
                    } else if !closing
                        && (name.eq_ignore_ascii_case("pre") || name.eq_ignore_ascii_case("code"))
                    {
                        literal = Some((name, offset));
                    }
                    offset = end;
                } else {
                    offset += 1;
                }
                consumed = offset;
            }
        }
        if let Some((_, start)) = literal {
            regions.code.push(start..description.len());
        }
        regions.code.sort_by_key(|range| range.start);

        regions
    }

    fn for_line(&self, offset: usize, length: usize) -> Self {
        let relative = |ranges: &[Range<usize>]| {
            ranges
                .iter()
                .filter_map(|range| {
                    let start = range.start.max(offset);
                    let end = range.end.min(offset + length);

                    (start < end).then(|| start - offset..end - offset)
                })
                .collect()
        };

        Self {
            code: relative(&self.code),
            comments: relative(&self.comments),
            inline_code: relative(&self.inline_code),
        }
    }
}

/// Pairs each original line with its code-block and HTML-comment context.
/// Source offsets include line endings, including CRLF. Partial-line comment
/// spans distinguish visible prose from hidden text on the same line.
pub(crate) fn markdown_lines(description: &str) -> impl Iterator<Item = (&str, MarkdownRegions)> {
    let regions = MarkdownRegions::parse(description);
    description
        .split_inclusive('\n')
        .scan(0, move |offset, raw| {
            let end = *offset + raw.len();
            let line = raw
                .strip_suffix("\r\n")
                .or_else(|| raw.strip_suffix('\n'))
                .unwrap_or(raw);
            let context = regions.for_line(*offset, line.len());
            *offset = end;

            Some((line, context))
        })
}

/// Returns a tag's name, closing flag, and byte length. Angle brackets and
/// comment delimiters in quoted attributes cannot terminate a tag.
fn html_tag(source: &str) -> Option<(&str, bool, usize)> {
    let content = source.strip_prefix('<')?;
    let closing = content.starts_with('/');
    let content = content.strip_prefix('/').unwrap_or(content);
    let length = content
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':'))
        .count();
    if !content
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
    {
        return None;
    }
    let name = &content[..length];
    if !content[length..].starts_with(|character: char| {
        character.is_ascii_whitespace() || matches!(character, '/' | '>')
    }) {
        return None;
    }
    let prefix = 1 + usize::from(closing) + length;
    let mut quote = None;
    for (offset, byte) in source.as_bytes()[prefix..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(current), byte) if current == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some((name, closing, prefix + offset + 1)),
            _ => {}
        }
    }

    None
}

/// Block spans exclude inline backticks. Source spans retain fences, internal
/// blank lines, and indentation so blocks can be compared intact rather than
/// as rendered text.
pub(crate) fn code_block_ranges(description: &str) -> Vec<Range<usize>> {
    MarkdownRegions::parse(description).code
}

/// Retains complete inline-code source spans, including backtick delimiters,
/// embedded newlines, and whitespace that rendered code text normalizes.
pub(crate) fn inline_code_spans(description: &str) -> Vec<&str> {
    MarkdownRegions::parse(description)
        .inline_code
        .into_iter()
        .map(|range| &description[range])
        .collect()
}

/// Emits one canonical Agentty footer as the final nonblank line when enabled.
/// Rejects descriptions that would swallow attribution in an unclosed code
/// block or comment, including when attribution is disabled.
pub(crate) fn normalize_attribution(
    description: &str,
    enabled: bool,
) -> Result<String, SessionError> {
    let stripped = strip_attribution(description);
    let body = stripped.trim_end_matches('\n');
    let normalized = if body.trim().is_empty() {
        AGENTTY_FOOTER.to_string()
    } else {
        format!("{body}\n\n{AGENTTY_FOOTER}")
    };
    if markdown_lines(&normalized)
        .last()
        .is_some_and(|(_, context)| context.is_literal())
    {
        return Err(SessionError::Workflow(
            "Review-request description leaves attribution inside a Markdown code block or HTML \
             comment; close the block and retry"
                .to_string(),
        ));
    }

    Ok(if enabled {
        normalized
    } else {
        body.to_string()
    })
}

/// Parsed commit continuity used as evidence for review-request generation.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestCommitMessage {
    /// Optional commit description supplied as supporting evidence.
    pub(crate) body: Option<String>,
    /// Title copied from the first non-empty commit-message line.
    pub(crate) title: String,
}

/// Parses a session-branch commit message into review-request continuity.
pub(crate) fn parse_review_request_commit_message(
    commit_message: &str,
) -> Option<ReviewRequestCommitMessage> {
    let mut lines = commit_message.lines();
    let title = lines
        .find(|line| !line.trim().is_empty())?
        .trim()
        .to_string();
    let description = lines.collect::<Vec<_>>().join("\n");
    let description = description.trim();

    Some(ReviewRequestCommitMessage {
        body: (!description.is_empty()).then(|| description.to_string()),
        title,
    })
}

#[cfg(test)]
#[path = "review_request_test.rs"]
mod tests;

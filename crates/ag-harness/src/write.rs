//! Built-in `write` tool that applies unified-diff patches to repository
//! files.
//!
//! Besides a standard unified diff, the tool accepts the forms models
//! trained on other harnesses send: a diff without file headers, a diff
//! inside Codex's `*** Begin Patch`/`*** End Patch` envelope, and Codex's
//! own `apply_patch` dialect (see [`parse_codex_patch`]).

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;

use crate::effect::Effects;
use crate::file_system::FileSystem;
use crate::reservation::WriteJournal;
use crate::tool::WriteArguments;
use crate::{schema_contract, telemetry};

const BYTE_ORDER_MARK: &[u8] = b"\xef\xbb\xbf";
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Line comparisons one patch may spend relocating hunks whose header
/// position does not match, so repetitive targets cannot stall the turn.
const MAX_LOCATE_COMPARISONS: usize = 8 * 1024 * 1024;
/// Candidate line numbers named when a hunk matches several places.
const MAX_REPORTED_CANDIDATES: usize = 8;
/// Lines of the target shown before and after the place a hunk that
/// matches nowhere most nearly matches.
const SHOWN_CONTEXT_LINES: usize = 3;
/// Most lines of the target shown with a hunk that matches nowhere.
const MAX_SHOWN_LINES: usize = 40;
/// Most bytes of the target shown with a hunk that matches nowhere.
const MAX_SHOWN_BYTES: usize = 4 * 1024;
/// First line of a Codex patch, with its line ending.
const CODEX_BEGIN: &str = "*** Begin Patch\n";
/// Last line of a Codex patch.
const CODEX_END: &str = "*** End Patch";
const CODEX_ADD_FILE: &str = "*** Add File: ";
const CODEX_DELETE_FILE: &str = "*** Delete File: ";
const CODEX_END_OF_FILE: &str = "*** End of File";
const CODEX_MOVE_TO: &str = "*** Move to: ";
const CODEX_UPDATE_FILE: &str = "*** Update File: ";

/// Successful result of applying one repository write.
///
/// The model-facing result lists `adjusted_hunks` only when a hunk applied
/// away from its header line or only after ignoring trailing whitespace, so
/// the model can correct its line numbers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteOutput {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    adjusted_hunks: Vec<HunkAdjustment>,
    bytes_written: usize,
    /// Reported in the activity only; the model's result stays unchanged.
    #[serde(skip)]
    line_changes: LineChanges,
    path: String,
    status: &'static str,
}

impl WriteOutput {
    /// Returns the number of bytes in the resulting file.
    pub fn bytes_written(&self) -> usize {
        self.bytes_written
    }

    /// Returns the repository-relative path that was written.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the lines the write added, as `ToolActivity::Write` reports.
    pub(crate) fn lines_added(&self) -> u64 {
        self.line_changes.added
    }

    /// Returns the lines the write removed, as `ToolActivity::Write`
    /// reports.
    pub(crate) fn lines_removed(&self) -> u64 {
        self.line_changes.removed
    }

    pub(crate) fn to_tool_result(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    fn new(
        path: String,
        bytes_written: usize,
        line_changes: LineChanges,
        adjusted_hunks: Vec<HunkAdjustment>,
    ) -> Self {
        Self {
            adjusted_hunks,
            bytes_written,
            line_changes,
            path,
            status: "applied",
        }
    }
}

/// Failure returned while validating or applying one repository write.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WriteError {
    /// The existing file is not valid UTF-8 text.
    #[error("write target `{path}` is not UTF-8 text")]
    BinaryTarget {
        /// Repository-relative target path.
        path: String,
    },
    /// A hunk's context and removed lines match nowhere in the target.
    /// `content` holds the target's current lines where they most nearly
    /// match, or around the hunk's position, and the model-facing result
    /// shows them so the model can correct the hunk without reading the
    /// file again.
    #[error("invalid patch: {reason}")]
    ContextMismatch {
        /// Current target lines, each with its line ending.
        content: String,
        /// Bounded validation diagnostic.
        reason: String,
        /// One-based line number of the first line of `content`.
        start_line: usize,
    },
    /// The bounded tool result could not be encoded for the model.
    #[error("failed to encode write result: {0}")]
    Encode(#[from] serde_json::Error),
    /// The patch made no change to the target.
    #[error("patch does not change `{path}`")]
    NoChange {
        /// Repository-relative target path.
        path: String,
    },
    /// The patch could not be parsed or applied.
    #[error("invalid patch: {reason}")]
    Patch {
        /// Bounded validation diagnostic.
        reason: String,
    },
    /// The patch headers do not name the requested path.
    #[error("patch headers do not match write path `{path}`")]
    PathMismatch {
        /// Repository-relative target path.
        path: String,
    },
    /// Reading the current target failed.
    #[error("failed to read write target `{path}`: {source}")]
    ReadTarget {
        /// Repository-relative target path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The trusted repository root could not be resolved.
    #[error("failed to resolve repository root `{path}`: {source}")]
    RepositoryRoot {
        /// Repository root supplied by the host.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The existing target exceeds the write safety limit.
    #[error("write target `{path}` exceeds the {limit}-byte limit")]
    TargetTooLarge {
        /// Configured byte limit.
        limit: usize,
        /// Repository-relative target path.
        path: String,
    },
    /// The requested diff operation is outside the one-file create/update
    /// subset.
    #[error("unsupported patch operation: {reason}")]
    Unsupported {
        /// Bounded explanation of the unsupported operation.
        reason: String,
    },
    /// Replacing the target or recording its durable write intent/outcome
    /// failed.
    #[error("failed to write target `{path}`: {source}")]
    WriteTarget {
        /// Repository-relative target path.
        path: String,
        /// Underlying filesystem or journal persistence failure.
        #[source]
        source: io::Error,
    },
}

impl WriteError {
    pub(crate) fn is_model_correctable(&self) -> bool {
        match self {
            Self::BinaryTarget { .. }
            | Self::ContextMismatch { .. }
            | Self::NoChange { .. }
            | Self::Patch { .. }
            | Self::PathMismatch { .. }
            | Self::TargetTooLarge { .. }
            | Self::Unsupported { .. } => true,
            Self::WriteTarget { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::InvalidData
            ),
            Self::Encode(_) | Self::ReadTarget { .. } | Self::RepositoryRoot { .. } => false,
        }
    }

    pub(crate) fn to_tool_result(&self, path: &str) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct CurrentLines<'a> {
            content: &'a str,
            end_line: usize,
            start_line: usize,
        }

        #[derive(Serialize)]
        struct RejectedWrite<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            current: Option<CurrentLines<'a>>,
            error: String,
            path: &'a str,
            status: &'static str,
        }

        let error = schema_contract::bounded_diagnostic(self);
        let (current, error) = match self {
            Self::ContextMismatch {
                content,
                start_line,
                ..
            } => {
                let end_line = start_line + content.split_inclusive('\n').count() - 1;

                (
                    Some(CurrentLines {
                        content,
                        end_line,
                        start_line: *start_line,
                    }),
                    format!(
                        "{error}; `current` holds lines {start_line}-{end_line} of the file as it \
                         is now; copy context and removed lines exactly from it"
                    ),
                )
            }
            _ => (None, error),
        };

        serde_json::to_string(&RejectedWrite {
            current,
            error,
            path,
            status: "rejected",
        })
    }
}

pub(crate) struct WriteTool {
    pub(crate) effects: Effects,
    pub(crate) journal: Option<WriteJournal>,
    file_system: Arc<dyn FileSystem>,
    repository_root: PathBuf,
}

impl WriteTool {
    pub(crate) fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self {
            effects: Effects::default(),
            journal: None,
            file_system,
            repository_root,
        }
    }

    pub(crate) async fn execute(
        &self,
        arguments: &WriteArguments,
        call_id: &str,
    ) -> Result<WriteOutput, WriteError> {
        let root = self
            .file_system
            .canonicalize(&self.repository_root)
            .await
            .map_err(|source| WriteError::RepositoryRoot {
                path: self.repository_root.clone(),
                source,
            })?;
        let current = self.read_current(&root, arguments.path()).await?;
        let PatchedText {
            adjusted_hunks,
            content: result,
            line_changes,
        } = apply_unified_diff(arguments.path(), current.as_deref(), arguments.patch())?;
        if result.len() > MAX_FILE_BYTES {
            return Err(WriteError::TargetTooLarge {
                limit: MAX_FILE_BYTES,
                path: arguments.path().to_string(),
            });
        }
        if current.as_deref() == Some(result.as_slice()) {
            return Err(WriteError::NoChange {
                path: arguments.path().to_string(),
            });
        }
        let bytes_written = result.len();
        let intent = match &self.journal {
            Some(journal) => Some(
                journal
                    .intent(
                        call_id,
                        &root,
                        arguments.path(),
                        current.as_deref(),
                        &result,
                    )
                    .await
                    .map_err(|source| WriteError::WriteTarget {
                        path: arguments.path().to_string(),
                        source: io::Error::other(source),
                    })?,
            ),
            None => None,
        };
        let file_system = Arc::clone(&self.file_system);
        let journal = self.journal.clone();
        let path = arguments.path().to_string();
        let effects = self.effects.clone();
        let mut effect = effects.retain();
        effect.starting();
        let operation = async move {
            let replacement = file_system
                .replace_beneath(&root, Path::new(&path), current, result)
                .await;
            effect.acknowledged();
            if let (Some(journal), Some(intent)) = (journal, intent)
                && let Err(recording) = journal.finish(intent, replacement.is_ok()).await
            {
                let source = match replacement {
                    Ok(()) => io::Error::other(recording),
                    Err(error) => {
                        io::Error::other(format!("{error}; outcome recording failed: {recording}"))
                    }
                };
                effect.recorded();
                effects.recording_failed(&source);

                return Err(WriteError::WriteTarget { path, source });
            }
            effect.recorded();
            replacement.map_err(|source| WriteError::WriteTarget {
                path: path.clone(),
                source,
            })?;

            Ok(WriteOutput::new(
                path,
                bytes_written,
                line_changes,
                adjusted_hunks,
            ))
        };
        // Retain the tool context even after cancellation ends its lifecycle
        // span.
        let task = tokio::spawn(telemetry::with_current_context(operation));

        task.await.map_err(|source| WriteError::WriteTarget {
            path: arguments.path().to_string(),
            source: io::Error::other(source),
        })?
    }

    async fn read_current(&self, root: &Path, path: &str) -> Result<Option<Vec<u8>>, WriteError> {
        let file = match self.file_system.open_beneath(root, Path::new(path)).await {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(WriteError::ReadTarget {
                    path: path.to_string(),
                    source,
                });
            }
        };
        let mut content = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut content)
            .await
            .map_err(|source| WriteError::ReadTarget {
                path: path.to_string(),
                source,
            })?;
        if content.len() > MAX_FILE_BYTES {
            return Err(WriteError::TargetTooLarge {
                limit: MAX_FILE_BYTES,
                path: path.to_string(),
            });
        }

        Ok(Some(content))
    }
}

/// Returns the repository path the file headers of `patch` name as its
/// result, for a `write` call that left out its `path`: the `+++` header
/// without its `b/` prefix, or the path of a Codex patch's
/// `*** Update File:` or `*** Add File:` line. Only lines before the first
/// hunk count, and `/dev/null` names no path.
pub(crate) fn header_target(patch: &str) -> Option<&str> {
    patch
        .lines()
        .take_while(|line| !line.starts_with("@@"))
        .find_map(|line| {
            line.strip_prefix("+++ ")
                .map(|header| header_path(header.split('\t').next().unwrap_or_default()))
                .or_else(|| codex_target(line))
        })
        .map(str::trim_end)
        .filter(|path| !path.is_empty() && *path != "/dev/null")
}

/// Lines a write added and removed: the addition and removal lines of the
/// hunks it applied, as `git apply --numstat` counts them. Relocation and
/// whitespace-tolerant matching change where a hunk applies, not which
/// lines it adds or removes, and a created file is all additions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LineChanges {
    added: u64,
    removed: u64,
}

impl LineChanges {
    /// Counts the addition and removal lines of `hunks`.
    fn of(hunks: &[Hunk]) -> Self {
        let count = |kind| {
            hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == kind)
                .count() as u64
        };

        Self {
            added: count(PatchLineKind::Addition),
            removed: count(PatchLineKind::Removal),
        }
    }
}

/// Hunk that applied away from its header line or only after ignoring
/// trailing whitespace. Line numbers are one-based in the original file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct HunkAdjustment {
    applied_line: usize,
    header_line: usize,
    hunk: usize,
    #[serde(rename = "match")]
    line_match: LineMatch,
}

/// Comparison under which a hunk's original-side lines matched the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LineMatch {
    Exact,
    IgnoringTrailingWhitespace,
}

impl LineMatch {
    fn matches(self, source: &str, expected: &TextLine) -> bool {
        if source.ends_with('\n') != expected.newline {
            return false;
        }
        let source = source.strip_suffix('\n').unwrap_or(source);

        match self {
            Self::Exact => source == expected.content,
            Self::IgnoringTrailingWhitespace => source.trim_end() == expected.content.trim_end(),
        }
    }
}

/// Patched file bytes plus the hunks that needed relocation and the lines
/// the patch added and removed.
#[derive(Debug)]
struct PatchedText {
    adjusted_hunks: Vec<HunkAdjustment>,
    content: Vec<u8>,
    line_changes: LineChanges,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TextLine {
    content: String,
    newline: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatchLineKind {
    Addition,
    Context,
    Removal,
}

impl PatchLineKind {
    /// Splits a hunk line into its kind and content: a ` `, `+`, or `-`
    /// prefix, or none for an empty line, which reads as blank context.
    fn split(line: &str) -> Option<(Self, &str)> {
        if line.is_empty() {
            return Some((Self::Context, line));
        }
        let (prefix, content) = line.split_at_checked(1)?;
        let kind = match prefix {
            "+" => Self::Addition,
            " " => Self::Context,
            "-" => Self::Removal,
            _ => return None,
        };

        Some((kind, content))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PatchLine {
    kind: PatchLineKind,
    text: TextLine,
}

impl PatchLine {
    fn new(kind: PatchLineKind, content: &str, newline: bool) -> Self {
        Self {
            kind,
            text: TextLine {
                content: content.to_string(),
                newline,
            },
        }
    }
}

/// One parsed hunk. Line counts are derived from the body, as with
/// `git apply --recount`; the header's original count only distinguishes an
/// explicit `-N,0` insertion anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Hunk {
    lines: Vec<PatchLine>,
    new_start: usize,
    old_count: usize,
    old_start: usize,
}

impl Hunk {
    /// Returns the lines the target must contain where the hunk applies.
    fn original_lines(&self) -> Vec<&TextLine> {
        self.lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Addition)
            .map(|line| &line.text)
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnifiedDiff {
    hunks: Vec<Hunk>,
    modified_path: String,
    original_path: String,
}

#[derive(Default)]
struct PatchTermination {
    new_side: bool,
    old_side: bool,
}

impl PatchTermination {
    fn apply_marker(
        &mut self,
        hunk_lines: &mut [PatchLine],
        marker_repeated: bool,
    ) -> Result<(), WriteError> {
        if marker_repeated {
            return Err(patch_error("newline marker cannot be repeated"));
        }
        let previous = hunk_lines
            .last_mut()
            .ok_or_else(|| patch_error("newline marker has no preceding patch line"))?;
        previous.text.newline = false;
        match previous.kind {
            PatchLineKind::Addition => self.new_side = true,
            PatchLineKind::Context => {
                self.old_side = true;
                self.new_side = true;
            }
            PatchLineKind::Removal => self.old_side = true,
        }

        Ok(())
    }

    fn validate_line(&self, kind: PatchLineKind) -> Result<(), WriteError> {
        if (self.old_side && kind != PatchLineKind::Addition)
            || (self.new_side && kind != PatchLineKind::Removal)
        {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }

        Ok(())
    }
}

/// Applies hunks in order to an in-memory copy of the target, so a failing
/// hunk leaves the file untouched.
struct HunkApplier<'a> {
    adjusted_hunks: Vec<HunkAdjustment>,
    comparisons_left: usize,
    new_side_terminated: bool,
    output: Vec<Cow<'a, str>>,
    source_index: usize,
    source_lines: Vec<&'a str>,
}

impl<'a> HunkApplier<'a> {
    fn new(current: &'a str) -> Self {
        Self {
            adjusted_hunks: Vec::new(),
            comparisons_left: MAX_LOCATE_COMPARISONS,
            new_side_terminated: false,
            output: Vec::new(),
            source_index: 0,
            source_lines: current.split_inclusive('\n').collect(),
        }
    }

    fn apply_hunk(&mut self, number: usize, hunk: &Hunk) -> Result<(), WriteError> {
        let original = hunk.original_lines();
        let start = if original.is_empty() {
            self.insertion_start(hunk)?
        } else {
            self.locate(number, hunk, &original)?
        };
        self.copy_source_until(start)?;
        for line in &hunk.lines {
            self.apply_line(line);
        }

        Ok(())
    }

    fn finish(
        self,
        line_ending: &str,
        line_changes: LineChanges,
    ) -> Result<PatchedText, WriteError> {
        if self.new_side_terminated && self.source_index < self.source_lines.len() {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }
        let mut output = String::new();
        for line in self
            .output
            .iter()
            .map(AsRef::as_ref)
            .chain(self.source_lines[self.source_index..].iter().copied())
        {
            match line.strip_suffix('\n') {
                Some(content) => {
                    output.push_str(content);
                    output.push_str(line_ending);
                }
                None => output.push_str(line),
            }
        }

        Ok(PatchedText {
            adjusted_hunks: self.adjusted_hunks,
            content: output.into_bytes(),
            line_changes,
        })
    }

    /// Places a hunk without original-side lines exactly where its `-N,0`
    /// header says: after original line `N`.
    fn insertion_start(&self, hunk: &Hunk) -> Result<usize, WriteError> {
        if hunk.old_count != 0 {
            return Err(patch_error(
                "a hunk with only added lines needs a `-N,0` header naming the line it follows; \
                 add context lines to place it",
            ));
        }
        let start = hunk.old_start;
        if start < self.source_index {
            return Err(patch_error(
                "hunks must use ascending, non-overlapping original ranges",
            ));
        }
        if start > self.source_lines.len() {
            return Err(patch_error("hunk starts beyond the end of the target"));
        }
        let modified_index = hunk_start(hunk.new_start, "modified")?;
        if modified_index != self.output.len() + (start - self.source_index) {
            return Err(patch_error(
                "hunk modified range does not match its original range",
            ));
        }

        Ok(start)
    }

    /// Returns where a hunk with original-side lines applies: its header line
    /// when those lines match there exactly or, failing that, ignoring
    /// trailing whitespace; otherwise the single place in the file, as
    /// already changed by earlier hunks, where they match exactly or, failing
    /// that, ignoring trailing whitespace.
    fn locate(
        &mut self,
        number: usize,
        hunk: &Hunk,
        original: &[&TextLine],
    ) -> Result<usize, WriteError> {
        let header_index = hunk_start(hunk.old_start, "original")?;
        if header_index >= self.source_index {
            if block_matches(&self.source_lines, header_index, original, LineMatch::Exact) {
                return Ok(header_index);
            }
            let tolerant = LineMatch::IgnoringTrailingWhitespace;
            if block_matches(&self.source_lines, header_index, original, tolerant) {
                self.adjusted_hunks.push(HunkAdjustment {
                    applied_line: header_index + 1,
                    header_line: hunk.old_start,
                    hunk: number,
                    line_match: tolerant,
                });

                return Ok(header_index);
            }
        }
        let repetitive = || too_repetitive(number);
        let remaining = &self.source_lines[self.source_index..];
        self.comparisons_left = self
            .comparisons_left
            .checked_sub(self.output.len() + remaining.len())
            .ok_or_else(repetitive)?;
        let evolving: Vec<&str> = self
            .output
            .iter()
            .map(AsRef::as_ref)
            .chain(remaining.iter().copied())
            .collect();
        for line_match in [LineMatch::Exact, LineMatch::IgnoringTrailingWhitespace] {
            let candidates =
                find_matches(&evolving, original, line_match, &mut self.comparisons_left)
                    .ok_or_else(repetitive)?;
            match candidates.as_slice() {
                [] => {}
                [candidate] => {
                    let Some(offset) = candidate.checked_sub(self.output.len()) else {
                        return Err(patch_error(format!(
                            "hunk {number} matches the target only at line {}, which is at or \
                             before an earlier hunk; hunks must use ascending, non-overlapping \
                             original ranges",
                            candidate + 1
                        )));
                    };
                    let start = self.source_index + offset;
                    self.adjusted_hunks.push(HunkAdjustment {
                        applied_line: start + 1,
                        header_line: hunk.old_start,
                        hunk: number,
                        line_match,
                    });

                    return Ok(start);
                }
                _ => return Err(ambiguous_hunk_error(number, &candidates, line_match)),
            }
        }

        Err(context_mismatch(
            &self.source_lines,
            header_index,
            original,
            &mut self.comparisons_left,
            &format!(
                "hunk {number} context and removed lines do not match the target at header line \
                 {} or anywhere else",
                hunk.old_start
            ),
        ))
    }

    fn copy_source_until(&mut self, start: usize) -> Result<(), WriteError> {
        if self.new_side_terminated && self.source_index < start {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }
        self.output.extend(
            self.source_lines[self.source_index..start]
                .iter()
                .map(|line| Cow::Borrowed(*line)),
        );
        self.source_index = start;

        Ok(())
    }

    /// Applies one located line; context lines keep the target's own text so
    /// whitespace-tolerant matches never rewrite unchanged lines.
    fn apply_line(&mut self, line: &PatchLine) {
        match line.kind {
            PatchLineKind::Addition => {
                let mut added = line.text.content.clone();
                if line.text.newline {
                    added.push('\n');
                }
                self.output.push(Cow::Owned(added));
                self.new_side_terminated = !line.text.newline;
            }
            PatchLineKind::Context => {
                self.output
                    .push(Cow::Borrowed(self.source_lines[self.source_index]));
                self.source_index += 1;
                self.new_side_terminated = !line.text.newline;
            }
            PatchLineKind::Removal => {
                self.source_index += 1;
            }
        }
    }
}

fn apply_unified_diff(
    requested_path: &str,
    current: Option<&[u8]>,
    patch: &str,
) -> Result<PatchedText, WriteError> {
    let (bom, text) = decode_text(requested_path, current.unwrap_or_default())?;
    let (line_ending, normalized) = normalize_line_endings(requested_path, text)?;
    let diff = parse_patch(
        requested_path,
        current.is_some().then_some(&*normalized),
        patch,
    )?;
    validate_headers(requested_path, current.is_some(), &diff)?;
    let mut patched = apply_hunks(&normalized, &diff.hunks, line_ending)?;
    patched.content.splice(0..0, bom.iter().copied());

    Ok(patched)
}

/// Parses `text` in the form it is written in: a unified diff, which may
/// leave out its file headers or sit inside Codex's `*** Begin Patch` and
/// `*** End Patch` lines, or a Codex `apply_patch` patch, resolved against
/// `current`, the target's text when it exists. Blank lines that end the
/// patch are ignored, and a carriage return outside a `\r\n` line ending
/// rejects every form.
fn parse_patch(path: &str, current: Option<&str>, text: &str) -> Result<UnifiedDiff, WriteError> {
    let normalized = text.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(patch_error("patch contains unsupported carriage returns"));
    }
    let body = normalized.trim_end_matches('\n');
    let body = match body.strip_prefix(CODEX_BEGIN) {
        Some(operations) if codex_target(operations).is_some() => {
            return parse_codex_patch(path, current, operations);
        }
        Some(diff) => diff,
        None => body,
    };
    let body = match body.strip_suffix(CODEX_END) {
        Some(diff) if diff.is_empty() || diff.ends_with('\n') => diff.trim_end_matches('\n'),
        _ => body,
    };
    if !body.starts_with("@@") {
        return parse_unified_diff(&format!("{body}\n"));
    }
    let original = if current.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };

    parse_unified_diff(&format!("--- {original}\n+++ b/{path}\n{body}\n"))
}

/// Returns the path the first line of `text` names when it is a Codex
/// `*** Add File:`, `*** Delete File:`, or `*** Update File:` line.
fn codex_target(text: &str) -> Option<&str> {
    let line = text.lines().next()?;

    [CODEX_ADD_FILE, CODEX_DELETE_FILE, CODEX_UPDATE_FILE]
        .into_iter()
        .find_map(|operation| line.strip_prefix(operation))
        .map(str::trim)
}

/// Translates a patch in Codex's `apply_patch` dialect, which models trained
/// on Codex send, into the hunks this tool applies. `operations` are the
/// lines after its `*** Begin Patch` line, for the write to `path`, whose
/// current text is `current` when the file exists.
///
/// A patch holds one `*** Add File:` operation whose lines all start with
/// `+`, or one `*** Update File:` operation whose hunks each start with
/// `@@`, optionally followed by a line to find first, compared ignoring
/// surrounding whitespace so it may leave out its indentation. Hunks carry no
/// line numbers, so each is searched after the previous hunk for its context
/// and removed lines, exactly or else ignoring trailing whitespace. A hunk with
/// an `@@` line applies at the first match after that line, as in Codex; a
/// hunk without one must match exactly one place after the previous hunk,
/// so a short, repeated hunk is rejected rather than applied to the wrong
/// place. `*** End of File` makes a hunk match only at the end of the file.
/// A hunk of only added lines appends to the file. Deleting or moving a
/// file, and changing several files in one call, are rejected with a
/// correction.
fn parse_codex_patch(
    path: &str,
    current: Option<&str>,
    operations: &str,
) -> Result<UnifiedDiff, WriteError> {
    let operations = match operations.strip_suffix(CODEX_END) {
        Some(body) if body.ends_with('\n') => body,
        _ => operations,
    };
    let mut lines = operations.lines();
    let operation = lines.next().unwrap_or_default();
    let body: Vec<&str> = lines.collect();
    if body.iter().any(|line| codex_target(line).is_some()) {
        return Err(WriteError::Unsupported {
            reason: "a Codex patch may change only the file named by `path`; send one `write` \
                     call per file"
                .to_string(),
        });
    }
    if codex_target(operation) != Some(path) {
        return Err(WriteError::PathMismatch {
            path: path.to_string(),
        });
    }
    if operation.starts_with(CODEX_ADD_FILE) {
        return codex_add_file(path, &body);
    }
    if operation.starts_with(CODEX_DELETE_FILE) {
        return Err(WriteError::Unsupported {
            reason: "`write` cannot delete files; delete it with `bash` if that is available, or \
                     tell the user it should be deleted"
                .to_string(),
        });
    }

    codex_update_file(path, current, &body)
}

/// Translates a Codex `*** Add File:` operation into a creation diff.
fn codex_add_file(path: &str, body: &[&str]) -> Result<UnifiedDiff, WriteError> {
    let lines = body
        .iter()
        .map(|line| {
            line.strip_prefix('+')
                .map(|content| PatchLine::new(PatchLineKind::Addition, content, true))
                .ok_or_else(|| patch_error("every `*** Add File:` line must start with `+`"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let hunks = if lines.is_empty() {
        Vec::new()
    } else {
        vec![Hunk {
            lines,
            new_start: 1,
            old_count: 0,
            old_start: 0,
        }]
    };

    Ok(UnifiedDiff {
        hunks,
        modified_path: format!("b/{path}"),
        original_path: "/dev/null".to_string(),
    })
}

/// One `@@` hunk of a Codex `*** Update File:` operation.
#[derive(Default)]
struct CodexHunk<'a> {
    anchor: Option<&'a str>,
    end_of_file: bool,
    lines: Vec<(PatchLineKind, &'a str)>,
}

/// Translates a Codex `*** Update File:` operation into hunks placed
/// against `current`.
fn codex_update_file(
    path: &str,
    current: Option<&str>,
    body: &[&str],
) -> Result<UnifiedDiff, WriteError> {
    let Some(current) = current else {
        return Err(WriteError::Unsupported {
            reason: "`*** Update File:` names a file that does not exist; create it with `*** Add \
                     File:`"
                .to_string(),
        });
    };
    let body = match body.split_first() {
        Some((line, rest)) if line.starts_with(CODEX_MOVE_TO) => {
            if line[CODEX_MOVE_TO.len()..].trim() != path {
                return Err(WriteError::Unsupported {
                    reason: "`write` cannot move files; create the file at its new path with `*** \
                             Add File:`, then delete the old one with `bash` if that is available"
                        .to_string(),
                });
            }

            rest
        }
        _ => body,
    };
    let codex_hunks = parse_codex_hunks(body)?;
    let source: Vec<&str> = current.split_inclusive('\n').collect();
    // Hunk lines carry no end-of-file marker, so they are searched as if
    // the final line had a line ending.
    let terminated = if current.is_empty() || current.ends_with('\n') {
        Cow::Borrowed(current)
    } else {
        Cow::Owned(format!("{current}\n"))
    };
    let searched: Vec<&str> = terminated.split_inclusive('\n').collect();
    let mut placement = CodexPlacement {
        comparisons_left: MAX_LOCATE_COMPARISONS,
        consumed: 0,
        cursor: 0,
        produced: 0,
        searched: &searched,
        source: &source,
    };
    let hunks = codex_hunks
        .iter()
        .enumerate()
        .map(|(index, codex_hunk)| placement.place(index + 1, codex_hunk))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(UnifiedDiff {
        hunks,
        modified_path: format!("b/{path}"),
        original_path: format!("a/{path}"),
    })
}

/// Splits a Codex update's lines into hunks. Lines before the first `@@`
/// form a hunk of their own, as Codex accepts. A unified-diff range after
/// `@@`, such as `@@ -4,2 +4,3 @@`, is ignored like the line numbers Codex
/// hunks lack.
fn parse_codex_hunks<'a>(body: &[&'a str]) -> Result<Vec<CodexHunk<'a>>, WriteError> {
    let mut hunks: Vec<CodexHunk<'a>> = vec![CodexHunk::default()];
    for line in body {
        let last = hunks.len() - 1;
        if let Some(anchor) = line.strip_prefix("@@") {
            let anchor = anchor.trim();
            let range = anchor
                .strip_prefix('-')
                .is_some_and(|range| range.starts_with(|first: char| first.is_ascii_digit()));
            let hunk = CodexHunk {
                anchor: (!anchor.is_empty() && !range).then_some(anchor),
                ..CodexHunk::default()
            };
            // The first `@@` replaces the unmarked hunk while it is empty.
            let first = &hunks[0];
            if last == 0 && first.lines.is_empty() && !first.end_of_file && first.anchor.is_none() {
                hunks[0] = hunk;
            } else {
                hunks.push(hunk);
            }

            continue;
        }
        let hunk = &mut hunks[last];
        if *line == CODEX_END_OF_FILE {
            hunk.end_of_file = true;

            continue;
        }
        let line = PatchLineKind::split(line).ok_or_else(|| {
            patch_error("Codex patch hunk line must start with space, `+`, or `-`")
        })?;
        hunk.lines.push(line);
    }
    if let Some(number) = hunks.iter().position(|hunk| hunk.lines.is_empty()) {
        return Err(patch_error(format!(
            "hunk {} contains no patch lines",
            number + 1
        )));
    }

    Ok(hunks)
}

/// Places a Codex update's hunks in order against the target's lines.
struct CodexPlacement<'a, 'b> {
    comparisons_left: usize,
    /// Source lines before the end of the previous hunk.
    consumed: usize,
    /// Source line from which the next hunk is searched.
    cursor: usize,
    /// Modified lines before the end of the previous hunk.
    produced: usize,
    /// `source` with a line ending on every line.
    searched: &'b [&'a str],
    source: &'b [&'a str],
}

impl CodexPlacement<'_, '_> {
    /// Places hunk `number` after the previous one and returns it with the
    /// line numbers the unified-diff applier expects.
    fn place(&mut self, number: usize, codex_hunk: &CodexHunk<'_>) -> Result<Hunk, WriteError> {
        if let Some(anchor) = codex_hunk.anchor {
            let anchor_line = TextLine {
                content: anchor.to_string(),
                newline: true,
            };
            let found = self.anchor_after(number, anchor)?;
            let found = found.ok_or_else(|| {
                self.mismatch(
                    &[&anchor_line],
                    &format!(
                        "hunk {number}'s `@@ {anchor}` line does not occur after the previous hunk"
                    ),
                )
            })?;
            self.cursor = found + 1;
        }
        let original: Vec<TextLine> = codex_hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != PatchLineKind::Addition)
            .map(|(_, content)| TextLine {
                content: (*content).to_string(),
                newline: true,
            })
            .collect();
        if original.is_empty() {
            if codex_hunk.anchor.is_some() {
                return Err(patch_error(format!(
                    "hunk {number} has only added lines after its `@@` line; add the line they \
                     follow as a context line starting with a space"
                )));
            }

            return self.append(number, codex_hunk);
        }
        let original: Vec<&TextLine> = original.iter().collect();
        let from = if codex_hunk.end_of_file {
            self.source
                .len()
                .saturating_sub(original.len())
                .max(self.cursor)
        } else {
            self.cursor
        };
        // An `@@` line or the end of the file already pins the place.
        let limit = if codex_hunk.anchor.is_some() || codex_hunk.end_of_file {
            1
        } else {
            2
        };
        let start = match self
            .matches_after(number, from, &original, limit)?
            .as_slice()
        {
            [start] => *start,
            [] => {
                let place = if codex_hunk.end_of_file {
                    " at the end of the file"
                } else {
                    ""
                };

                return Err(self.mismatch(
                    &original,
                    &format!(
                        "hunk {number} context and removed lines do not occur after the previous \
                         hunk{place}"
                    ),
                ));
            }
            [first, second, ..] => {
                return Err(patch_error(format!(
                    "hunk {number} matches more than one place after the previous hunk, at lines \
                     {} and {}; add an `@@` line naming a line above the change, such as its \
                     enclosing function, or add context lines",
                    first + 1,
                    second + 1
                )));
            }
        };

        Ok(self.hunk(start, start + 1, codex_hunk))
    }

    /// Returns the first source line after the previous hunk that equals
    /// `anchor` once surrounding whitespace is ignored on both, as Codex
    /// matches an `@@` line, which models write with or without the line's
    /// indentation.
    ///
    /// # Errors
    /// Returns why hunk `number` cannot be located once the comparison
    /// budget runs out.
    fn anchor_after(&mut self, number: usize, anchor: &str) -> Result<Option<usize>, WriteError> {
        for (offset, line) in self.searched[self.cursor..].iter().enumerate() {
            self.comparisons_left = self
                .comparisons_left
                .checked_sub(1)
                .ok_or_else(|| too_repetitive(number))?;
            if line.trim() == anchor {
                return Ok(Some(self.cursor + offset));
            }
        }

        Ok(None)
    }

    /// Returns up to `limit` indexes, in order, from `from` where `original`
    /// matches the source exactly or, when it matches nowhere exactly,
    /// ignoring trailing whitespace.
    ///
    /// # Errors
    /// Returns why hunk `number` cannot be located once the comparison
    /// budget runs out.
    fn matches_after(
        &mut self,
        number: usize,
        from: usize,
        original: &[&TextLine],
        limit: usize,
    ) -> Result<Vec<usize>, WriteError> {
        let Some(last) = self.searched.len().checked_sub(original.len()) else {
            return Ok(Vec::new());
        };
        for line_match in [LineMatch::Exact, LineMatch::IgnoringTrailingWhitespace] {
            let mut found = Vec::new();
            'starts: for start in from..=last {
                for (source, expected) in self.searched[start..].iter().zip(original) {
                    self.comparisons_left = self
                        .comparisons_left
                        .checked_sub(1)
                        .ok_or_else(|| too_repetitive(number))?;
                    if !line_match.matches(source, expected) {
                        continue 'starts;
                    }
                }
                found.push(start);
                if found.len() == limit {
                    break;
                }
            }
            if !found.is_empty() {
                return Ok(found);
            }
        }

        Ok(Vec::new())
    }

    fn mismatch(&mut self, original: &[&TextLine], problem: &str) -> WriteError {
        context_mismatch(
            self.source,
            self.cursor,
            original,
            &mut self.comparisons_left,
            problem,
        )
    }

    /// Places a hunk of only added lines after the target's last line, which
    /// it rewrites with a line ending when that line has none.
    fn append(&mut self, number: usize, codex_hunk: &CodexHunk<'_>) -> Result<Hunk, WriteError> {
        let Some(last) = self.source.len().checked_sub(1).filter(|last| {
            let line = self.source[*last];

            !line.ends_with('\n')
        }) else {
            return Ok(self.hunk(self.source.len(), self.source.len(), codex_hunk));
        };
        if last < self.consumed {
            return Err(patch_error(format!(
                "hunk {number} appends after an earlier hunk that ends at the file's last line; \
                 merge them into one hunk"
            )));
        }

        Ok(self.hunk(last, last + 1, codex_hunk))
    }

    /// Builds the hunk for `codex_hunk`, starting at source index `start`,
    /// with `old_start` its header's original start. A line matched against
    /// the target's final line, when that line has no line ending, keeps it
    /// missing; a hunk that adds lines after that line rewrites it with a
    /// line ending first.
    fn hunk(&mut self, start: usize, old_start: usize, codex_hunk: &CodexHunk<'_>) -> Hunk {
        let unterminated = self
            .source
            .last()
            .filter(|line| !line.ends_with('\n'))
            .map(|_| self.source.len() - 1);
        let mut lines = Vec::new();
        let mut position = start;
        let appends_to_unterminated = start < self.source.len()
            && codex_hunk
                .lines
                .iter()
                .all(|(kind, _)| *kind == PatchLineKind::Addition);
        if appends_to_unterminated {
            lines.extend(self.terminated(start));
            position += 1;
        }
        for (index, (kind, content)) in codex_hunk.lines.iter().enumerate() {
            if *kind == PatchLineKind::Addition {
                lines.push(PatchLine::new(*kind, content, true));

                continue;
            }
            let at_unterminated = Some(position) == unterminated;
            let followed = index + 1 < codex_hunk.lines.len();
            if at_unterminated && *kind == PatchLineKind::Context && followed {
                lines.extend(self.terminated(position));
            } else {
                lines.push(PatchLine::new(*kind, content, !at_unterminated));
            }
            position += 1;
        }
        let old_count = position - start;
        let new_count = lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Removal)
            .count();
        let new_index = self.produced + (start - self.consumed);
        self.consumed = start + old_count;
        self.produced = new_index + new_count;
        self.cursor = self.cursor.max(self.consumed);

        Hunk {
            lines,
            new_start: new_index + 1,
            old_count,
            old_start,
        }
    }

    /// Returns the lines that rewrite the target's unterminated final line,
    /// at source index `index`, with a line ending.
    fn terminated(&self, index: usize) -> [PatchLine; 2] {
        let content = self.source[index];

        [
            PatchLine::new(PatchLineKind::Removal, content, false),
            PatchLine::new(PatchLineKind::Addition, content, true),
        ]
    }
}

fn parse_unified_diff(patch: &str) -> Result<UnifiedDiff, WriteError> {
    let normalized = patch.replace("\r\n", "\n");
    if normalized.contains('\r') {
        return Err(patch_error("patch contains unsupported carriage returns"));
    }
    let mut lines = normalized.split_inclusive('\n').peekable();
    let original_path = parse_header(lines.next(), "--- ")?;
    let modified_path = parse_header(lines.next(), "+++ ")?;
    let mut hunks = Vec::new();
    let mut termination = PatchTermination::default();
    while let Some(header) = lines.next() {
        let header = strip_patch_newline(header);
        let (old_start, old_count, new_start) = parse_hunk_header(header)?;
        let hunk_lines = parse_hunk_lines(&mut lines, &mut termination)?;
        if hunk_lines.is_empty() {
            return Err(patch_error("hunk contains no patch lines"));
        }
        hunks.push(Hunk {
            lines: hunk_lines,
            new_start,
            old_count,
            old_start,
        });
    }
    Ok(UnifiedDiff {
        hunks,
        modified_path,
        original_path,
    })
}

fn parse_hunk_lines<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    termination: &mut PatchTermination,
) -> Result<Vec<PatchLine>, WriteError>
where
    I: Iterator<Item = &'a str>,
{
    let mut hunk_lines = Vec::new();
    let mut previous_line_had_marker = false;
    while let Some(line) = lines.next_if(|line| !line.starts_with("@@ ")) {
        let line = strip_patch_newline(line);
        if line == "\\ No newline at end of file" {
            termination.apply_marker(&mut hunk_lines, previous_line_had_marker)?;
            previous_line_had_marker = true;

            continue;
        }
        let patch_line = parse_patch_line(line)?;
        termination.validate_line(patch_line.kind)?;
        hunk_lines.push(patch_line);
        previous_line_had_marker = false;
    }

    Ok(hunk_lines)
}

/// Parses one hunk line. An empty line is a blank context line whose
/// leading space was dropped, as editors and models often do.
fn parse_patch_line(line: &str) -> Result<PatchLine, WriteError> {
    let (kind, content) = PatchLineKind::split(line)
        .ok_or_else(|| patch_error("hunk line must start with space, `+`, or `-`"))?;

    Ok(PatchLine::new(kind, content, true))
}

fn parse_header(line: Option<&str>, prefix: &str) -> Result<String, WriteError> {
    let line = line.ok_or_else(|| patch_error("patch is missing file headers"))?;
    let line = strip_patch_newline(line);
    let path = line
        .strip_prefix(prefix)
        .ok_or_else(|| patch_error("patch must start with `---` and `+++` file headers"))?
        .split('\t')
        .next()
        .unwrap_or_default();
    if path.is_empty() {
        return Err(patch_error("patch file header path must not be empty"));
    }

    Ok(path.to_string())
}

/// Returns the original start and count and the modified start. The
/// modified count is validated but unused because counts are recounted.
fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize), WriteError> {
    let ranges = header
        .strip_prefix("@@ -")
        .and_then(|header| header.split_once(" @@").map(|(ranges, _)| ranges))
        .ok_or_else(|| patch_error("invalid unified diff hunk header"))?;
    let (old_range, new_range) = ranges
        .split_once(" +")
        .ok_or_else(|| patch_error("invalid unified diff hunk ranges"))?;
    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, _) = parse_range(new_range)?;

    Ok((old_start, old_count, new_start))
}

fn parse_range(range: &str) -> Result<(usize, usize), WriteError> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    let start = start
        .parse::<usize>()
        .map_err(|_| patch_error("hunk range start is not a valid integer"))?;
    let count = count
        .parse::<usize>()
        .map_err(|_| patch_error("hunk range count is not a valid integer"))?;

    Ok((start, count))
}

fn validate_headers(path: &str, exists: bool, diff: &UnifiedDiff) -> Result<(), WriteError> {
    if diff.modified_path == "/dev/null" {
        return Err(WriteError::Unsupported {
            reason: "file deletion is not supported".to_string(),
        });
    }
    if header_path(&diff.modified_path) != path {
        return Err(WriteError::PathMismatch {
            path: path.to_string(),
        });
    }
    if exists {
        if diff.original_path == "/dev/null" {
            return Err(WriteError::Unsupported {
                reason: "create patch targets an existing file".to_string(),
            });
        }
        if header_path(&diff.original_path) != path {
            return Err(WriteError::Unsupported {
                reason: "file rename is not supported".to_string(),
            });
        }
    } else if diff.original_path != "/dev/null" {
        return Err(WriteError::Unsupported {
            reason: "new files require `/dev/null` as the original path".to_string(),
        });
    }

    Ok(())
}

fn header_path(path: &str) -> &str {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

fn decode_text<'a>(path: &str, bytes: &'a [u8]) -> Result<(&'a [u8], &'a str), WriteError> {
    let (bom, text) = bytes
        .strip_prefix(BYTE_ORDER_MARK)
        .map_or((&[][..], bytes), |text| (BYTE_ORDER_MARK, text));
    let text = std::str::from_utf8(text).map_err(|_| WriteError::BinaryTarget {
        path: path.to_string(),
    })?;

    Ok((bom, text))
}

fn normalize_line_endings<'a>(
    path: &str,
    text: &'a str,
) -> Result<(&'static str, std::borrow::Cow<'a, str>), WriteError> {
    let bytes = text.as_bytes();
    let has_crlf = bytes.windows(2).any(|pair| pair == b"\r\n");
    let has_bare_lf = bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\n' && (index == 0 || bytes[index - 1] != b'\r'));
    let has_lone_cr = bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\r' && bytes.get(index + 1) != Some(&b'\n'));
    if has_lone_cr || (has_crlf && has_bare_lf) {
        return Err(WriteError::Unsupported {
            reason: format!("mixed or unsupported line endings in `{path}`"),
        });
    }
    if has_crlf {
        return Ok(("\r\n", std::borrow::Cow::Owned(text.replace("\r\n", "\n"))));
    }

    Ok(("\n", std::borrow::Cow::Borrowed(text)))
}

fn apply_hunks(
    current: &str,
    hunks: &[Hunk],
    line_ending: &str,
) -> Result<PatchedText, WriteError> {
    let mut applier = HunkApplier::new(current);
    for (index, hunk) in hunks.iter().enumerate() {
        applier.apply_hunk(index + 1, hunk)?;
    }

    applier.finish(line_ending, LineChanges::of(hunks))
}

/// Converts the one-based start of a non-empty hunk side to a line index.
///
/// Both callers pass a side with at least one line: parsing rejects empty
/// hunks, and a hunk is located by its original lines only when it has some.
fn hunk_start(start: usize, side: &str) -> Result<usize, WriteError> {
    start
        .checked_sub(1)
        .ok_or_else(|| patch_error(format!("non-empty hunk cannot start at {side} line zero")))
}

fn block_matches(
    lines: &[&str],
    start: usize,
    original: &[&TextLine],
    line_match: LineMatch,
) -> bool {
    lines
        .get(start..start.saturating_add(original.len()))
        .is_some_and(|window| {
            window
                .iter()
                .zip(original)
                .all(|(source, expected)| line_match.matches(source, expected))
        })
}

/// Returns every start index where `original` matches `lines`, or `None`
/// once the comparison budget runs out.
fn find_matches(
    lines: &[&str],
    original: &[&TextLine],
    line_match: LineMatch,
    comparisons_left: &mut usize,
) -> Option<Vec<usize>> {
    let mut matches = Vec::new();
    let Some(last_start) = lines.len().checked_sub(original.len()) else {
        return Some(matches);
    };
    'starts: for start in 0..=last_start {
        for (source, expected) in lines[start..].iter().zip(original) {
            *comparisons_left = comparisons_left.checked_sub(1)?;
            if !line_match.matches(source, expected) {
                continue 'starts;
            }
        }
        matches.push(start);
    }

    Some(matches)
}

fn ambiguous_hunk_error(number: usize, candidates: &[usize], line_match: LineMatch) -> WriteError {
    let mut lines = candidates
        .iter()
        .take(MAX_REPORTED_CANDIDATES)
        .map(|candidate| (candidate + 1).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if candidates.len() > MAX_REPORTED_CANDIDATES {
        lines.push_str(", ...");
    }
    let numbering = if number > 1 {
        " (counted after this patch's earlier hunks)"
    } else {
        ""
    };
    let comparison = match line_match {
        LineMatch::Exact => "",
        LineMatch::IgnoringTrailingWhitespace => " after ignoring trailing whitespace",
    };

    patch_error(format!(
        "hunk {number} does not match at its header line and matches the target at lines \
         {lines}{numbering}{comparison}; re-read the file and add context lines so the hunk \
         matches exactly one place"
    ))
}

/// Returns the error for hunk `number` when locating it exhausts the
/// patch's comparison budget.
fn too_repetitive(number: usize) -> WriteError {
    patch_error(format!(
        "hunk {number} is too repetitive to locate within the comparison limit; add distinctive \
         context lines or split the patch into smaller writes"
    ))
}

/// Returns the error for a hunk whose `original` lines match nowhere,
/// described by `problem`, with the target's current lines where those
/// lines most nearly match or, failing that, around index `near`, so the
/// model can copy them instead of reading the file again.
///
/// The nearest match is the place where the most non-blank lines of the
/// hunk match the target ignoring surrounding whitespace, the one closest
/// to `near` among equals; finding it spends the patch's comparison budget, and
/// once that runs out the lines around `near` are shown. At most
/// [`MAX_SHOWN_LINES`] whole lines within [`MAX_SHOWN_BYTES`] are shown.
fn context_mismatch(
    lines: &[&str],
    near: usize,
    original: &[&TextLine],
    comparisons_left: &mut usize,
    problem: &str,
) -> WriteError {
    let nearest = nearest_match(lines, original, near, comparisons_left);
    let anchor = nearest.unwrap_or(near).min(lines.len().saturating_sub(1));
    let start = anchor.saturating_sub(SHOWN_CONTEXT_LINES);
    let end = (anchor + original.len() + SHOWN_CONTEXT_LINES)
        .min(lines.len())
        .min(start + MAX_SHOWN_LINES);
    let mut content = String::new();
    for line in &lines[start..end] {
        if content.len() + line.len() > MAX_SHOWN_BYTES {
            break;
        }
        content.push_str(line);
    }
    if content.is_empty() {
        return patch_error(format!(
            "{problem}; re-read the file and copy those lines exactly"
        ));
    }
    let place = if nearest.is_some() {
        "the lines where the hunk most nearly matches are attached"
    } else {
        "the lines around the hunk's position are attached"
    };

    WriteError::ContextMismatch {
        content,
        reason: schema_contract::bounded_diagnostic(format!("{problem}; {place}")),
        start_line: start + 1,
    }
}

/// Returns the start index where the most non-blank `original` lines match
/// `lines` ignoring surrounding whitespace, the one closest to `near` among
/// equals, or `None` when no line matches or the comparison budget runs out.
fn nearest_match(
    lines: &[&str],
    original: &[&TextLine],
    near: usize,
    comparisons_left: &mut usize,
) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for start in 0..lines.len() {
        let mut score = 0;
        for (source, expected) in lines[start..].iter().zip(original) {
            *comparisons_left = comparisons_left.checked_sub(1)?;
            let source = strip_patch_newline(source);
            if !expected.content.trim().is_empty() && source.trim() == expected.content.trim() {
                score += 1;
            }
        }
        let better = best.is_none_or(|(best_score, best_start)| {
            score > best_score
                || (score == best_score && start.abs_diff(near) < best_start.abs_diff(near))
        });
        if score > 0 && better {
            best = Some((score, start));
        }
    }

    best.map(|(_, start)| start)
}

fn strip_patch_newline(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

fn patch_error(reason: impl Into<String>) -> WriteError {
    WriteError::Patch {
        reason: schema_contract::bounded_diagnostic(reason.into()),
    }
}

#[cfg(test)]
#[path = "write_test.rs"]
mod tests;

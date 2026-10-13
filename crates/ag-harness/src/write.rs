use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use opentelemetry::trace::FutureExt as _;
use serde::Serialize;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;

use crate::effect::Effects;
use crate::file_system::FileSystem;
use crate::reservation::WriteJournal;
use crate::schema_contract;
use crate::tool::WriteArguments;

const BYTE_ORDER_MARK: &[u8] = b"\xef\xbb\xbf";
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Line comparisons one patch may spend relocating hunks whose header
/// position does not match, so repetitive targets cannot stall the turn.
const MAX_LOCATE_COMPARISONS: usize = 8 * 1024 * 1024;
/// Candidate line numbers named when a hunk matches several places.
const MAX_REPORTED_CANDIDATES: usize = 8;

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

    pub(crate) fn to_tool_result(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    fn new(path: String, bytes_written: usize, adjusted_hunks: Vec<HunkAdjustment>) -> Self {
        Self {
            adjusted_hunks,
            bytes_written,
            path,
            status: "applied",
        }
    }
}

/// Failure returned while validating or applying one repository write.
#[derive(Debug, Error)]
pub enum WriteError {
    /// The existing file is not valid UTF-8 text.
    #[error("write target `{path}` is not UTF-8 text")]
    BinaryTarget {
        /// Repository-relative target path.
        path: String,
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
        struct RejectedWrite<'a> {
            error: String,
            path: &'a str,
            status: &'static str,
        }

        serde_json::to_string(&RejectedWrite {
            error: schema_contract::bounded_diagnostic(self),
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

            Ok(WriteOutput::new(path, bytes_written, adjusted_hunks))
        };
        // Retain the tool context even after cancellation ends its lifecycle
        // span.
        let task = tokio::spawn(operation.with_current_context());

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

/// Patched file bytes plus the hunks that needed relocation.
#[derive(Debug)]
struct PatchedText {
    adjusted_hunks: Vec<HunkAdjustment>,
    content: Vec<u8>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct PatchLine {
    kind: PatchLineKind,
    text: TextLine,
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

    fn finish(self, line_ending: &str) -> Result<PatchedText, WriteError> {
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
        let modified_index = hunk_start(hunk.new_start, hunk.lines.len(), "modified")?;
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
        let header_index = hunk_start(hunk.old_start, original.len(), "original")?;
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
        let too_repetitive = || {
            patch_error(format!(
                "hunk {number} is too repetitive to locate within the comparison limit; add \
                 distinctive context lines or split the patch into smaller writes"
            ))
        };
        let remaining = &self.source_lines[self.source_index..];
        self.comparisons_left = self
            .comparisons_left
            .checked_sub(self.output.len() + remaining.len())
            .ok_or_else(too_repetitive)?;
        let evolving: Vec<&str> = self
            .output
            .iter()
            .map(AsRef::as_ref)
            .chain(remaining.iter().copied())
            .collect();
        for line_match in [LineMatch::Exact, LineMatch::IgnoringTrailingWhitespace] {
            let candidates =
                find_matches(&evolving, original, line_match, &mut self.comparisons_left)
                    .ok_or_else(too_repetitive)?;
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

        Err(patch_error(format!(
            "hunk {number} context and removed lines do not match the target at header line {} or \
             anywhere else; re-read the file and copy those lines exactly",
            hunk.old_start
        )))
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
    unified_patch: &str,
) -> Result<PatchedText, WriteError> {
    let diff = parse_unified_diff(unified_patch)?;
    validate_headers(requested_path, current.is_some(), &diff)?;
    let (bom, text) = decode_text(requested_path, current.unwrap_or_default())?;
    let (line_ending, normalized) = normalize_line_endings(requested_path, text)?;
    let mut patched = apply_hunks(&normalized, &diff.hunks, line_ending)?;
    patched.content.splice(0..0, bom.iter().copied());

    Ok(patched)
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

fn parse_patch_line(line: &str) -> Result<PatchLine, WriteError> {
    let (prefix, content) = line
        .split_at_checked(1)
        .ok_or_else(|| patch_error("hunk contains an empty patch line"))?;
    let kind = match prefix {
        "+" => PatchLineKind::Addition,
        " " => PatchLineKind::Context,
        "-" => PatchLineKind::Removal,
        _ => return Err(patch_error("hunk line must start with space, `+`, or `-`")),
    };

    Ok(PatchLine {
        kind,
        text: TextLine {
            content: content.to_string(),
            newline: true,
        },
    })
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

    applier.finish(line_ending)
}

fn hunk_start(start: usize, count: usize, side: &str) -> Result<usize, WriteError> {
    if count == 0 {
        return Ok(start);
    }

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

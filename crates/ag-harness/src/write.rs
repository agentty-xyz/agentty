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
/// Line comparisons one patch may spend locating its hunks, so repetitive
/// targets cannot stall the turn on the async runtime.
const MAX_LOCATE_COMPARISONS: usize = 8 * 1024 * 1024;

/// Successful result of applying one repository write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteOutput {
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

    fn new(path: String, bytes_written: usize) -> Self {
        Self {
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
        let result = apply_unified_diff(arguments.path(), current.as_deref(), arguments.patch())?;
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

            Ok(WriteOutput::new(path, bytes_written))
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

/// One hunk whose original-side lines locate it in the target. Header line
/// numbers are only a placement hint: models routinely miscount them, so
/// counts are derived from the hunk body instead of validated.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Hunk {
    lines: Vec<PatchLine>,
    old_range: OldRange,
}

impl Hunk {
    /// Returns the zero-based target line the header suggests, when it
    /// declared one. An insertion-only hunk has no lines to locate, so only
    /// an explicit `-N,0` range places it, after line `N`; any other range
    /// cannot tell whether it inserts before or after line `N`.
    fn start_hint(&self) -> Option<usize> {
        let old_start = self.old_range.start?;
        if self.original_lines().next().is_none() {
            return (self.old_range.count == Some(0)).then_some(old_start);
        }

        Some(old_start.saturating_sub(1))
    }

    /// Returns the lines the target must contain where the hunk applies.
    fn original_lines(&self) -> impl Iterator<Item = &TextLine> {
        self.lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Addition)
            .map(|line| &line.text)
    }
}

/// Original-side range a hunk header declares; either part is `None` when
/// the header omits it or it does not parse.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct OldRange {
    count: Option<usize>,
    start: Option<usize>,
}

/// Progressively looser line comparisons used when locating a hunk; each one
/// accepts every line the stricter ones accept.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LineMatch {
    Exact,
    IgnoreTrailingWhitespace,
    IgnoreSurroundingWhitespace,
}

impl LineMatch {
    /// Returns the strictest comparison under which `source` matches
    /// `expected`, or `None` when even the loosest one rejects it.
    fn strictest(source: &str, expected: &TextLine) -> Option<Self> {
        if source.ends_with('\n') != expected.newline {
            return None;
        }
        let source = source.strip_suffix('\n').unwrap_or(source);
        if source.trim() != expected.content.trim() {
            return None;
        }

        Some(if source == expected.content {
            Self::Exact
        } else if source.trim_end() == expected.content.trim_end() {
            Self::IgnoreTrailingWhitespace
        } else {
            Self::IgnoreSurroundingWhitespace
        })
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

struct HunkApplier<'a> {
    comparisons_left: usize,
    new_side_terminated: bool,
    output: String,
    source_index: usize,
    source_lines: Vec<&'a str>,
}

impl<'a> HunkApplier<'a> {
    fn new(current: &'a str) -> Self {
        Self {
            comparisons_left: MAX_LOCATE_COMPARISONS,
            new_side_terminated: false,
            output: String::with_capacity(current.len()),
            source_index: 0,
            source_lines: current.split_inclusive('\n').collect(),
        }
    }

    fn apply_hunk(&mut self, hunk: &Hunk) -> Result<(), WriteError> {
        let start = self.locate(hunk)?;
        self.copy_source_until(start)?;
        for line in &hunk.lines {
            self.apply_line(line);
        }

        Ok(())
    }

    fn finish(mut self) -> Result<String, WriteError> {
        if self.new_side_terminated && self.source_index < self.source_lines.len() {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }
        for source_line in &self.source_lines[self.source_index..] {
            self.output.push_str(source_line);
        }

        Ok(self.output)
    }

    /// Finds where `hunk` applies at or after the previous hunk, preferring
    /// the strictest [`LineMatch`] and then the match nearest the header's
    /// hint. Matches tied for nearest, including several matches without a
    /// hint, are ambiguous and rejected, as is a looser match nearer the hint
    /// than the preferred one. An insertion-only hunk applies at its hint, or
    /// anywhere only when the target is empty.
    fn locate(&mut self, hunk: &Hunk) -> Result<usize, WriteError> {
        let original: Vec<&TextLine> = hunk.original_lines().collect();
        let hint = hunk.start_hint();
        if original.is_empty() {
            let start = match hint {
                Some(start) => start,
                None if self.source_lines.is_empty() => 0,
                None => {
                    return Err(patch_error(
                        "a hunk without context or removed lines needs a `-N,0` header naming the \
                         line it follows; add context lines to place it",
                    ));
                }
            };
            if start < self.source_index {
                return Err(patch_error(
                    "hunks must be ordered by their position in the target",
                ));
            }
            if start > self.source_lines.len() {
                return Err(patch_error("hunk starts beyond the end of the target"));
            }

            return Ok(start);
        }
        let end = (self.source_lines.len() + 1).saturating_sub(original.len());
        let mut matches = Vec::new();
        for start in self.source_index..end {
            if let Some(line_match) = self.match_at(start, &original)? {
                matches.push((start, line_match));
            }
        }
        let distance = |start: usize| hint.map_or(0, |hint| start.abs_diff(hint));
        let Some(&(start, line_match)) = matches
            .iter()
            .min_by_key(|(start, line_match)| (*line_match, distance(*start)))
        else {
            return Err(patch_error(
                "hunk context does not match the target at or after the previous hunk; re-read \
                 the file and send its current lines in file order",
            ));
        };
        let nearest = distance(start);
        let tied = matches
            .iter()
            .filter(|(other, other_match)| {
                *other_match == line_match && distance(*other) == nearest
            })
            .count();
        if tied > 1 {
            return Err(patch_error(
                "hunk context matches the target at more than one position; add context lines or \
                 a header line number that identifies one",
            ));
        }
        if matches.iter().any(|(other, _)| distance(*other) < nearest) {
            return Err(patch_error(
                "hunk context matches one position closely and another, nearer the header line \
                 number, only after ignoring whitespace; re-read the file and copy its current \
                 lines exactly",
            ));
        }

        Ok(start)
    }

    /// Returns the loosest [`LineMatch`] any of `original` needs at `start`,
    /// or `None` when a line does not match, charging each line comparison
    /// to the patch's budget.
    fn match_at(
        &mut self,
        start: usize,
        original: &[&TextLine],
    ) -> Result<Option<LineMatch>, WriteError> {
        let mut needed = LineMatch::Exact;
        for (source, expected) in self.source_lines[start..].iter().zip(original) {
            self.comparisons_left = self.comparisons_left.checked_sub(1).ok_or_else(|| {
                patch_error(
                    "hunk context is too repetitive to locate within the comparison limit; add \
                     distinctive context lines or split the patch into smaller writes",
                )
            })?;
            let Some(line_match) = LineMatch::strictest(source, expected) else {
                return Ok(None);
            };
            needed = needed.max(line_match);
        }

        Ok(Some(needed))
    }

    fn copy_source_until(&mut self, start: usize) -> Result<(), WriteError> {
        if self.new_side_terminated && self.source_index < start {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }
        for source_line in &self.source_lines[self.source_index..start] {
            self.output.push_str(source_line);
        }
        self.source_index = start;

        Ok(())
    }

    /// Applies one located line; context lines keep the target's own text so
    /// whitespace-tolerant matches never rewrite unchanged lines.
    fn apply_line(&mut self, line: &PatchLine) {
        match line.kind {
            PatchLineKind::Addition => {
                push_text_line(&mut self.output, &line.text);
                self.new_side_terminated = !line.text.newline;
            }
            PatchLineKind::Context => {
                self.output.push_str(self.source_lines[self.source_index]);
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
) -> Result<Vec<u8>, WriteError> {
    let diff = parse_unified_diff(unified_patch)?;
    validate_headers(requested_path, current.is_some(), &diff)?;
    let (bom, text) = decode_text(requested_path, current.unwrap_or_default())?;
    let (line_ending, normalized) = normalize_line_endings(requested_path, text)?;
    let mut output = apply_hunks(&normalized, &diff.hunks)?;
    if line_ending == "\r\n" {
        output = output.replace('\n', "\r\n");
    }
    let mut bytes = Vec::with_capacity(bom.len() + output.len());
    bytes.extend_from_slice(bom);
    bytes.extend_from_slice(output.as_bytes());

    Ok(bytes)
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
        let old_range = parse_hunk_header(strip_patch_newline(header))?;
        let hunk_lines = parse_hunk_lines(&mut lines, &mut termination)?;
        if hunk_lines.is_empty() {
            return Err(patch_error("hunk contains no patch lines"));
        }
        hunks.push(Hunk {
            lines: hunk_lines,
            old_range,
        });
    }
    Ok(UnifiedDiff {
        hunks,
        modified_path,
        original_path,
    })
}

/// Parses one hunk body. A bare empty line is read as an empty context line,
/// which models emit when they drop the leading space; trailing bare lines
/// are padding and are discarded.
fn parse_hunk_lines<'a, I>(
    lines: &mut std::iter::Peekable<I>,
    termination: &mut PatchTermination,
) -> Result<Vec<PatchLine>, WriteError>
where
    I: Iterator<Item = &'a str>,
{
    let mut hunk_lines = Vec::new();
    let mut previous_line_had_marker = false;
    let mut trailing_bare_lines = 0_usize;
    while let Some(line) = lines.next_if(|line| !line.starts_with("@@")) {
        let line = strip_patch_newline(line);
        if line == "\\ No newline at end of file" {
            termination.apply_marker(&mut hunk_lines, previous_line_had_marker)?;
            previous_line_had_marker = true;
            trailing_bare_lines = 0;

            continue;
        }
        let patch_line = parse_patch_line(line)?;
        termination.validate_line(patch_line.kind)?;
        hunk_lines.push(patch_line);
        previous_line_had_marker = false;
        trailing_bare_lines = if line.is_empty() {
            trailing_bare_lines + 1
        } else {
            0
        };
    }
    hunk_lines.truncate(hunk_lines.len() - trailing_bare_lines);

    Ok(hunk_lines)
}

fn parse_patch_line(line: &str) -> Result<PatchLine, WriteError> {
    let (prefix, content) = line.split_at_checked(1).unwrap_or((" ", ""));
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

/// Returns the original range a hunk header declares, empty for a bare `@@`
/// header or ranges that do not parse; counts are never validated.
fn parse_hunk_header(header: &str) -> Result<OldRange, WriteError> {
    let ranges = header
        .strip_prefix("@@")
        .ok_or_else(|| patch_error("invalid unified diff hunk header"))?;
    let Some(old_range) = ranges
        .trim_start()
        .strip_prefix('-')
        .and_then(|ranges| ranges.split(' ').next())
    else {
        return Ok(OldRange::default());
    };
    let (start, count) = old_range
        .split_once(',')
        .map_or((old_range, None), |(start, count)| (start, Some(count)));

    Ok(OldRange {
        count: count.and_then(|count| count.parse().ok()),
        start: start.parse().ok(),
    })
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

fn apply_hunks(current: &str, hunks: &[Hunk]) -> Result<String, WriteError> {
    let mut applier = HunkApplier::new(current);
    for hunk in hunks {
        applier.apply_hunk(hunk)?;
    }

    applier.finish()
}

fn push_text_line(output: &mut String, line: &TextLine) {
    output.push_str(&line.content);
    if line.newline {
        output.push('\n');
    }
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

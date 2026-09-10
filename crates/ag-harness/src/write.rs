use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;

use crate::file_system::FileSystem;
use crate::schema_contract;
use crate::tool::WriteArguments;

const BYTE_ORDER_MARK: &[u8] = b"\xef\xbb\xbf";
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

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
    /// Atomically replacing the target failed.
    #[error("failed to write target `{path}`: {source}")]
    WriteTarget {
        /// Repository-relative target path.
        path: String,
        /// Underlying filesystem failure.
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
    file_system: Arc<dyn FileSystem>,
    repository_root: PathBuf,
}

impl WriteTool {
    pub(crate) fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self {
            file_system,
            repository_root,
        }
    }

    pub(crate) async fn execute(
        &self,
        arguments: &WriteArguments,
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
        self.file_system
            .replace_beneath(&root, Path::new(arguments.path()), current, result)
            .await
            .map_err(|source| WriteError::WriteTarget {
                path: arguments.path().to_string(),
                source,
            })?;

        Ok(WriteOutput::new(
            arguments.path().to_string(),
            bytes_written,
        ))
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct Hunk {
    lines: Vec<PatchLine>,
    new_count: usize,
    new_start: usize,
    old_count: usize,
    old_start: usize,
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
    new_side_terminated: bool,
    output: String,
    output_index: usize,
    previous_original_end: Option<usize>,
    source_index: usize,
    source_lines: std::str::SplitInclusive<'a, char>,
}

impl<'a> HunkApplier<'a> {
    fn new(current: &'a str) -> Self {
        Self {
            new_side_terminated: false,
            output: String::with_capacity(current.len()),
            output_index: 0,
            previous_original_end: None,
            source_index: 0,
            source_lines: current.split_inclusive('\n'),
        }
    }

    fn apply_hunk(&mut self, hunk: &Hunk) -> Result<(), WriteError> {
        let original_index = hunk_start(hunk.old_start, hunk.old_count, "original")?;
        let modified_index = hunk_start(hunk.new_start, hunk.new_count, "modified")?;
        self.validate_original_range(original_index, hunk.old_count)?;
        self.copy_source_until(original_index)?;
        if modified_index != self.output_index {
            return Err(patch_error(
                "hunk modified range does not match its original range",
            ));
        }
        for line in &hunk.lines {
            self.apply_line(line)?;
        }

        Ok(())
    }

    fn finish(mut self) -> Result<String, WriteError> {
        if self.new_side_terminated && self.source_lines.next().is_some() {
            return Err(patch_error(
                "newline marker must terminate its affected file side",
            ));
        }
        for source_line in self.source_lines {
            self.output.push_str(source_line);
        }

        Ok(self.output)
    }

    fn validate_original_range(
        &mut self,
        original_index: usize,
        old_count: usize,
    ) -> Result<(), WriteError> {
        if self
            .previous_original_end
            .is_some_and(|end| original_index < end)
        {
            return Err(patch_error(
                "hunks must use ascending, non-overlapping original ranges",
            ));
        }
        self.previous_original_end = Some(
            original_index
                .checked_add(old_count)
                .ok_or_else(|| patch_error("hunk original range overflowed"))?,
        );

        Ok(())
    }

    fn copy_source_until(&mut self, original_index: usize) -> Result<(), WriteError> {
        while self.source_index < original_index {
            if self.new_side_terminated {
                return Err(patch_error(
                    "newline marker must terminate its affected file side",
                ));
            }
            let source_line = self
                .source_lines
                .next()
                .ok_or_else(|| patch_error("hunk starts beyond the end of the target"))?;
            self.output.push_str(source_line);
            self.output_index += 1;
            self.source_index += 1;
        }

        Ok(())
    }

    fn apply_line(&mut self, line: &PatchLine) -> Result<(), WriteError> {
        match line.kind {
            PatchLineKind::Addition => {
                push_text_line(&mut self.output, &line.text);
                self.output_index += 1;
                self.new_side_terminated = !line.text.newline;
            }
            PatchLineKind::Context => {
                let source_line = self.next_matching_source_line(
                    &line.text,
                    "hunk context does not match the target",
                )?;
                self.output.push_str(source_line);
                self.output_index += 1;
                self.source_index += 1;
                self.new_side_terminated = !line.text.newline;
            }
            PatchLineKind::Removal => {
                self.next_matching_source_line(
                    &line.text,
                    "hunk removal does not match the target",
                )?;
                self.source_index += 1;
            }
        }

        Ok(())
    }

    fn next_matching_source_line(
        &mut self,
        expected: &TextLine,
        mismatch_error: &'static str,
    ) -> Result<&'a str, WriteError> {
        let source_line = self
            .source_lines
            .next()
            .ok_or_else(|| patch_error(mismatch_error))?;
        if !source_line_matches(source_line, expected) {
            return Err(patch_error(mismatch_error));
        }

        Ok(source_line)
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
        let header = strip_patch_newline(header);
        let (old_start, old_count, new_start, new_count) = parse_hunk_header(header)?;
        let hunk_lines = parse_hunk_lines(&mut lines, &mut termination)?;
        validate_hunk_line_counts(&hunk_lines, old_count, new_count)?;
        hunks.push(Hunk {
            lines: hunk_lines,
            new_count,
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

fn validate_hunk_line_counts(
    hunk_lines: &[PatchLine],
    old_count: usize,
    new_count: usize,
) -> Result<(), WriteError> {
    let actual_old = hunk_lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Addition)
        .count();
    let actual_new = hunk_lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Removal)
        .count();
    if actual_old != old_count || actual_new != new_count {
        return Err(patch_error("hunk line counts do not match its header"));
    }

    Ok(())
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

fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize), WriteError> {
    let ranges = header
        .strip_prefix("@@ -")
        .and_then(|header| header.split_once(" @@").map(|(ranges, _)| ranges))
        .ok_or_else(|| patch_error("invalid unified diff hunk header"))?;
    let (old_range, new_range) = ranges
        .split_once(" +")
        .ok_or_else(|| patch_error("invalid unified diff hunk ranges"))?;
    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Ok((old_start, old_count, new_start, new_count))
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

fn apply_hunks(current: &str, hunks: &[Hunk]) -> Result<String, WriteError> {
    let mut applier = HunkApplier::new(current);
    for hunk in hunks {
        applier.apply_hunk(hunk)?;
    }

    applier.finish()
}

fn hunk_start(start: usize, count: usize, side: &str) -> Result<usize, WriteError> {
    if count == 0 {
        return Ok(start);
    }

    start
        .checked_sub(1)
        .ok_or_else(|| patch_error(format!("non-empty hunk cannot start at {side} line zero")))
}

fn source_line_matches(source: &str, expected: &TextLine) -> bool {
    source.ends_with('\n') == expected.newline
        && source.strip_suffix('\n').unwrap_or(source) == expected.content
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

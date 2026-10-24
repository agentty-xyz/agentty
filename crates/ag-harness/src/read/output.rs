use std::fmt::Write as _;
use std::{fmt, io};

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

use super::runtime::ReadTool;
use crate::schema_contract;
use crate::tool::MAX_TOOL_RESULT_BYTES;

/// Bounded text returned by one successful `read` execution.
///
/// Its tool result names the shown line window before the content and,
/// when more of the file follows, how many lines the file has and the exact
/// `offset` and `limit` that fetch the next page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ReadOutput {
    pub(super) comparison_base: Option<String>,
    pub(super) content: String,
    pub(super) end_line: Option<u64>,
    /// Byte length of the only shown line when the content is just its
    /// first bytes, because the whole line did not fit.
    pub(super) line_bytes: Option<usize>,
    pub(super) next_offset: Option<u64>,
    pub(super) path: String,
    pub(super) start_line: u64,
    pub(super) total_lines: Option<u64>,
    pub(super) truncated: bool,
}

impl ReadOutput {
    /// Returns the selected text with line endings normalized to `\n`.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the final included one-based line, or `None` for empty output.
    pub fn end_line(&self) -> Option<u64> {
        self.end_line
    }

    /// Returns the next one-based line to request when output was truncated.
    pub fn next_offset(&self) -> Option<u64> {
        self.next_offset
    }

    /// Returns the repository-relative path that was read.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the requested one-based starting line.
    pub fn start_line(&self) -> u64 {
        self.start_line
    }

    /// Returns how many lines the file has, or `None` when counting them
    /// would exceed the read's scan limit.
    pub fn total_lines(&self) -> Option<u64> {
        self.total_lines
    }

    /// Returns whether additional file content follows this result.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Encodes this output within [`MAX_TOOL_RESULT_BYTES`] and as a result
    /// `fits` accepts, with the output that result shows: this output whole,
    /// else its longest run of leading whole lines, else the first bytes of
    /// its first line. A cut result says which lines it shows and where to
    /// continue.
    ///
    /// # Errors
    /// Returns [`ReadError::LineTooLong`] when not even the first byte of the
    /// first line fits.
    pub(crate) fn to_tool_result_within(
        &self,
        fits: ResultFits<'_>,
    ) -> Result<(String, Self), ReadError> {
        let accepts = |encoded: &str| encoded.len() <= MAX_TOOL_RESULT_BYTES && fits(encoded);
        let encoded = serde_json::to_string(self)?;
        if accepts(&encoded) {
            return Ok((encoded, self.clone()));
        }

        let line_ends = self
            .content
            .match_indices('\n')
            .map(|(index, _)| index)
            .chain((!self.content.is_empty()).then_some(self.content.len()))
            .collect::<Vec<_>>();
        // Every line together already failed, with less metadata than a cut.
        let fitting_lines = Self::longest_fitting(line_ends.len().saturating_sub(1), |lines| {
            serde_json::to_string(&self.with_line_prefix(&line_ends, lines))
                .is_ok_and(|candidate| accepts(&candidate))
        });
        let page = if fitting_lines > 0 {
            self.with_line_prefix(&line_ends, fitting_lines)
        } else {
            let first_line = &self.content[..line_ends.first().copied().unwrap_or(0)];
            let boundaries = first_line
                .char_indices()
                .map(|(index, _)| index)
                .skip(1)
                .chain((!first_line.is_empty()).then_some(first_line.len()))
                .collect::<Vec<_>>();
            let fitting_bytes = Self::longest_fitting(boundaries.len(), |count| {
                serde_json::to_string(&self.with_line_part(&line_ends, boundaries[count - 1]))
                    .is_ok_and(|candidate| accepts(&candidate))
            });
            if fitting_bytes == 0 {
                return Err(ReadError::LineTooLong {
                    line: self.start_line,
                    path: self.path.clone(),
                });
            }

            self.with_line_part(&line_ends, boundaries[fitting_bytes - 1])
        };

        Ok((serde_json::to_string(&page)?, page))
    }

    /// Returns the largest count in `0..=most` that `fits`, which holds for
    /// every count below one that fits.
    fn longest_fitting(most: usize, fits: impl Fn(usize) -> bool) -> usize {
        let (mut fitting, mut candidate) = (0_usize, most);
        while fitting < candidate {
            let midpoint = fitting + (candidate - fitting).div_ceil(2);
            if fits(midpoint) {
                fitting = midpoint;
            } else {
                candidate = midpoint - 1;
            }
        }

        fitting
    }

    /// Returns this output cut to its first `lines` whole lines.
    fn with_line_prefix(&self, line_ends: &[usize], lines: usize) -> Self {
        let content_end = line_ends[lines - 1];
        let lines = u64::try_from(lines).unwrap_or(u64::MAX);
        let end_line = self.start_line.checked_add(lines.saturating_sub(1));
        let next_offset = self.start_line.checked_add(lines);

        Self {
            comparison_base: self.comparison_base.clone(),
            content: self.content[..content_end].to_string(),
            end_line,
            line_bytes: None,
            next_offset,
            path: self.path.clone(),
            start_line: self.start_line,
            total_lines: self.total_lines,
            truncated: true,
        }
    }

    /// Returns this output cut to the first `bytes` of its first line.
    fn with_line_part(&self, line_ends: &[usize], bytes: usize) -> Self {
        let line_bytes = self
            .line_bytes
            .unwrap_or_else(|| line_ends.first().copied().unwrap_or(0));
        let more_lines = line_ends.len() > 1 || self.next_offset.is_some();

        Self {
            comparison_base: self.comparison_base.clone(),
            content: self.content[..bytes].to_string(),
            end_line: Some(self.start_line),
            line_bytes: Some(line_bytes),
            next_offset: more_lines.then(|| self.start_line.saturating_add(1)),
            path: self.path.clone(),
            start_line: self.start_line,
            total_lines: self.total_lines,
            truncated: true,
        }
    }

    /// Describes what a truncated result leaves out and how to fetch it;
    /// `None` for a whole result.
    fn continuation(&self) -> Option<String> {
        if !self.truncated {
            return None;
        }
        let of_total = self
            .total_lines
            .map_or_else(String::new, |total| format!(" of {total}"));
        if let Some(line_bytes) = self.line_bytes {
            let mut note = format!(
                "Showing only the first {} of {line_bytes} bytes of line {}{of_total}, which does \
                 not fit whole; read cannot return the rest of that line.",
                self.content.len(),
                self.start_line,
            );
            if let Some(next_offset) = self.next_offset {
                let _ = write!(note, " Use offset={next_offset} to continue after it.");
            }

            return Some(note);
        }
        let (end_line, next_offset) = self.end_line.zip(self.next_offset)?;
        let shown = end_line.saturating_sub(self.start_line).saturating_add(1);
        let unknown_total = if self.total_lines.is_none() {
            " More lines follow."
        } else {
            ""
        };

        Some(format!(
            "Showing lines {}-{end_line}{of_total}.{unknown_total} Use offset={next_offset} \
             limit={shown} to continue.",
            self.start_line
        ))
    }
}

impl Serialize for ReadOutput {
    /// Writes the line window and any continuation before the content, so
    /// the model sees what a result holds before reading it.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct ReadResult<'a> {
            path: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            comparison_base: Option<&'a str>,
            start_line: u64,
            end_line: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            total_lines: Option<u64>,
            truncated: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            next_offset: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            line_bytes: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            continuation: Option<String>,
            content: &'a str,
        }

        ReadResult {
            path: &self.path,
            comparison_base: self.comparison_base.as_deref(),
            start_line: self.start_line,
            end_line: self.end_line,
            total_lines: self.total_lines,
            truncated: self.truncated,
            next_offset: self.next_offset,
            line_bytes: self.line_bytes,
            continuation: self.continuation(),
            content: &self.content,
        }
        .serialize(serializer)
    }
}

/// Failure while safely executing one repository-relative read.
#[derive(Debug, Error)]
pub enum ReadError {
    /// The repository root could not be resolved.
    #[error("failed to resolve repository root: {source}")]
    RepositoryRoot {
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested path could not be resolved.
    #[error("failed to resolve read path `{path}`: {source}")]
    ResolvePath {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The canonical requested path escapes the canonical repository root.
    #[error("read path `{path}` resolves outside the repository")]
    OutsideRepository {
        /// Repository-relative requested path.
        path: String,
    },
    /// The requested file could not be opened.
    #[error("failed to open read path `{path}`: {source}")]
    Open {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested file could not be read.
    #[error("failed to read `{path}`: {source}")]
    Read {
        /// Repository-relative requested path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The requested line does not exist.
    #[error("read offset {offset} is beyond the end of `{path}`")]
    OffsetBeyondEnd {
        /// Requested one-based line.
        offset: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// A line cannot fit in the bounded tool result.
    #[error("line {line} in `{path}` exceeds the read size limit")]
    LineTooLong {
        /// One-based line whose content exceeded the cap.
        line: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// File content is not valid UTF-8 text.
    #[error("line {line} in `{path}` is not valid UTF-8")]
    InvalidUtf8 {
        /// One-based invalid line.
        line: u64,
        /// Repository-relative requested path.
        path: String,
    },
    /// The read consumed its bounded file-scan allowance.
    #[error("read of `{path}` exceeds the scan limit of {limit} bytes")]
    ScanLimitExceeded {
        /// Maximum file bytes one read may consume.
        limit: usize,
        /// Repository-relative requested path.
        path: String,
    },
    /// The successful result could not be encoded for the model.
    #[error("failed to encode read result: {0}")]
    Encode(#[from] serde_json::Error),
}

impl ReadError {
    pub(crate) fn is_model_correctable(&self) -> bool {
        !matches!(self, Self::RepositoryRoot { .. } | Self::Encode(_))
    }

    pub(crate) fn to_tool_result(&self, path: &str) -> Result<String, serde_json::Error> {
        rejected_tool_result(self, path)
    }
}

/// Decides whether one rendered `read` result may enter the request; a
/// result is cut until it does.
pub(crate) type ResultFits<'a> = &'a (dyn Fn(&str) -> bool + Sync);

#[derive(Debug, Error)]
pub(crate) enum InspectionError {
    #[error("repository inspection failed: {source}")]
    RepositoryCommand {
        #[source]
        source: io::Error,
    },
    #[error("repository inspection was rejected: {detail}")]
    RepositoryCommandRejected { detail: String },
    #[error("repository inspection returned non-UTF-8 output")]
    InvalidUtf8,
    #[error(
        "path `{path}` matches no repository file; it may not exist. List its parent directory to \
         see what exists"
    )]
    PathMatchesNothing { path: String },
    #[error(transparent)]
    Read(#[from] ReadError),
}

impl InspectionError {
    pub(crate) fn is_model_correctable(&self) -> bool {
        match self {
            Self::RepositoryCommand { .. } => false,
            Self::Read(error) => error.is_model_correctable(),
            Self::RepositoryCommandRejected { .. }
            | Self::InvalidUtf8
            | Self::PathMatchesNothing { .. } => true,
        }
    }

    pub(crate) fn to_tool_result(&self, path: &str) -> Result<String, serde_json::Error> {
        rejected_tool_result(self, path)
    }

    pub(crate) fn into_read_error(self, path: String) -> ReadError {
        match self {
            Self::RepositoryCommand { source } => ReadError::Read { path, source },
            Self::RepositoryCommandRejected { detail } => ReadError::Open {
                path,
                source: io::Error::other(detail),
            },
            Self::InvalidUtf8 => ReadError::InvalidUtf8 { line: 1, path },
            Self::PathMatchesNothing { .. } => ReadError::Open {
                source: io::Error::new(io::ErrorKind::NotFound, self.to_string()),
                path,
            },
            Self::Read(error) => error,
        }
    }
}

/// Returns a recorded `read` result cut to the longest form `fits` accepts,
/// by the same rules that bounded it when it ran: a file or revision read
/// to leading whole lines with its continuation, and a listing, search, or
/// diff to leading whole items or lines marked truncated. Returns `None` for
/// any other result, such as a rejection, or when no form fits.
pub(crate) fn fit_tool_result(content: &str, fits: ResultFits<'_>) -> Option<String> {
    #[derive(Deserialize)]
    struct InspectionResult {
        action: String,
        comparison_base: Option<String>,
        result: InspectionContent,
        truncated: bool,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum InspectionContent {
        Items(Vec<String>),
        Text(String),
    }

    if let Ok(output) = serde_json::from_str::<ReadOutput>(content) {
        return output
            .to_tool_result_within(fits)
            .ok()
            .map(|(result, _)| result);
    }
    let inspection = serde_json::from_str::<InspectionResult>(content).ok()?;
    let result = match inspection.result {
        InspectionContent::Items(items) => {
            ReadTool::bounded_items_result(&inspection.action, &items, inspection.truncated, fits)
        }
        InspectionContent::Text(text) => ReadTool::bounded_text_result(
            &inspection.action,
            &text,
            inspection.truncated,
            inspection.comparison_base.as_deref(),
            fits,
        )
        .ok()?,
    };

    fits(&result).then_some(result)
}

pub(crate) fn invalid_arguments_tool_result(
    error: &str,
    path: &str,
) -> Result<String, serde_json::Error> {
    rejected_tool_result(error, path)
}

fn rejected_tool_result(error: impl fmt::Display, path: &str) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct RejectedRead<'a> {
        error: String,
        path: &'a str,
        status: &'static str,
    }

    serde_json::to_string(&RejectedRead {
        error: schema_contract::bounded_diagnostic(error),
        path,
        status: "rejected",
    })
}

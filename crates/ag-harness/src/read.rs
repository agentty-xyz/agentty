use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader};

use crate::file_system::FileSystem;
use crate::tool::ReadArguments;

const MAX_READ_BYTES: usize = 50 * 1024;
const MAX_READ_LINES: u64 = 2_000;
const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;

/// Bounded text returned by one successful `read` execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadOutput {
    content: String,
    end_line: Option<u64>,
    next_offset: Option<u64>,
    path: String,
    start_line: u64,
    truncated: bool,
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

    /// Returns whether additional file content follows this result.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn to_tool_result(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
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

pub(crate) struct ReadTool {
    file_system: Arc<dyn FileSystem>,
    repository_root: PathBuf,
}

impl ReadTool {
    pub(crate) fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self {
            file_system,
            repository_root,
        }
    }

    pub(crate) async fn execute(&self, arguments: &ReadArguments) -> Result<ReadOutput, ReadError> {
        let root = self
            .file_system
            .canonicalize(&self.repository_root)
            .await
            .map_err(|source| ReadError::RepositoryRoot { source })?;
        let path = arguments.path().to_string();
        let candidate = root.join(Path::new(&path));
        let canonical_path = self
            .file_system
            .canonicalize(&candidate)
            .await
            .map_err(|source| ReadError::ResolvePath {
                path: path.clone(),
                source,
            })?;
        if !canonical_path.starts_with(&root) || canonical_path == root {
            return Err(ReadError::OutsideRepository { path });
        }
        let file = self
            .file_system
            .open_beneath(&root, Path::new(&path))
            .await
            .map_err(|source| ReadError::Open {
                path: path.clone(),
                source,
            })?;

        Self::read(file, arguments, path).await
    }

    async fn read(
        file: Box<dyn AsyncRead + Send + Unpin>,
        arguments: &ReadArguments,
        path: String,
    ) -> Result<ReadOutput, ReadError> {
        let start_line = arguments.offset().unwrap_or(1);
        let requested_lines = arguments.limit().unwrap_or(MAX_READ_LINES);
        let selected_lines = requested_lines.min(MAX_READ_LINES);
        let file: Box<dyn AsyncRead + Send + Unpin> =
            Box::new(file.take((MAX_SCAN_BYTES + 1) as u64));
        let mut reader = BufReader::new(file);
        let mut current_line = 1_u64;
        let mut remaining_scan_bytes = MAX_SCAN_BYTES;

        while current_line < start_line {
            if !Self::skip_line(&mut reader, &path, &mut remaining_scan_bytes).await? {
                return Err(ReadError::OffsetBeyondEnd {
                    offset: start_line,
                    path,
                });
            }
            current_line += 1;
        }

        let mut content = String::new();
        let mut lines_read = 0_u64;
        let mut next_offset = None;
        while lines_read < selected_lines {
            let Some(line) =
                Self::next_line(&mut reader, current_line, &path, &mut remaining_scan_bytes)
                    .await?
            else {
                break;
            };
            let line = Self::decode_line(line, current_line, &path)?;
            let separator_bytes = usize::from(lines_read > 0);
            if content
                .len()
                .checked_add(separator_bytes)
                .and_then(|bytes| bytes.checked_add(line.len()))
                .is_none_or(|bytes| bytes > MAX_READ_BYTES)
            {
                next_offset = Some(current_line);
                break;
            }
            if separator_bytes > 0 {
                content.push('\n');
            }
            content.push_str(&line);
            lines_read += 1;
            current_line += 1;
        }

        if next_offset.is_none()
            && lines_read == selected_lines
            && Self::has_more(&mut reader, &path).await?
        {
            next_offset = Some(current_line);
        }
        if lines_read == 0 && start_line > 1 {
            return Err(ReadError::OffsetBeyondEnd {
                offset: start_line,
                path,
            });
        }
        let end_line = lines_read
            .checked_sub(1)
            .and_then(|additional_lines| start_line.checked_add(additional_lines));

        Ok(ReadOutput {
            content,
            end_line,
            next_offset,
            path,
            start_line,
            truncated: next_offset.is_some(),
        })
    }

    async fn next_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        line: u64,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<Option<Vec<u8>>, ReadError> {
        let mut bytes = Vec::new();
        let mut limited = (&mut *reader).take((MAX_READ_BYTES + 3) as u64);
        let bytes_read = limited
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })?;
        if bytes_read == 0 {
            return Ok(None);
        }
        Self::consume_scan_budget(remaining_scan_bytes, bytes.len(), path)?;
        let line_content_bytes = if let Some(line) = bytes.strip_suffix(b"\n") {
            line.strip_suffix(b"\r").unwrap_or(line)
        } else {
            &bytes
        };
        if line_content_bytes.len() > MAX_READ_BYTES {
            return Err(ReadError::LineTooLong {
                line,
                path: path.to_string(),
            });
        }

        Ok(Some(bytes))
    }

    async fn skip_line(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
        remaining_scan_bytes: &mut usize,
    ) -> Result<bool, ReadError> {
        let mut saw_bytes = false;
        loop {
            let (bytes_to_consume, reached_newline) = {
                let bytes = reader.fill_buf().await.map_err(|source| ReadError::Read {
                    path: path.to_string(),
                    source,
                })?;
                if bytes.is_empty() {
                    return Ok(saw_bytes);
                }
                saw_bytes = true;

                bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or((bytes.len(), false), |index| (index + 1, true))
            };
            Self::consume_scan_budget(remaining_scan_bytes, bytes_to_consume, path)?;
            reader.consume(bytes_to_consume);
            if reached_newline {
                return Ok(true);
            }
        }
    }

    fn consume_scan_budget(
        remaining_scan_bytes: &mut usize,
        bytes: usize,
        path: &str,
    ) -> Result<(), ReadError> {
        if bytes > *remaining_scan_bytes {
            return Err(ReadError::ScanLimitExceeded {
                limit: MAX_SCAN_BYTES,
                path: path.to_string(),
            });
        }
        *remaining_scan_bytes -= bytes;

        Ok(())
    }

    async fn has_more(
        reader: &mut BufReader<Box<dyn AsyncRead + Send + Unpin>>,
        path: &str,
    ) -> Result<bool, ReadError> {
        reader
            .fill_buf()
            .await
            .map(|bytes| !bytes.is_empty())
            .map_err(|source| ReadError::Read {
                path: path.to_string(),
                source,
            })
    }

    fn decode_line(mut line: Vec<u8>, line_number: u64, path: &str) -> Result<String, ReadError> {
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }

        String::from_utf8(line).map_err(|_| ReadError::InvalidUtf8 {
            line: line_number,
            path: path.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use mockall::Sequence;
    use tokio::io::ReadBuf;

    use super::*;
    use crate::file_system::MockFileSystem;

    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("broken stream")))
        }
    }

    struct ContentThenFailReader {
        content: Option<Vec<u8>>,
    }

    impl AsyncRead for ContentThenFailReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let Some(content) = self.content.take() else {
                return Poll::Ready(Err(io::Error::other("broken continuation probe")));
            };
            buffer.put_slice(&content);

            Poll::Ready(Ok(()))
        }
    }

    fn arguments(value: serde_json::Value) -> ReadArguments {
        serde_json::from_value(value).expect("read arguments should be valid")
    }

    fn file_system(content: impl Into<Vec<u8>>) -> Arc<MockFileSystem> {
        file_system_reader(Box::new(Cursor::new(content.into())))
    }

    fn file_system_reader(reader: Box<dyn AsyncRead + Send + Unpin>) -> Arc<MockFileSystem> {
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .withf(|path| path == Path::new("repo"))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .withf(|path| path == Path::new("/repo/input.txt"))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
        file_system
            .expect_open_beneath()
            .withf(|root, path| root == Path::new("/repo") && path == Path::new("input.txt"))
            .times(1)
            .return_once(move |_, _| Ok(reader));

        Arc::new(file_system)
    }

    #[tokio::test]
    async fn reads_requested_lines_and_reports_continuation() {
        // Arrange
        let tool = ReadTool::new(file_system("one\r\ntwo\nthree\nfour\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2,
            "limit": 2
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("bounded read should succeed");

        // Assert
        assert_eq!(output.content(), "two\nthree");
        assert_eq!(output.path(), "input.txt");
        assert_eq!(output.start_line(), 2);
        assert_eq!(output.end_line(), Some(3));
        assert_eq!(output.next_offset(), Some(4));
        assert!(output.truncated());
        assert_eq!(
            output.to_tool_result().expect("output should serialize"),
            r#"{"content":"two\nthree","end_line":3,"next_offset":4,"path":"input.txt","start_line":2,"truncated":true}"#
        );
    }

    #[tokio::test]
    async fn reads_empty_file_without_truncation() {
        // Arrange
        let tool = ReadTool::new(file_system(Vec::new()), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("empty file should be readable");

        // Assert
        assert_eq!(output.content(), "");
        assert_eq!(output.end_line(), None);
        assert_eq!(output.next_offset(), None);
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn preserves_leading_and_consecutive_blank_lines() {
        // Arrange
        let tool = ReadTool::new(file_system("\n\nvalue\n\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 4
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("blank lines should be preserved");

        // Assert
        assert_eq!(output.content(), "\n\nvalue\n");
        assert_eq!(output.start_line(), 1);
        assert_eq!(output.end_line(), Some(4));
        assert_eq!(output.next_offset(), None);
    }

    #[tokio::test]
    async fn reads_to_exact_end_without_truncation() {
        // Arrange
        let tool = ReadTool::new(file_system("one\ntwo"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 2
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("complete bounded read should succeed");

        // Assert
        assert_eq!(output.content(), "one\ntwo");
        assert_eq!(output.end_line(), Some(2));
        assert_eq!(output.next_offset(), None);
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn caps_requested_line_count() {
        // Arrange
        let line_count =
            usize::try_from(MAX_READ_LINES + 1).expect("read line limit should fit the platform");
        let content = "line\n".repeat(line_count);
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": u64::MAX
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("line-bounded read should succeed");

        // Assert
        assert_eq!(output.end_line(), Some(MAX_READ_LINES));
        assert_eq!(output.next_offset(), Some(MAX_READ_LINES + 1));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn bounds_output_by_bytes() {
        // Arrange
        let first_line = "a".repeat(MAX_READ_BYTES - 1);
        let content = format!("{first_line}\nsecond\n");
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("byte-bounded read should succeed");

        // Assert
        assert_eq!(output.content(), first_line);
        assert_eq!(output.next_offset(), Some(2));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn accepts_exact_byte_limit_before_lf() {
        // Arrange
        let expected = "x".repeat(MAX_READ_BYTES);
        let tool = ReadTool::new(file_system(format!("{expected}\n")), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("line at the normalized byte limit should succeed");

        // Assert
        assert_eq!(output.content(), expected);
        assert_eq!(output.end_line(), Some(1));
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn accepts_exact_byte_limit_before_crlf() {
        // Arrange
        let expected = "x".repeat(MAX_READ_BYTES);
        let tool = ReadTool::new(file_system(format!("{expected}\r\n")), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("CRLF line at the normalized byte limit should succeed");

        // Assert
        assert_eq!(output.content(), expected);
        assert_eq!(output.end_line(), Some(1));
        assert!(!output.truncated());
    }

    #[tokio::test]
    async fn does_not_validate_unrequested_oversized_line() {
        // Arrange
        let content = format!("one\n{}", "x".repeat(MAX_READ_BYTES + 1));
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("unrequested line should only be probed for presence");

        // Assert
        assert_eq!(output.content(), "one");
        assert_eq!(output.end_line(), Some(1));
        assert_eq!(output.next_offset(), Some(2));
        assert!(output.truncated());
    }

    #[tokio::test]
    async fn skips_unrequested_oversized_prefix_line() {
        // Arrange
        let content = format!("{}\nvalue\n", "x".repeat(MAX_READ_BYTES + 1));
        let tool = ReadTool::new(file_system(content), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2,
            "limit": 1
        }));

        // Act
        let output = tool
            .execute(&arguments)
            .await
            .expect("unrequested prefix line should be discarded");

        // Assert
        assert_eq!(output.content(), "value");
        assert_eq!(output.start_line(), 2);
        assert_eq!(output.end_line(), Some(2));
        assert_eq!(output.next_offset(), None);
    }

    #[tokio::test]
    async fn rejects_reads_that_exceed_scan_budget() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![b'x'; MAX_SCAN_BYTES + 1]), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("prefix scan beyond the byte budget should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::ScanLimitExceeded { limit, path }
                if limit == MAX_SCAN_BYTES && path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn reports_continuation_probe_failure() {
        // Arrange
        let reader = ContentThenFailReader {
            content: Some(b"one\n".to_vec()),
        };
        let tool = ReadTool::new(file_system_reader(Box::new(reader)), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "limit": 1
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("failed continuation probe should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }

    #[tokio::test]
    async fn reports_failure_while_skipping_prefix() {
        // Arrange
        let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("failed prefix discard should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }

    #[tokio::test]
    async fn rejects_offset_beyond_end() {
        // Arrange
        let tool = ReadTool::new(file_system("one\n"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 3
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("out-of-range offset should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OffsetBeyondEnd { offset: 3, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_offset_after_unterminated_final_line() {
        // Arrange
        let tool = ReadTool::new(file_system("one"), "repo".into());
        let arguments = arguments(serde_json::json!({
            "path": "input.txt",
            "offset": 2
        }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("offset after an unterminated final line should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OffsetBeyondEnd { offset: 2, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_line_without_unbounded_read() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![b'x'; MAX_READ_BYTES + 1]), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("oversized line should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::LineTooLong { line: 1, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_utf8() {
        // Arrange
        let tool = ReadTool::new(file_system(vec![0xff, b'\n']), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("invalid UTF-8 should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::InvalidUtf8 { line: 1, path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_path_that_resolves_outside_repository() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/outside/input.txt")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("escaping canonical path should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::OutsideRepository { path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn rejects_path_that_resolves_to_repository_root() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(2)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("repository directory should not be readable as a file");

        // Assert
        assert!(matches!(
            error,
            ReadError::OutsideRepository { path } if path == "input.txt"
        ));
    }

    #[tokio::test]
    async fn reports_path_resolution_failure() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing file")));
        file_system.expect_open_beneath().times(0);
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("missing file should fail path resolution");

        // Assert
        assert!(matches!(
            error,
            ReadError::ResolvePath { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::NotFound
        ));
    }

    #[tokio::test]
    async fn reports_file_open_failure() {
        // Arrange
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "permission denied",
                ))
            });
        let tool = ReadTool::new(Arc::new(file_system), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("unopenable file should fail");

        // Assert
        assert!(matches!(
            error,
            ReadError::Open { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[tokio::test]
    async fn reports_file_read_failure() {
        // Arrange
        let tool = ReadTool::new(file_system_reader(Box::new(FailingReader)), "repo".into());
        let arguments = arguments(serde_json::json!({ "path": "input.txt" }));

        // Act
        let error = tool
            .execute(&arguments)
            .await
            .expect_err("broken stream should fail the read");

        // Assert
        assert!(matches!(
            error,
            ReadError::Read { path, source }
                if path == "input.txt" && source.kind() == io::ErrorKind::Other
        ));
    }
}

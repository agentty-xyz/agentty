use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};

use super::next_event;
use crate::execution::contract::{ExecutionError, OutputStream, ProcessEvent};

struct FailedReader;

impl AsyncRead for FailedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("broken pipe")))
    }
}

#[tokio::test]
async fn drains_both_streams_in_bounded_chunks_and_reports_each_eof_once() {
    // Arrange
    let mut stdout = Some(b"output".as_slice());
    let mut stderr = Some(b"errors".as_slice());
    let mut buffer = [0; 4];
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut eof = Vec::new();

    // Act
    while stdout.is_some() || stderr.is_some() {
        match next_event(&mut stdout, &mut stderr, &mut buffer)
            .await
            .expect("event")
        {
            ProcessEvent::Output(stream, length) => {
                assert!(length <= buffer.len());
                match stream {
                    OutputStream::Stdout => output.extend_from_slice(&buffer[..length]),
                    OutputStream::Stderr => errors.extend_from_slice(&buffer[..length]),
                }
            }
            ProcessEvent::Eof(stream) => eof.push(stream),
            event => unreachable!("unexpected event: {event:?}"),
        }
    }

    // Assert
    assert_eq!(output, b"output");
    assert_eq!(errors, b"errors");
    assert_eq!(eof.len(), 2);
    assert!(
        eof.iter()
            .any(|stream| matches!(stream, OutputStream::Stdout))
    );
    assert!(
        eof.iter()
            .any(|stream| matches!(stream, OutputStream::Stderr))
    );
}

#[tokio::test]
async fn caps_reads_at_the_executor_chunk_limit() {
    // Arrange
    let bytes = [1; 8192];
    let mut stdout = Some(bytes.as_slice());
    let mut stderr: Option<&[u8]> = None;
    let mut buffer = [0; 8192];

    // Act
    let event = next_event(&mut stdout, &mut stderr, &mut buffer).await;

    // Assert
    assert!(matches!(
        event,
        Ok(ProcessEvent::Output(OutputStream::Stdout, 4096))
    ));
    assert_eq!(&buffer[..4096], &[1; 4096]);
    assert_eq!(&buffer[4096..], &[0; 4096]);
}

#[tokio::test]
async fn propagates_read_failures_without_closing_the_stream() {
    // Arrange
    let mut stdout = Some(FailedReader);
    let mut stderr: Option<&[u8]> = None;
    let mut buffer = [0; 16];

    // Act
    let result = next_event(&mut stdout, &mut stderr, &mut buffer).await;

    // Assert
    assert!(matches!(result, Err(ExecutionError::Process)));
    assert!(stdout.is_some());
}

#[tokio::test]
async fn cancelling_a_pending_read_preserves_subsequent_output() {
    // Arrange
    let (reader, mut writer) = tokio::io::duplex(16);
    let mut stdout = Some(reader);
    let mut stderr: Option<&[u8]> = None;
    let mut buffer = [0; 16];

    // Act
    tokio::select! {
        biased;
        _ = next_event(&mut stdout, &mut stderr, &mut buffer) => unreachable!("read must wait"),
        () = std::future::ready(()) => {}
    }
    writer.write_all(b"retained").await.expect("write");
    let event = next_event(&mut stdout, &mut stderr, &mut buffer).await;

    // Assert
    assert!(matches!(
        event,
        Ok(ProcessEvent::Output(OutputStream::Stdout, 8))
    ));
    assert_eq!(&buffer[..8], b"retained");
}

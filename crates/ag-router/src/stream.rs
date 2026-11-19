//! Streamed responses: the deltas a caller observes while a response
//! streams, the server-sent event reader both streamed wire protocols share,
//! and their shared transport.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;

use crate::http::{HttpTransport, SUCCESS_BODY_LIMIT_BYTES, TransportError};

/// Bound for one whole streamed response. Long reasoning at the highest
/// efforts can run for many minutes, so silence is bounded separately.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(30);
/// Longest silence tolerated on an open stream, matching Codex CLI's default
/// stream idle timeout.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_mins(5);

/// One fragment of a response's content, reported to a [`DeltaObserver`]
/// as it streams.
///
/// Fragments of one kind concatenate, in order, to the streamed content of
/// that kind. The completion the request returns stays authoritative: it is
/// decoded from the whole response exactly as an unstreamed one would be,
/// and a response that fails after streaming began returns its error.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResponseDelta {
    /// Assistant text: the answer, or the text written beside tool calls.
    Text(String),
    /// Provider reasoning text, such as a Responses reasoning summary or a
    /// Chat Completions `reasoning_content`.
    Reasoning(String),
    /// Arguments of one tool call, as JSON text that is incomplete until
    /// the call's last fragment.
    ToolArguments {
        /// Fragment of the call's arguments.
        arguments: String,
        /// Zero-based position of the call among the response's calls, in
        /// the order the calls started, which is their order in
        /// [`crate::Completion::ToolCalls`].
        index: usize,
    },
}

/// Observer told about each [`ResponseDelta`] of a streamed response as it
/// arrives, such as a host that renders a model's answer while it is
/// written.
///
/// Only the Responses and Chat Completions protocols stream; an Anthropic
/// Messages request reports no deltas.
#[async_trait]
pub trait DeltaObserver: Send + Sync {
    /// Reports `delta`; the stream is not read further until this call
    /// returns, and cancelling the request while it runs stops the call.
    ///
    /// The request's whole-response bound keeps running meanwhile, and the
    /// connection may fail as idle once reading resumes after a pause longer
    /// than the stream's idle timeout, so return promptly.
    async fn delta(&self, delta: ResponseDelta);
}

/// Production transport for streamed requests, built on first use and
/// shared by every router, so hosts that build a router per model do not
/// build an HTTP client per router.
static DEFAULT_TRANSPORT: LazyLock<Arc<HttpTransport>> = LazyLock::new(|| {
    Arc::new(HttpTransport::streaming(
        STREAM_IDLE_TIMEOUT,
        REQUEST_TIMEOUT,
    ))
});

/// Returns the shared production transport for streamed requests.
pub(crate) fn default_transport() -> Arc<HttpTransport> {
    Arc::clone(&DEFAULT_TRANSPORT)
}

/// Incremental reader of a server-sent event stream that returns the
/// `data` of each event. Each pending line and event stays within the
/// success body limit, so memory is bounded however long the stream runs.
#[derive(Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    data: String,
    events: Vec<String>,
}

impl SseDecoder {
    /// Reads `chunk`, returning the data of each event it completed, in
    /// order; events without data are skipped.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, TransportError> {
        self.buffer.extend_from_slice(chunk);
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                self.dispatch();
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
            if self.data.len() > SUCCESS_BODY_LIMIT_BYTES {
                return Err(TransportError::ResponseBodyTooLarge);
            }
        }
        if self.buffer.len() > SUCCESS_BODY_LIMIT_BYTES {
            return Err(TransportError::ResponseBodyTooLarge);
        }

        Ok(std::mem::take(&mut self.events))
    }

    /// Returns the data of the event left when the stream closed, if any.
    pub(crate) fn finish(mut self) -> Result<Option<String>, TransportError> {
        // A trailing line without its newline cannot complete an event, so
        // reading it only adds its data to the pending event.
        if !self.buffer.is_empty() {
            self.push(b"\n")?;
        }
        self.dispatch();

        Ok(self.events.pop())
    }

    fn dispatch(&mut self) {
        let data = std::mem::take(&mut self.data);
        if !data.is_empty() {
            self.events.push(data);
        }
    }
}

/// The stream closed before its response completed.
#[derive(Debug, thiserror::Error)]
#[error("the response stream closed before the response completed")]
pub(crate) struct StreamClosed;

#[cfg(test)]
#[path = "stream_test.rs"]
mod tests;

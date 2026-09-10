//! Provider diagnostics for requests that must shrink before retrying.

/// Reports whether a provider rejected the size of an input or context window.
///
/// These deterministic failures cannot be repaired by restarting a transport
/// with the same prompt. Accept wrapped diagnostics from one-shot clients too.
pub fn is_input_size_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    [
        "contextwindowexceeded",
        "context_window_exceeded",
        "context window exceeded",
        "input exceeds the maximum length",
        "maximum context length",
        "prompt is too long",
    ]
    .iter()
    .any(|signature| message.contains(signature))
}

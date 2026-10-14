//! Shared response-error declarations for router and harness boundaries.

/// Defines the common model-response failures while allowing each boundary to
/// retain its own additional variants and error classification methods.
///
/// Internal workspace plumbing; callers should use the concrete `ModelError`
/// types instead. Both invoking crates provide the `thiserror` derive.
#[doc(hidden)]
#[macro_export]
macro_rules! define_model_response_error {
    ($(#[$attribute:meta])* $visibility:vis enum $name:ident { $($extra:tt)* }) => {
        $(#[$attribute])*
        #[derive(Debug, thiserror::Error)]
        $visibility enum $name {
            /// Request construction or transport failed.
            #[error("model request failed: {0}")]
            Request(#[source] Box<dyn std::error::Error + Send + Sync>),
            /// No usable assistant response was returned.
            #[error("model returned no response content")]
            InvalidResponse,
            /// Provider response ended before completion.
            #[error("model response is incomplete: {reason}")]
            IncompleteResponse {
                /// Provider finish reason.
                reason: String,
            },
            /// Response body exceeded its bound.
            #[error("model response body exceeds the size limit")]
            ResponseBodyTooLarge,
            /// The provider cannot use the requested schema.
            #[error("provider cannot satisfy this output schema: {reason}")]
            UnsupportedOutputSchema {
                /// Provider limitation.
                reason: String,
            },
            /// The provider cannot use the requested image input.
            #[error("model does not support image input: {reason}")]
            UnsupportedImageInput {
                /// Model limitation.
                reason: String,
            },
            /// Response content exceeded its bound.
            #[error("model response content exceeds the size limit")]
            ResponseContentTooLarge,
            /// Provider returned invalid JSON.
            #[error("model returned invalid JSON: {reason}")]
            InvalidJson {
                /// Bounded parser diagnostic.
                reason: String,
            },
            /// Provider output did not match the requested schema.
            #[error("model output violates the schema at {path}: {reason}")]
            SchemaViolation {
                /// Path to the invalid value.
                path: String,
                /// Bounded validator diagnostic.
                reason: String,
            },
            /// The provider did not include a required function call.
            #[error("model returned no tool call")]
            MissingToolCall,
            /// A function call identifier was invalid.
            #[error("model returned a blank or oversized tool call identifier")]
            InvalidToolCallId,
            /// Function call identifiers were not unique.
            #[error("model returned duplicate tool call identifier: {id}")]
            DuplicateToolCallId {
                /// Duplicate identifier.
                id: String,
            },
            /// Terminal output also contained function calls.
            #[error("model terminal response contained tool calls")]
            TerminalResponseWithToolCalls,
            /// Provider returned a non-function tool call.
            #[error("model requested unsupported tool type: {kind}")]
            UnsupportedToolType {
                /// Unsupported wire type.
                kind: String,
            },
            /// Provider returned an unadvertised function.
            #[error("model requested unsupported tool: {name}")]
            UnsupportedToolName {
                /// Function name.
                name: String,
            },
            /// Function arguments were invalid.
            #[error("model returned invalid tool arguments: {reason}")]
            InvalidToolArguments {
                /// Bounded parser diagnostic.
                reason: String,
            },
            $($extra)*
        }
    };
}

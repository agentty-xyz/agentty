use crate::agent::response_parser::parse_gemini_response_with_fallback;
use crate::model::session::SessionDiffState;

#[test]
fn test_gemini_parse_response_reads_legacy_usage() {
    // Arrange
    let stdout = r#"{"response":"Planned response","stats":{"models":{"gemini":{"tokens":{"input":11,"candidates":7}}}}}"#;

    // Act
    let parsed = parse_gemini_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Planned response");
    assert_eq!(parsed.stats.input_tokens, 11);
    assert_eq!(parsed.stats.output_tokens, 7);
    assert_eq!(parsed.stats.diff_state, SessionDiffState::Unknown);
}

#[test]
fn test_gemini_parse_response_reads_stream_usage() {
    // Arrange
    let stdout = concat!(
        r#"{"type":"content","text":"Streamed response"}"#,
        "\n",
        r#"{"type":"result","stats":{"input_tokens":13,"output_tokens":5}}"#
    );

    // Act
    let parsed = parse_gemini_response_with_fallback(stdout, "");

    // Assert
    assert_eq!(parsed.content, "Streamed response");
    assert_eq!(parsed.stats.input_tokens, 13);
    assert_eq!(parsed.stats.output_tokens, 5);
    assert_eq!(parsed.stats.diff_state, SessionDiffState::Unknown);
}

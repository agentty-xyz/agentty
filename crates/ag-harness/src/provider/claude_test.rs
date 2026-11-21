//! Claude catalog tests for model context profiles and output limits.

use super::{
    CLAUDE_FABLE_5_2, CLAUDE_HAIKU_5_5, CLAUDE_OPUS_5_5, CLAUDE_SONNET_5_5, max_output_tokens,
    profile,
};

#[test]
fn profiles_keep_requests_inside_each_documented_window() {
    // Arrange
    let cataloged = [
        CLAUDE_FABLE_5_2,
        CLAUDE_OPUS_5_5,
        CLAUDE_SONNET_5_5,
        CLAUDE_HAIKU_5_5,
    ];

    // Act
    let unlisted = (
        profile("claude-unlisted"),
        max_output_tokens("claude-unlisted"),
    );

    // Assert
    for model in cataloged {
        let profile = profile(model).expect("current Claude models are cataloged");
        assert_eq!(profile.context_window_tokens(), 1_000_000, "{model}");
        assert_eq!(profile.input_token_budget(), 611_929, "{model}");
        assert_eq!(max_output_tokens(model), Some(128_000), "{model}");
    }
    assert_eq!(unlisted, (None, None));
}

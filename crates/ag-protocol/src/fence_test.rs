use crate::fence::diff_fence;

#[test]
/// Ensures the fence is at least three backticks and outgrows the longest
/// backtick run in the payload.
fn test_diff_fence_outgrows_longest_backtick_run() {
    // Arrange
    let cases = [
        ("", "```"),
        ("plain `code` text", "```"),
        ("before ```` after `` end", "`````"),
    ];

    for (content, expected_fence) in cases {
        // Act
        let fence = diff_fence(content);

        // Assert
        assert_eq!(fence, expected_fence, "{content:?}");
    }
}

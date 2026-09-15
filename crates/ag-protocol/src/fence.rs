/// Builds a Markdown code-fence delimiter long enough to safely wrap an
/// arbitrary prompt payload.
///
/// Returns a string of backticks whose length exceeds the longest run of
/// consecutive backticks found anywhere in `content`, with a minimum length
/// of three. This prevents a triple-backtick fence from being terminated
/// prematurely when the payload itself contains Markdown fences (for example,
/// when reviewing changes to Markdown or prompt-template files).
pub fn diff_fence(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current_run = 0usize;
    for character in content.chars() {
        if character == '`' {
            current_run += 1;
            if current_run > max_run {
                max_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }

    let fence_length = std::cmp::max(3, max_run + 1);

    "`".repeat(fence_length)
}

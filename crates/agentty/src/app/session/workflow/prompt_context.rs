//! Shared byte budgeting for JSON-encoded user context.

/// Returns the longest UTF-8 prefix whose JSON string fits `max_bytes`,
/// including quotes and escapes. Callers supply a budget of at least two bytes.
pub(super) fn json_prefix(value: &str, max_bytes: usize) -> &str {
    let mut start = 0;
    let mut end = value.len().min(max_bytes.saturating_sub(2));
    while start < end {
        let middle = start + (end - start).div_ceil(2);
        let prefix = &value[..value.floor_char_boundary(middle)];
        if serde_json::json!(prefix).to_string().len() <= max_bytes {
            start = middle;
        } else {
            end = middle - 1;
        }
    }

    &value[..value.floor_char_boundary(start)]
}

#[cfg(test)]
#[path = "prompt_context_test.rs"]
mod tests;

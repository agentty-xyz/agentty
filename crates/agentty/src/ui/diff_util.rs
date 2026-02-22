/// The kind of a line in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    FileHeader,
    HunkHeader,
    Context,
    Addition,
    Deletion,
}

/// A parsed line from a unified diff, with optional old/new line numbers.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: &'a str,
}

/// Extract `(old_start, old_count, new_start, new_count)` from a hunk header
/// like `@@ -10,5 +20,7 @@`.
pub fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let line = line.strip_prefix("@@ -")?;
    let at_idx = line.find(" @@")?;
    let range_part = &line[..at_idx];
    let mut parts = range_part.split(" +");
    let old_range = parts.next()?;
    let new_range = parts.next()?;

    let (old_start, old_count) = parse_range(old_range)?;
    let (new_start, new_count) = parse_range(new_range)?;

    Some((old_start, old_count, new_start, new_count))
}

/// Parse a full unified diff into structured [`DiffLine`] entries with line
/// numbers.
pub fn parse_diff_lines(diff: &str) -> Vec<DiffLine<'_>> {
    let mut result = Vec::new();
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            result.push(DiffLine {
                kind: DiffLineKind::FileHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if line.starts_with("@@") {
            if let Some((old_start, _, new_start, _)) = parse_hunk_header(line) {
                old_line = old_start;
                new_line = new_start;
            }
            result.push(DiffLine {
                kind: DiffLineKind::HunkHeader,
                old_line: None,
                new_line: None,
                content: line,
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            result.push(DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(new_line),
                content: rest,
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            result.push(DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(old_line),
                new_line: None,
                content: rest,
            });
            old_line += 1;
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line);
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content,
            });
            old_line += 1;
            new_line += 1;
        }
    }

    result
}

/// Find the maximum line number across all parsed diff lines for gutter width
/// calculation.
pub fn max_diff_line_number(lines: &[DiffLine<'_>]) -> u32 {
    lines
        .iter()
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .unwrap_or(0)
}

/// Split a diff content string into chunks that fit within `max_width`
/// characters. Returns at least one chunk (empty string if content is empty).
pub fn wrap_diff_content(content: &str, max_width: usize) -> Vec<&str> {
    if max_width == 0 {
        return vec![content];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_width {
            chunks.push(remaining);

            break;
        }

        let split_at = remaining
            .char_indices()
            .nth(max_width)
            .map_or(remaining.len(), |(idx, _)| idx);
        chunks.push(&remaining[..split_at]);
        remaining = &remaining[split_at..];
    }

    if chunks.is_empty() {
        chunks.push("");
    }

    chunks
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    if let Some((start, count)) = range.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((range.parse().ok()?, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hunk_header_basic() {
        // Arrange
        let line = "@@ -10,5 +20,7 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((10, 5, 20, 7)));
    }

    #[test]
    fn test_parse_hunk_header_no_count() {
        // Arrange
        let line = "@@ -1 +1 @@";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context() {
        // Arrange
        let line = "@@ -100,3 +200,4 @@ fn main() {";

        // Act
        let result = parse_hunk_header(line);

        // Assert
        assert_eq!(result, Some((100, 3, 200, 4)));
    }

    #[test]
    fn test_parse_hunk_header_invalid() {
        // Arrange & Act & Assert
        assert_eq!(parse_hunk_header("not a hunk"), None);
        assert_eq!(parse_hunk_header("@@@ invalid @@@"), None);
    }

    #[test]
    fn test_parse_diff_lines_full() {
        // Arrange
        let diff = "\
diff --git a/file.rs b/file.rs
index abc..def 100644
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,4 @@
 line1
+added
 line2
-removed";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 9);

        assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[0].content, "diff --git a/file.rs b/file.rs");
        assert_eq!(lines[0].old_line, None);

        assert_eq!(lines[4].kind, DiffLineKind::HunkHeader);
        assert_eq!(lines[4].old_line, None);

        assert_eq!(lines[5].kind, DiffLineKind::Context);
        assert_eq!(lines[5].content, "line1");
        assert_eq!(lines[5].old_line, Some(1));
        assert_eq!(lines[5].new_line, Some(1));

        assert_eq!(lines[6].kind, DiffLineKind::Addition);
        assert_eq!(lines[6].content, "added");
        assert_eq!(lines[6].old_line, None);
        assert_eq!(lines[6].new_line, Some(2));

        assert_eq!(lines[7].kind, DiffLineKind::Context);
        assert_eq!(lines[7].content, "line2");
        assert_eq!(lines[7].old_line, Some(2));
        assert_eq!(lines[7].new_line, Some(3));

        assert_eq!(lines[8].kind, DiffLineKind::Deletion);
        assert_eq!(lines[8].content, "removed");
        assert_eq!(lines[8].old_line, Some(3));
        assert_eq!(lines[8].new_line, None);
    }

    #[test]
    fn test_parse_diff_lines_empty() {
        // Arrange
        let diff = "";

        // Act
        let lines = parse_diff_lines(diff);

        // Assert
        assert_eq!(lines.len(), 0);
    }

    #[test]
    fn test_max_diff_line_number() {
        // Arrange
        let diff = "\
@@ -95,3 +100,4 @@
 context
+added
 context2
-removed";
        let lines = parse_diff_lines(diff);

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 102);
    }

    #[test]
    fn test_max_diff_line_number_empty() {
        // Arrange
        let lines: Vec<DiffLine<'_>> = Vec::new();

        // Act
        let max_num = max_diff_line_number(&lines);

        // Assert
        assert_eq!(max_num, 0);
    }

    #[test]
    fn test_wrap_diff_content_fits() {
        // Arrange
        let content = "short line";

        // Act
        let chunks = wrap_diff_content(content, 80);

        // Assert
        assert_eq!(chunks, vec!["short line"]);
    }

    #[test]
    fn test_wrap_diff_content_wraps() {
        // Arrange
        let content = "abcdefghij";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn test_wrap_diff_content_empty() {
        // Arrange & Act
        let chunks = wrap_diff_content("", 10);

        // Assert
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn test_wrap_diff_content_exact() {
        // Arrange
        let content = "abcd";

        // Act
        let chunks = wrap_diff_content(content, 4);

        // Assert
        assert_eq!(chunks, vec!["abcd"]);
    }
}

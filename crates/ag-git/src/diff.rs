//! Pure inspection of captured unified diffs, without reading a live worktree.

/// One changed file and its original unified-diff text.
#[derive(Debug)]
pub struct DiffFile<'a> {
    /// Path after the change; deleted files retain their old path here.
    pub new_path: String,
    /// Path before the change; added files retain their new path here.
    pub old_path: String,
    /// Complete original file diff, including metadata and hunk headers.
    pub text: &'a str,
}

impl<'a> DiffFile<'a> {
    /// Parses file boundaries and decodes Git-quoted paths.
    ///
    /// Non-diff input is left to the caller; malformed file headers are
    /// retained with an empty path rather than silently losing their contents.
    pub fn parse(input: &'a str) -> Vec<Self> {
        let mut starts = Vec::new();
        let mut offset = 0;
        for line in input.split_inclusive('\n') {
            if line.starts_with("diff --git ") {
                starts.push(offset);
            }
            offset += line.len();
        }
        starts
            .iter()
            .enumerate()
            .map(|(index, start)| {
                let end = starts.get(index + 1).copied().unwrap_or(input.len());
                let text = &input[*start..end];
                let (old_path, new_path) = Self::paths(text).unwrap_or_default();

                Self {
                    new_path,
                    old_path,
                    text,
                }
            })
            .collect()
    }

    /// Returns source ranges whose consecutive lines exactly match `snippet`.
    ///
    /// Matching ignores indentation but never crosses a hunk gap. At least
    /// one cited line must be changed. Repeated matches remain ambiguous for
    /// the caller to resolve using a supplied range, rather than guessing.
    pub fn source_ranges(&self, snippet: &str, old_side: bool) -> Vec<(u32, u32)> {
        let wanted: Vec<_> = snippet.lines().map(str::trim).collect();
        if wanted.is_empty() || wanted.iter().all(|line| line.is_empty()) {
            return Vec::new();
        }
        let mut ranges = Vec::new();
        let mut lines = Vec::new();
        let mut next = None;
        for line in self.text.lines() {
            if line.starts_with("@@") {
                Self::match_lines(&lines, &wanted, &mut ranges);
                lines.clear();
                next = hunk_starts(line).map(|(old, new)| if old_side { old } else { new });
            } else if let Some(number) = next {
                let marker = if old_side { '-' } else { '+' };
                if let Some(content) = line.strip_prefix(marker) {
                    lines.push((number, content.trim(), true));
                    next = number.checked_add(1);
                } else if line.starts_with(' ') || line.is_empty() {
                    lines.push((number, line.trim(), false));
                    next = number.checked_add(1);
                }
            }
        }
        Self::match_lines(&lines, &wanted, &mut ranges);

        ranges
    }

    fn match_lines(lines: &[(u32, &str, bool)], wanted: &[&str], ranges: &mut Vec<(u32, u32)>) {
        for window in lines.windows(wanted.len()) {
            if window
                .iter()
                .zip(wanted)
                .all(|(line, text)| line.1 == *text)
                && window.iter().any(|line| line.2)
            {
                ranges.push((window[0].0, window[window.len() - 1].0));
            }
        }
    }

    fn paths(text: &str) -> Option<(String, String)> {
        // The ---/+++ form keeps unquoted spaces unambiguous. Metadata-only
        // changes have no such lines, so decode the diff header as a fallback.
        let metadata = text.lines().take_while(|line| !line.starts_with("@@"));
        let old = metadata.clone().find_map(|line| line.strip_prefix("--- "));
        let new = metadata.clone().find_map(|line| line.strip_prefix("+++ "));
        if let (Some(old), Some(new)) = (old, new) {
            let old = Self::path_line(old)?;
            let new = Self::path_line(new)?;
            let old = old.strip_prefix("a/").unwrap_or(&old);
            let new = new.strip_prefix("b/").unwrap_or(&new);

            return Some((
                if old == "/dev/null" { new } else { old }.to_string(),
                if new == "/dev/null" { old } else { new }.to_string(),
            ));
        }
        let old = metadata.clone().find_map(|line| {
            line.strip_prefix("rename from ")
                .or_else(|| line.strip_prefix("copy from "))
        });
        let new = metadata.clone().find_map(|line| {
            line.strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
        });
        if let (Some(old), Some(new)) = (old, new) {
            return Some((Self::path_line(old)?, Self::path_line(new)?));
        }
        let header = text.lines().next()?.strip_prefix("diff --git ")?;
        // Git leaves spaces unquoted. Without rename/copy metadata, a
        // space-containing path is unambiguous when both sides are identical.
        if let Some(paths) = header.strip_prefix("a/") {
            for (separator, _) in paths.match_indices(" b/") {
                let old = &paths[..separator];
                let new = &paths[separator + 3..];
                if !old.is_empty() && old == new {
                    return Some((old.to_string(), new.to_string()));
                }
            }
        }
        let (old, remaining) = decode_path(header)?;
        let (new, trailing) = decode_path(remaining.trim_start())?;
        if !trailing.is_empty() {
            return None;
        }

        Some((
            old.strip_prefix("a/")?.to_string(),
            new.strip_prefix("b/")?.to_string(),
        ))
    }

    fn path_line(line: &str) -> Option<String> {
        let path = line.split('\t').next()?;

        if path.starts_with('"') {
            decode_path(path).map(|(path, _)| path)
        } else {
            Some(path.to_string())
        }
    }
}

/// Parses the old/new starting line numbers of a unified-diff hunk.
pub fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let ranges = line.strip_prefix("@@ -")?.split_once(" @@")?.0;
    let (old, new) = ranges.split_once(" +")?;

    Some((
        old.split(',').next()?.parse().ok()?,
        new.split(',').next()?.parse().ok()?,
    ))
}

fn decode_path(input: &str) -> Option<(String, &str)> {
    if !input.starts_with('"') {
        let end = input.find(' ').unwrap_or(input.len());

        return (!input[..end].is_empty()).then(|| (input[..end].to_string(), &input[end..]));
    }
    let bytes = input.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Some((String::from_utf8(decoded).ok()?, &input[index + 1..])),
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                let byte = match escaped {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    b'0'..=b'7' => {
                        let mut value = u16::from(escaped - b'0');
                        for _ in 1..3 {
                            let Some(digit @ b'0'..=b'7') = bytes.get(index + 1) else {
                                break;
                            };
                            index += 1;
                            value = value * 8 + u16::from(*digit - b'0');
                        }
                        u8::try_from(value).ok()?
                    }
                    _ => return None,
                };
                decoded.push(byte);
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }

    None
}

#[cfg(test)]
#[path = "diff_test.rs"]
mod tests;

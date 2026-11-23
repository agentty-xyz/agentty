//! Raw colour detection in page templates and other HTML, and the
//! `theme-color` metas whose value the generator writes.
//!
//! Inline `<style>` elements and `style` attributes are searched as
//! stylesheets, inline `<script>` elements as scripts, and colour attributes,
//! such as an SVG `fill`, as one declaration value. Text, comments, and other
//! attributes are not searched.

use std::ops::Range;

use super::script;
use super::stylesheet::{self, RawColor};

/// Attributes whose value is a colour.
const COLOR_ATTRIBUTES: [&str; 7] = [
    "bgcolor",
    "color",
    "fill",
    "flood-color",
    "lighting-color",
    "stop-color",
    "stroke",
];

/// What a template holds that the design token check cares about.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Scan {
    pub(super) colors: Vec<RawColor>,
    /// The byte range of each `theme-color` meta's `content` value.
    pub(super) theme_colors: Vec<Range<usize>>,
}

/// Returns the raw colours and `theme-color` metas of `source`.
pub(super) fn scan(source: &str) -> Scan {
    let mut scan = Scan::default();
    let mut index = 0;
    while let Some(offset) = source[index..].find('<') {
        let start = index + offset;
        if source[start..].starts_with("<!--") {
            index = source[start..]
                .find("-->")
                .map_or(source.len(), |end| start + end + 3);
            continue;
        }
        let name_end = source[start + 1..]
            .find(|char: char| !(char.is_ascii_alphanumeric() || char == '-' || char == ':'))
            .map_or(source.len(), |end| start + 1 + end);
        let name = source[start + 1..name_end].to_ascii_lowercase();
        if !name.starts_with(|char: char| char.is_ascii_alphabetic()) {
            index = start + 1;
            continue;
        }
        let (attributes, tag_end) = attributes(source, name_end);
        scan.tag(source, &name, &attributes);
        index = tag_end;
        if name == "script" || name == "style" {
            let close = format!("</{name}");
            let end = source[index..]
                .to_ascii_lowercase()
                .find(&close)
                .map_or(source.len(), |end| index + end);
            let body = &source[index..end];
            let first_line = line_at(source, index);
            let colors = if name == "script" {
                script::raw_colors(body, first_line)
            } else {
                shifted(stylesheet::raw_colors(body, false), first_line)
            };
            scan.colors.extend(colors);
            index = end;
        }
    }

    scan
}

impl Scan {
    /// Records the colours in one tag's attributes, and its `content` when
    /// it is a `theme-color` meta.
    fn tag(&mut self, source: &str, name: &str, attributes: &[Attribute]) {
        let value = |key: &str| {
            attributes
                .iter()
                .find(|attribute| attribute.name == key)
                .map(|attribute| attribute.value.clone())
        };
        let is_theme_color = name == "meta"
            && value("name").is_some_and(|range| source[range].eq_ignore_ascii_case("theme-color"));
        if let Some(content) = value("content").filter(|_| is_theme_color) {
            self.theme_colors.push(content);
        }
        for attribute in attributes {
            let text = &source[attribute.value.clone()];
            let declarations = if attribute.name == "style" {
                format!("{text};")
            } else if COLOR_ATTRIBUTES.contains(&attribute.name.as_str()) {
                format!("color: {text};")
            } else {
                continue;
            };
            let first_line = line_at(source, attribute.value.start);
            self.colors.extend(shifted(
                stylesheet::raw_colors(&declarations, false),
                first_line,
            ));
        }
    }
}

/// One attribute: its lowercase name and the byte range of its value.
struct Attribute {
    name: String,
    value: Range<usize>,
}

/// Reads the attributes of a tag whose name ends at `start`, returning them
/// and the index just past the tag.
fn attributes(source: &str, start: usize) -> (Vec<Attribute>, usize) {
    let bytes = source.as_bytes();
    let mut attributes = Vec::new();
    let mut index = start;
    loop {
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'/')
        {
            index += 1;
        }
        match bytes.get(index) {
            None => return (attributes, index),
            Some(b'>') => return (attributes, index + 1),
            Some(_) => {}
        }
        let name_start = index;
        while bytes
            .get(index)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>' | b'/'))
        {
            index += 1;
        }
        let name = source[name_start..index].to_ascii_lowercase();
        let mut value = index..index;
        let mut cursor = index;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            let (value_start, end) = if let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor) {
                let end = source[cursor + 1..]
                    .find(char::from(*quote))
                    .map_or(source.len(), |end| cursor + 1 + end);

                (cursor + 1, end)
            } else {
                let end = source[cursor..]
                    .find(|char: char| char.is_ascii_whitespace() || char == '>')
                    .map_or(source.len(), |end| cursor + end);

                (cursor, end)
            };
            value = value_start..end;
            // Step past a closing quote.
            index = if end < source.len() && value_start > cursor {
                end + 1
            } else {
                end
            };
        }
        attributes.push(Attribute { name, value });
    }
}

/// Moves colours found in text that starts on `first_line` to their line
/// in the whole file.
fn shifted(colors: Vec<RawColor>, first_line: usize) -> Vec<RawColor> {
    colors
        .into_iter()
        .map(|color| RawColor {
            line: color.line + first_line - 1,
            text: color.text,
        })
        .collect()
}

/// Returns the 1-based line of byte `offset` in `source`.
pub(super) fn line_at(source: &str, offset: usize) -> usize {
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

#[cfg(test)]
#[path = "template_test.rs"]
mod tests;

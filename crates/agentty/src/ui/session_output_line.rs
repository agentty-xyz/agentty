//! Shared message segments with viewport-sized access to transcript rows.

use std::sync::Arc;

use ratatui::text::Line;

/// Rendered rows assembled without cloning the contents of retained messages.
#[derive(Default)]
pub(crate) struct OutputLines {
    len: usize,
    segments: Vec<Segment>,
}

impl OutputLines {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn last(&self) -> Option<&Line<'static>> {
        self.segments
            .last()
            .map(|segment| &segment.lines[segment.end - segment.start - 1])
    }

    pub(crate) fn pop(&mut self) {
        if let Some(segment) = self.segments.last_mut() {
            segment.end -= 1;
            self.len -= 1;
            if segment.end == segment.start {
                self.segments.pop();
            }
        }
    }

    pub(crate) fn push(&mut self, line: Line<'static>) {
        self.append(Arc::from([line]));
    }

    pub(crate) fn extend(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        self.append(lines.into_iter().collect());
    }

    pub(crate) fn append(&mut self, lines: Arc<[Line<'static>]>) {
        if lines.is_empty() {
            return;
        }
        let start = self.len;
        self.len += lines.len();
        self.segments.push(Segment {
            end: self.len,
            lines,
            start,
        });
    }

    /// Shares an already rendered message, including any trimmed segment ends.
    pub(crate) fn append_shared(&mut self, lines: &Self) {
        for segment in &lines.segments {
            let start = self.len;
            self.len += segment.end - segment.start;
            self.segments.push(Segment {
                end: self.len,
                lines: Arc::clone(&segment.lines),
                start,
            });
        }
    }

    /// Finds the first segment by row index, then visits only the viewport.
    pub(crate) fn visible_lines(
        &self,
        first: usize,
        height: usize,
    ) -> impl Iterator<Item = &Line<'static>> {
        let first_segment = self
            .segments
            .partition_point(|segment| segment.end <= first);

        self.segments[first_segment..]
            .iter()
            .flat_map(move |segment| {
                let start = first.saturating_sub(segment.start);
                let end = segment.end - segment.start;

                segment.lines[start..end].iter()
            })
            .take(height)
    }

    /// Returns the row count after inspecting only trailing blank rows.
    pub(crate) fn trimmed_len(&self) -> usize {
        let trailing = self
            .segments
            .iter()
            .rev()
            .flat_map(|segment| segment.lines[..segment.end - segment.start].iter().rev())
            .take_while(|line| line.width() == 0)
            .count();

        self.len - trailing
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Line<'static>> {
        self.visible_lines(0, self.len)
    }
}

struct Segment {
    end: usize,
    lines: Arc<[Line<'static>]>,
    start: usize,
}

#[cfg(test)]
#[path = "session_output_line_test.rs"]
mod tests;

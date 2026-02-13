use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::model::Session;
use crate::ui::Page;
use crate::ui::util::{DiffLineKind, max_diff_line_number, parse_diff_lines};

pub struct DiffPage<'a> {
    pub diff: String,
    pub scroll_offset: u16,
    pub session: &'a Session,
}

impl<'a> DiffPage<'a> {
    pub fn new(session: &'a Session, diff: String, scroll_offset: u16) -> Self {
        Self {
            diff,
            scroll_offset,
            session,
        }
    }
}

impl Page for DiffPage<'_> {
    fn render(&mut self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .margin(1)
            .split(area);

        let output_area = chunks[0];
        let footer_area = chunks[1];

        let title = format!(" Diff — {} ", self.session.display_title());

        let parsed = parse_diff_lines(&self.diff);
        let max_num = max_diff_line_number(&parsed);
        let gutter_width = if max_num == 0 {
            1
        } else {
            max_num.ilog10() as usize + 1
        };

        let gutter_style = Style::default().fg(Color::DarkGray);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(parsed.len());

        for diff_line in &parsed {
            let (sign, content_style) = match diff_line.kind {
                DiffLineKind::FileHeader => {
                    if diff_line.content.starts_with("diff ") && !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Yellow),
                    )));

                    continue;
                }
                DiffLineKind::HunkHeader => {
                    lines.push(Line::from(Span::styled(
                        diff_line.content,
                        Style::default().fg(Color::Cyan),
                    )));

                    continue;
                }
                DiffLineKind::Addition => ("+", Style::default().fg(Color::Green)),
                DiffLineKind::Deletion => ("-", Style::default().fg(Color::Red)),
                DiffLineKind::Context => (" ", Style::default().fg(Color::Gray)),
            };

            let old_str = match diff_line.old_line {
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };
            let new_str = match diff_line.new_line {
                Some(num) => format!("{num:>gutter_width$}"),
                None => " ".repeat(gutter_width),
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{old_str}│{new_str} "), gutter_style),
                Span::styled(sign, content_style),
                Span::styled(diff_line.content, content_style),
            ]));
        }

        if lines.is_empty() {
            lines.push(Line::from(" No changes found. "));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(Color::Yellow))),
            )
            .scroll((self.scroll_offset, 0));

        f.render_widget(paragraph, output_area);

        let help_message =
            Paragraph::new("q: back | j/k: scroll").style(Style::default().fg(Color::Gray));
        f.render_widget(help_message, footer_area);
    }
}

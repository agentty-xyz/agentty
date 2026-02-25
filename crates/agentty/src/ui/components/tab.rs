use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::app::Tab;
use crate::ui::Component;

/// Header tabs rendered at the top of list mode pages.
pub struct Tabs {
    current_tab: Tab,
}

impl Tabs {
    /// Creates a tabs component with the provided active tab.
    pub fn new(current_tab: Tab) -> Self {
        Self { current_tab }
    }
}

impl Component for Tabs {
    fn render(&self, f: &mut Frame, area: Rect) {
        let line = Line::from(tab_spans(self.current_tab));
        let paragraph = Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .padding(Padding::top(1)),
        );
        f.render_widget(paragraph, area);
    }
}

fn tab_spans(current_tab: Tab) -> Vec<Span<'static>> {
    [Tab::Projects, Tab::Sessions, Tab::Stats, Tab::Settings]
        .iter()
        .map(|tab| {
            let label = format!(" {} ", tab.title());
            if *tab == current_tab {
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(label, Style::default().fg(Color::Gray))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_spans_use_equal_spacing_between_labels() {
        // Arrange
        let current_tab = Tab::Projects;

        // Act
        let spans = tab_spans(current_tab);
        let rendered_tabs: String = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        // Assert
        assert_eq!(rendered_tabs, " Projects  Sessions  Stats  Settings ");
    }

    #[test]
    fn test_tab_spans_highlight_the_active_tab() {
        // Arrange
        let current_tab = Tab::Stats;

        // Act
        let spans = tab_spans(current_tab);

        // Assert
        assert_eq!(spans[0].style.fg, Some(Color::Gray));
        assert_eq!(spans[1].style.fg, Some(Color::Gray));
        assert_eq!(spans[2].style.fg, Some(Color::Yellow));
        assert_eq!(spans[3].style.fg, Some(Color::Gray));
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
    }
}

use std::env;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::icon::Icon;
use crate::ui::{Component, style};

/// Footer widget that renders the working directory and optional git status.
pub struct FooterBar {
    git_base_ref: Option<String>,
    git_base_status: Option<(u32, u32)>,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_upstream_ref: Option<String>,
    working_dir: String,
    workspace_context_visible: bool,
}

impl FooterBar {
    /// Creates a footer bar initialized with the active working directory and
    /// visible workspace context.
    pub fn new(working_dir: String) -> Self {
        Self {
            git_base_ref: None,
            git_base_status: None,
            git_branch: None,
            git_status: None,
            git_upstream_ref: None,
            working_dir,
            workspace_context_visible: true,
        }
    }

    /// Sets whether the footer renders active workspace path and branch
    /// context.
    #[must_use]
    pub fn workspace_context_visible(mut self, visible: bool) -> Self {
        self.workspace_context_visible = visible;
        self
    }

    /// Sets the active git branch name.
    #[must_use]
    pub fn git_branch(mut self, branch: Option<String>) -> Self {
        self.git_branch = branch;
        self
    }

    /// Sets the base branch reference label used by session footers.
    #[must_use]
    pub fn git_base_ref(mut self, base_ref: Option<String>) -> Self {
        self.git_base_ref = base_ref;
        self
    }

    /// Sets the base-branch comparison `(ahead, behind)` counts for session
    /// footers.
    #[must_use]
    pub fn git_base_status(mut self, status: Option<(u32, u32)>) -> Self {
        self.git_base_status = status;
        self
    }

    /// Sets the git status `(ahead, behind)` counts for the rendered branch.
    ///
    /// Project footers typically pass upstream-tracking counts, while session
    /// footers use this slot for tracked-remote counts.
    #[must_use]
    pub fn git_status(mut self, status: Option<(u32, u32)>) -> Self {
        self.git_status = status;
        self
    }

    /// Sets the upstream reference tracked by the active branch.
    #[must_use]
    pub fn git_upstream_ref(mut self, upstream_ref: Option<String>) -> Self {
        self.git_upstream_ref = upstream_ref;
        self
    }

    /// Returns the footer branch label, including the tracked upstream when
    /// available.
    fn branch_label(&self, branch: &str) -> String {
        match self.git_upstream_ref.as_deref() {
            Some(upstream_ref) => format!("{branch} -> {upstream_ref}"),
            None => branch.to_string(),
        }
    }

    /// Returns the full text rendered after the branch icon.
    fn branch_text(&self, branch: &str) -> String {
        if let Some(base_ref) = self.git_base_ref.as_deref() {
            return self.session_branch_text(branch, base_ref);
        }

        let status_text = self.git_status.map(Self::format_status).unwrap_or_default();

        format!("{status_text}{}", self.branch_label(branch))
    }

    /// Returns the compact session footer text with base and optional remote
    /// or local segments.
    fn session_branch_text(&self, branch: &str, base_ref: &str) -> String {
        let mut segments = vec![Self::format_session_segment(self.git_base_status, base_ref)];
        let remote_label = match self.git_upstream_ref.as_deref() {
            Some(upstream_ref) => format!("{branch} -> {upstream_ref}"),
            None => branch.to_string(),
        };
        segments.push(Self::format_session_segment(
            self.git_status,
            remote_label.as_str(),
        ));

        segments.join(" | ")
    }

    /// Formats one session segment as `<stats> <label>`.
    fn format_session_segment(status: Option<(u32, u32)>, label: &str) -> String {
        let status_text = status.map_or_else(|| Self::format_status((0, 0)), Self::format_status);

        format!("{status_text}{label}")
    }

    /// Formats one status segment with no reference label attached.
    fn format_status(status: (u32, u32)) -> String {
        let (ahead, behind) = status;

        if ahead == 0 && behind == 0 {
            return format!("{} ", Icon::Check);
        }

        format!("{}{behind} {}{ahead} ", Icon::ArrowDown, Icon::ArrowUp)
    }
}

impl Component for FooterBar {
    /// Renders the footer background plus optional workspace path and git
    /// context.
    fn render(&self, f: &mut Frame, area: Rect) {
        if area.width == 0 {
            return;
        }

        let total_width = usize::from(area.width);

        if !self.workspace_context_visible {
            let footer = Paragraph::new(Line::from(" ".repeat(total_width)))
                .style(Style::default().bg(style::palette::surface()));
            f.render_widget(footer, area);

            return;
        }

        let display_path = if let Some(home) = env::home_dir() {
            if let Ok(path) = Path::new(&self.working_dir).strip_prefix(home) {
                format!("~/{}", path.display())
            } else {
                self.working_dir.clone()
            }
        } else {
            self.working_dir.clone()
        };

        let path_style = Style::default()
            .fg(style::palette::text())
            .add_modifier(Modifier::DIM);

        let left_text = Span::styled(format!(" {display_path}"), path_style);

        let left_width = left_text.width();
        let mut spans = vec![left_text];

        if let Some(branch) = &self.git_branch {
            let trailing_branch_padding = 1;
            let branch_text = self.branch_text(branch);

            let branch_span =
                Span::styled(format!("{} {branch_text}", Icon::GitBranch), path_style);
            let branch_width = branch_span.width();

            if left_width + branch_width + trailing_branch_padding <= total_width {
                let padding_width =
                    total_width - left_width - branch_width - trailing_branch_padding;
                let padding = " ".repeat(padding_width);

                spans.push(Span::raw(padding));
                spans.push(branch_span);
                spans.push(Span::raw(" ".repeat(trailing_branch_padding)));
            }
        }

        let line_width: usize = spans.iter().map(Span::width).sum();
        if line_width < total_width {
            spans.push(Span::raw(" ".repeat(total_width - line_width)));
        }

        let footer =
            Paragraph::new(Line::from(spans)).style(Style::default().bg(style::palette::surface()));

        f.render_widget(footer, area);
    }
}

#[cfg(test)]
#[path = "footer_bar_test.rs"]
mod tests;

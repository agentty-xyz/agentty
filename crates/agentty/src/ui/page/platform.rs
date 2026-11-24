//! Platform tab page showing the sign-in, the account, and remote control
//! through the platform's relay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::presentation::help_action::platform_tab_actions;
use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformState, PlatformTabView,
};
use crate::ui::page::remote_control::{
    device_lines, field_line, fitting_qr_code, muted_line, outcome_lines, pairing_request_lines,
    relay_lines,
};
use crate::ui::style::palette;
use crate::ui::{Page, layout, style};

/// Explains what signing in gives, while signed out.
const SIGN_IN_PITCH: &str = "Sign in to connect this Agentty to the platform's relay, so devices \
                             you pair reach it without a tunnel or a relay of your own. Signing \
                             in grants no device any access: each one still pairs with a one-time \
                             code that you approve here, and its traffic is encrypted end to end.";
/// Explains what keep on does, under its state.
const KEEP_ON_HINT: &str =
    "With keep on, remote control through the platform's relay starts whenever Agentty starts.";

/// Renders the Platform tab.
pub struct PlatformPage<'a> {
    project_name: Option<String>,
    view: PlatformTabView<'a>,
}

impl<'a> PlatformPage<'a> {
    /// Creates the page for one frame. Paired devices follow the sessions
    /// of the project named `project_name`.
    pub(crate) fn new(view: PlatformTabView<'a>, project_name: Option<String>) -> Self {
        Self { project_name, view }
    }

    /// Returns the title naming the project that paired devices follow.
    fn title(&self) -> String {
        match self.project_name.as_deref() {
            Some(name) => format!("'{name}' on the platform"),
            None => "Agentty Platform".to_string(),
        }
    }

    /// Returns all render lines for content `width` columns wide and
    /// `height` rows high, with the relay QR code only when it fits.
    fn lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let lines = self.content_lines(width, &[]);
        let qr_code = fitting_qr_code(self.view.remote_control, &lines, width, height);
        if qr_code.is_empty() {
            return lines;
        }

        self.content_lines(width, qr_code)
    }

    /// Returns the render lines with `qr_code` rows below the relay status.
    fn content_lines(&self, width: usize, qr_code: &[String]) -> Vec<Line<'static>> {
        let platform = self.view.platform;
        let mut lines = vec![field_line(
            "Address",
            &platform.platform_url,
            palette::text(),
        )];
        match &platform.state {
            PlatformState::SignedOut { notice } => {
                lines.push(field_line("Account", "Signed out", palette::text_muted()));
                lines.push(Line::from(""));
                lines.push(muted_line(SIGN_IN_PITCH));
                if let Some(notice) = notice {
                    lines.push(Line::from(""));
                    lines.push(colored_line(notice, palette::text()));
                }
            }
            PlatformState::SigningOut { notice } => {
                lines.push(field_line("Account", "Signing out...", palette::warning()));
                if let Some(notice) = notice {
                    lines.push(Line::from(""));
                    lines.push(colored_line(notice, palette::warning()));
                }
            }
            PlatformState::Requesting => {
                lines.push(field_line("Account", "Signing in...", palette::warning()));
            }
            PlatformState::SigningIn(sign_in) => {
                lines.push(field_line(
                    "Account",
                    "Waiting for approval",
                    palette::warning(),
                ));
                lines.push(field_line("Code", &sign_in.user_code, palette::warning()));
                lines.push(field_line("Open", &sign_in.link, palette::text()));
                lines.push(Line::from(""));
                lines.push(muted_line(
                    "Open the page in a browser, sign in, and approve this Agentty if the page \
                     shows the same code.",
                ));
                if let Some(notice) = &sign_in.notice {
                    lines.push(colored_line(notice, palette::success()));
                }
            }
            PlatformState::SignedIn(account) => {
                lines.extend(self.account_lines(account, width, qr_code));
            }
        }

        lines.push(Line::from(""));
        lines.push(muted_line(&self.help_text()));

        lines
    }

    /// Returns the signed-in account, its keep-on state, and, while
    /// registered, remote control through the platform's relay.
    fn account_lines(
        &self,
        account: &PlatformAccountView,
        width: usize,
        qr_code: &[String],
    ) -> Vec<Line<'static>> {
        let login = account.login.as_deref().unwrap_or("Signed in");
        let mut lines = vec![field_line("Account", login, palette::text())];
        if let PlatformAccess::Registered { name } = &account.access {
            lines.push(field_line("Name", name, palette::text()));
        }
        lines.extend(keep_on_lines(account));
        lines.push(Line::from(""));
        let (status, color) = match &account.access {
            PlatformAccess::Checking => ("Checking the account...", palette::warning()),
            PlatformAccess::NoRelay => (
                "The platform offers this account no relay. Press Enter to check again.",
                palette::text_muted(),
            ),
            PlatformAccess::Unavailable { message } => {
                lines.push(colored_line(message, palette::danger()));
                lines.push(Line::from(""));
                ("Press Enter to try again.", palette::text_muted())
            }
            PlatformAccess::Waitlisted => (
                "This account is on the platform's waitlist and has no access yet, so this \
                 Agentty does not use the platform's relay. Press Enter to check again.",
                palette::warning(),
            ),
            PlatformAccess::Registered { .. } => {
                lines.extend(self.remote_lines(width, qr_code));

                return lines;
            }
        };
        lines.push(colored_line(status, color));

        lines
    }

    /// Returns remote control through the platform's relay: pairing
    /// requests, the relay link, outcomes, and paired devices.
    fn remote_lines(&self, width: usize, qr_code: &[String]) -> Vec<Line<'static>> {
        let remote_control = self.view.remote_control;
        let mut lines = pairing_request_lines(remote_control);
        let relay = remote_control
            .listener
            .as_ref()
            .and_then(|listener| listener.relay.as_ref());
        if let Some(relay) = relay {
            lines.push(field_line("Remote", "On", palette::success()));
            lines.extend(relay_lines(relay, qr_code));
        } else {
            lines.push(field_line("Remote", "Off", palette::text_muted()));
            lines.push(muted_line(
                "Start remote control to follow and steer this project's sessions from a paired \
                 phone or browser through the platform's relay.",
            ));
        }
        lines.extend(outcome_lines(remote_control));
        lines.extend(device_lines(remote_control, width));

        lines
    }

    /// Returns the key hints that apply to the current state.
    fn help_text(&self) -> String {
        let hints: Vec<String> = platform_tab_actions(self.view)
            .iter()
            .map(|action| format!("{}: {}", action.key, action.footer_label))
            .collect();

        hints.join(" | ")
    }
}

impl Page for PlatformPage<'_> {
    /// Renders the platform state in one bordered section that fills the
    /// tab page.
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let main_area = layout::tab_page_area(area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(self.title())
            .border_style(style::border_style())
            .padding(Padding::horizontal(1));
        let content_area = block.inner(main_area);
        let lines = self.lines(
            usize::from(content_area.width).max(1),
            usize::from(content_area.height),
        );
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true }).block(block);

        frame.render_widget(paragraph, main_area);
    }
}

/// Returns the keep-on state, what it does, and a warning when its last
/// change could not be saved.
fn keep_on_lines(account: &PlatformAccountView) -> Vec<Line<'static>> {
    let (state, color) = if account.keep_on {
        ("On", palette::success())
    } else {
        ("Off", palette::text_muted())
    };
    let mut lines = vec![
        field_line("Keep on", state, color),
        muted_line(KEEP_ON_HINT),
    ];
    if let Some(notice) = &account.keep_on_notice {
        lines.push(colored_line(notice, palette::warning()));
    }

    lines
}

/// Builds one row of `text` in `color`.
fn colored_line(text: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::default().fg(color)))
}

#[cfg(test)]
#[path = "platform_test.rs"]
mod tests;

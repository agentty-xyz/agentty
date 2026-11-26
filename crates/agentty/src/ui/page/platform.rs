//! Platform tab page showing the sign-in, the account, and remote control
//! through the platform's relay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::presentation::help_action::platform_tab_actions;
use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformState, PlatformTabView,
};
use crate::ui::Page;
use crate::ui::page::remote_control::{
    PageContent, QrPlacement, device_lines, field_line, hint_lines, muted_line, outcome_lines,
    pairing_request_lines, relay_lines, relay_qr_code, render_section,
};
use crate::ui::style::palette;

/// Explains what signing in gives, while signed out.
const SIGN_IN_PITCH: &str = "Sign in to connect this Agentty to the platform's relay, so devices \
                             you pair reach it without a tunnel or a relay of your own. Signing \
                             in grants no device any access: each one still pairs with a one-time \
                             code that you approve here, and its traffic is encrypted end to end.";
/// Explains what keep on does, under its state.
const KEEP_ON_HINT: &str =
    "With keep on, remote control through the platform's relay starts whenever Agentty starts.";

/// Title of the Platform tab, which names no project: paired devices pick
/// their own among the projects the terminal lists.
const TITLE: &str = "Agentty Platform";

/// Renders the Platform tab.
pub struct PlatformPage<'a> {
    view: PlatformTabView<'a>,
}

impl<'a> PlatformPage<'a> {
    /// Creates the page for one frame.
    pub(crate) fn new(view: PlatformTabView<'a>) -> Self {
        Self { view }
    }

    /// Lays the page out for content `width` columns wide and `height` rows
    /// high, with the relay QR code wherever it fits.
    fn content(&self, width: usize, height: usize) -> PageContent<'a> {
        PageContent::new(
            relay_qr_code(self.view.remote_control),
            width,
            height,
            |text_width, placement| self.lines(text_width, placement),
        )
    }

    /// Returns the render lines for text `width` columns wide, with the
    /// relay QR code at `placement`.
    fn lines(&self, width: usize, placement: QrPlacement) -> Vec<Line<'static>> {
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
                lines.extend(self.account_lines(account, width, placement));
            }
        }

        lines.push(Line::from(""));
        lines.extend(hint_lines(&self.hints(), width));

        lines
    }

    /// Returns the signed-in account, its keep-on state, and, while
    /// registered, remote control through the platform's relay.
    fn account_lines(
        &self,
        account: &PlatformAccountView,
        width: usize,
        placement: QrPlacement,
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
                lines.extend(self.remote_lines(width, placement));

                return lines;
            }
        };
        lines.push(colored_line(status, color));

        lines
    }

    /// Returns remote control through the platform's relay: pairing
    /// requests, the relay link, outcomes, and paired devices.
    fn remote_lines(&self, width: usize, placement: QrPlacement) -> Vec<Line<'static>> {
        let remote_control = self.view.remote_control;
        let mut lines = pairing_request_lines(remote_control);
        let relay = remote_control
            .listener
            .as_ref()
            .and_then(|listener| listener.relay.as_ref());
        if let Some(relay) = relay {
            lines.push(field_line("Remote", "On", palette::success()));
            lines.extend(relay_lines(relay, placement));
        } else {
            lines.push(field_line("Remote", "Off", palette::text_muted()));
            lines.push(muted_line(
                "Start remote control to follow and steer the sessions of your projects from a \
                 paired phone or browser through the platform's relay.",
            ));
        }
        lines.extend(outcome_lines(remote_control));
        lines.extend(device_lines(remote_control, width));

        lines
    }

    /// Returns the key hints that apply to the current state.
    fn hints(&self) -> Vec<String> {
        platform_tab_actions(self.view)
            .iter()
            .map(|action| format!("{}: {}", action.key, action.footer_label))
            .collect()
    }
}

impl Page for PlatformPage<'_> {
    /// Renders the platform state in one bordered section that fills the
    /// tab page.
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        render_section(frame, area, TITLE, |width, height| {
            self.content(width, height)
        });
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

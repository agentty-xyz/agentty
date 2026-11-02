use std::ffi::OsString;
use std::process::Stdio;

/// Opens URLs for the user, such as a sign-in page in the default browser.
#[cfg_attr(test, mockall::automock)]
pub trait BrowserClient: Send + Sync {
    /// Asks the system to open `url`. Failures are ignored because the
    /// caller also shows the URL for the user to open by hand.
    fn open(&self, url: &str);
}

/// Production [`BrowserClient`] that launches the platform URL handler:
/// `open` on macOS and `xdg-open` elsewhere.
pub struct RealBrowserClient {
    program: OsString,
}

impl RealBrowserClient {
    /// Creates a browser client using the platform URL handler.
    pub fn new() -> Self {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };

        Self {
            program: OsString::from(program),
        }
    }

    /// Builds the detached URL-handler command for `url`.
    fn command(&self, url: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        command
    }
}

impl Default for RealBrowserClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserClient for RealBrowserClient {
    fn open(&self, url: &str) {
        let _ = self.command(url).spawn();
    }
}

#[cfg(test)]
#[path = "browser_test.rs"]
mod tests;

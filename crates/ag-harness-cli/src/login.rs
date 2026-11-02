//! `ag-harness login chatgpt`: browser sign-in that stores a `ChatGPT` plan
//! credential for the `chatgpt` provider.

use std::{env, io};

use ag_harness::provider::{ChatGptSignIn, ChatGptSignInError};

/// Opens a URL for the user, such as in the default browser.
pub(crate) trait Browser {
    /// Asks the system to open `url`; failures only mean the user opens it
    /// by hand.
    fn open(&self, url: &str);
}

/// Opens URLs with the platform's default handler.
pub(crate) struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(program)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Runs the browser sign-in and stores the resulting credential, preferring
/// callback `port`.
pub(crate) async fn login(
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    browser: &dyn Browser,
    output: &mut (impl io::Write + ?Sized),
    port: u16,
) -> Result<(), ChatGptSignInError> {
    let sign_in = ChatGptSignIn::from_environment(environment)?.with_port(port);
    let auth_file = sign_in.auth_file().to_path_buf();
    let pending = sign_in.start().await?;
    let url = pending.authorize_url();
    let _ = writeln!(
        output,
        "Opening your browser to sign in with ChatGPT. If it does not open, visit:\n{url}"
    );
    browser.open(&url);
    let credentials = pending.complete().await?;
    let account = credentials
        .email
        .as_deref()
        .unwrap_or("your ChatGPT account");
    let _ = writeln!(
        output,
        "Signed in as {account}; requests through `--provider chatgpt` use your ChatGPT \
         plan.\nCredentials saved to {}",
        auth_file.display()
    );

    Ok(())
}

/// Default preferred callback port.
pub(crate) const fn default_port() -> u16 {
    1455
}

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;

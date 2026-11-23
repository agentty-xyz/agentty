//! Platform sign-in and account state projected for the Platform tab.

use crate::presentation::remote_control::RemoteControlView;

/// Sign-in state of this Agentty on one platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformView {
    /// Platform origin this Agentty signs in to.
    pub platform_url: String,
    /// Where sign-in stands.
    pub state: PlatformState,
}

impl PlatformView {
    /// Starts signed out of the platform at `platform_url`.
    pub fn new(platform_url: String) -> Self {
        Self {
            platform_url,
            state: PlatformState::SignedOut { notice: None },
        }
    }

    /// Returns the account state while signed in.
    pub fn account(&self) -> Option<&PlatformAccountView> {
        match &self.state {
            PlatformState::SignedIn(account) => Some(account),
            PlatformState::SignedOut { .. }
            | PlatformState::SigningOut { .. }
            | PlatformState::Requesting
            | PlatformState::SigningIn(_) => None,
        }
    }

    /// Returns whether remote control runs through the platform's relay.
    pub fn is_registered(&self) -> bool {
        self.account()
            .is_some_and(|account| matches!(account.access, PlatformAccess::Registered { .. }))
    }
}

/// Where sign-in stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformState {
    /// Not signed in. `notice` says how the last sign-in or sign-out ended.
    SignedOut {
        /// Outcome of the last sign-in or sign-out, when there is one.
        notice: Option<String>,
    },
    /// Signed out here, waiting for the platform to remove the registration
    /// and revoke the token; a new sign-in waits for it.
    SigningOut {
        /// Warning about the sign-out so far, when there is one.
        notice: Option<String>,
    },
    /// Asking the platform for a sign-in code.
    Requesting,
    /// Waiting for the user to approve the sign-in in a browser.
    SigningIn(PlatformSignInView),
    /// Signed in.
    SignedIn(PlatformAccountView),
}

/// A sign-in the user approves in a browser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformSignInView {
    /// Page to open, with the code filled in when the platform offers one.
    pub link: String,
    /// Confirmation of the latest link action, such as a copy.
    pub notice: Option<String>,
    /// Code the browser asks the user to confirm.
    pub user_code: String,
}

/// The signed-in account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformAccountView {
    /// What this Agentty may use of the platform.
    pub access: PlatformAccess,
    /// Account login, once the platform named it.
    pub login: Option<String>,
}

/// What a signed-in Agentty may use of the platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformAccess {
    /// Reading the account and registering this Agentty.
    Checking,
    /// `--remote-relay` names a relay, which remote control uses instead.
    ExplicitRelay,
    /// The platform offers this account no relay.
    NoRelay,
    /// Registered, so remote control runs through the platform's relay.
    Registered {
        /// Name the platform shows for this Agentty.
        name: String,
    },
    /// The account or the registration could not be read.
    Unavailable {
        /// What went wrong, in plain words.
        message: String,
    },
    /// The account is on the platform's waitlist and has no access yet.
    Waitlisted,
}

/// State the Platform tab renders: the sign-in and, once registered, the
/// remote control that runs through the platform's relay.
#[derive(Clone, Copy, Debug)]
pub struct PlatformTabView<'a> {
    /// Sign-in and account state.
    pub platform: &'a PlatformView,
    /// Listener, pairing, and paired-device state.
    pub remote_control: &'a RemoteControlView,
}

#[cfg(test)]
#[path = "platform_test.rs"]
mod tests;

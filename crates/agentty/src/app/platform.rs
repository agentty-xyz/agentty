//! Sign-in to an Agentty Platform behind the Platform tab: device sign-in,
//! account checks, instance registration, and sign-out.
//!
//! Platform requests run in the background and report through
//! [`AppEvent::PlatformUpdated`]; each carries the request it answers, so
//! the result of a cancelled or replaced request is dropped. A registered
//! Agentty hands the platform's relay to remote control. Remote control
//! starts only after an account check the user asked to connect, by signing
//! in, by `Enter`, or by turning keep on on; keep on, saved with the
//! sign-in, also connects the check a launch runs. A launch with
//! `--remote-relay` never enables the platform. The access token goes only
//! to the platform, never to a relay.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::app::link_copy::LinkCopy;
use crate::app::{App, AppEvent, spawn};
use crate::domain::setting::SettingName;
use crate::infra::browser::BrowserClient;
use crate::infra::db::DbError;
use crate::infra::platform::{
    AccessToken, DeviceAuthorization, PlatformClient, PlatformError, RemoteControlService, SignIn,
};
use crate::infra::terminal_clipboard::TerminalClipboard;
use crate::presentation::platform::{
    PlatformAccess, PlatformAccountView, PlatformSignInView, PlatformState, PlatformView,
};

/// Longest name a platform accepts for this Agentty, in characters.
const MAX_INSTANCE_NAME_CHARS: usize = 64;
/// Wait added after each `slow_down`, as RFC 8628 says.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// Shown after the platform revoked the token or no longer knows it.
const SIGNED_OUT_BY_PLATFORM: &str = "The platform no longer accepts this sign-in, so this \
                                      Agentty signed out. Sign in again to use it.";
/// Confirms a copied sign-in link.
const LINK_COPIED: &str = "Link copied.";
/// Reported when the terminal could not be asked to copy.
const LINK_NOT_COPIED: &str = "Could not copy the link; select it instead.";
/// Warns that a sign-out left the saved sign-in behind.
const SAVED_SIGN_IN_KEPT: &str =
    "The saved sign-in could not be removed, so it returns when Agentty restarts.";
/// Warns that a keep-on change lasts only for this launch.
const KEEP_ON_NOT_SAVED: &str =
    "Could not save keep on, so the change lasts only until Agentty quits.";

/// Background platform progress delivered to the foreground reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlatformUpdate {
    /// The platform issued a sign-in code for the user to approve.
    Authorized {
        /// Sign-in request this answers.
        request_id: u64,
        /// The code and the pages to approve it on.
        authorization: Box<DeviceAuthorization>,
    },
    /// The sign-in ended.
    SignInFinished {
        /// Sign-in request this answers.
        request_id: u64,
        /// The token and login, or why the sign-in ended without one.
        result: Result<SignIn, PlatformError>,
    },
    /// The account was read and this Agentty registered as far as it may.
    Refreshed {
        /// Account request this answers.
        request_id: u64,
        /// Account login, when the account could be read.
        login: Option<String>,
        /// What this Agentty may use, or why that is unknown.
        result: Result<Access, PlatformError>,
    },
    /// The platform answered the sign-out.
    SignedOut {
        /// Sign-out request this answers.
        request_id: u64,
        /// Whether the platform removed the registration and the token.
        result: Result<(), PlatformError>,
    },
}

/// What a checked account lets this Agentty use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Access {
    /// The platform offers no relay.
    NoRelay,
    /// Registered as `name`; remote control may use `service`.
    Registered {
        /// Name the platform shows.
        name: String,
        /// The platform's relay and pairing page.
        service: RemoteControlService,
    },
}

/// Platform client, sign-in state, and the token while signed in.
pub(crate) struct Platform {
    browser: Arc<dyn BrowserClient>,
    /// Client for the configured platform, once `--experimental-platform`
    /// enabled the tab.
    client: Option<Arc<dyn PlatformClient>>,
    /// Whether the next account check `Enter` asks for connects, because it
    /// is the first check of a sign-in that could not be saved.
    connects_on_next_check: bool,
    /// Name this Agentty asks to be shown as.
    instance_name: String,
    /// Whether the last sign-out could not remove the saved sign-in, which
    /// then returns on the next launch.
    is_saved_sign_in_kept: bool,
    link_copy: LinkCopy,
    /// Identifies the latest background request.
    request_id: u64,
    /// Whether remote control starts once the running account check
    /// confirms the registration.
    starts_remote_control_after_check: bool,
    /// Running sign-in poll or account check, aborted when a later request
    /// replaces it.
    task: Option<JoinHandle<()>>,
    /// Access token while signed in. It goes only to the platform.
    token: Option<AccessToken>,
    view: PlatformView,
}

impl Platform {
    /// Creates a disabled platform that opens and copies sign-in pages
    /// through `browser` and `clipboard`.
    pub(crate) fn new(
        browser: Arc<dyn BrowserClient>,
        clipboard: Arc<dyn TerminalClipboard>,
    ) -> Self {
        Self {
            browser,
            client: None,
            connects_on_next_check: false,
            instance_name: String::new(),
            is_saved_sign_in_kept: false,
            link_copy: LinkCopy::new(clipboard, |request_id, is_copied| {
                AppEvent::PlatformLinkCopied {
                    is_copied,
                    request_id,
                }
            }),
            request_id: 0,
            starts_remote_control_after_check: false,
            task: None,
            token: None,
            view: PlatformView::new(String::new()),
        }
    }

    /// Returns whether `--experimental-platform` enabled the tab.
    pub(crate) fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// Returns the state projected into the Platform tab.
    pub(crate) fn view(&self) -> &PlatformView {
        &self.view
    }

    /// Stops any running request and returns the identifier the next one
    /// reports under.
    fn next_request(&mut self) -> u64 {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.link_copy.forget();
        self.connects_on_next_check = false;
        self.starts_remote_control_after_check = false;
        self.request_id += 1;

        self.request_id
    }

    /// Adds a warning to a sign-out `notice` when the saved sign-in could
    /// not be removed.
    fn with_kept_sign_in_warning(&self, notice: String) -> String {
        if self.is_saved_sign_in_kept {
            return format!("{notice} {SAVED_SIGN_IN_KEPT}");
        }

        notice
    }

    /// Returns the sign-in page shown, while a sign-in waits.
    fn sign_in_link(&self) -> Option<String> {
        match &self.view.state {
            PlatformState::SigningIn(sign_in) => Some(sign_in.link.clone()),
            _ => None,
        }
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Stored form of a sign-in: the token together with the platform that
/// issued it, so a token is never sent to another platform, and whether
/// keep on starts remote control at launch. A fresh sign-in starts with
/// keep on off, and a sign-out clears it with the token.
#[derive(Deserialize)]
struct StoredSignIn {
    access_token: String,
    /// Missing in a sign-in saved before keep on existed, which reads as
    /// off.
    #[serde(default)]
    keep_on: bool,
    platform_url: String,
}

/// Returns the name this Agentty asks a platform to show for it: `Agentty
/// on` and the computer's name, without control characters and cut to the
/// length the platform accepts.
pub fn platform_instance_name(host_name: &str) -> String {
    let host_name: String = host_name
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let host_name = host_name.trim();
    let name = if host_name.is_empty() {
        "Agentty".to_string()
    } else {
        format!("Agentty on {host_name}")
    };

    name.chars().take(MAX_INSTANCE_NAME_CHARS).collect()
}

impl App {
    /// Offers the experimental Platform tab for the platform at
    /// `platform_url`, reached through `client`, naming this Agentty
    /// `instance_name` there. A sign-in saved for the same platform is
    /// checked right away, and with keep on, remote control starts once the
    /// check confirms the registration.
    pub async fn enable_platform(
        &mut self,
        client: Arc<dyn PlatformClient>,
        platform_url: String,
        instance_name: String,
    ) {
        self.platform.client = Some(client);
        self.platform.instance_name = instance_name;
        self.platform.view = PlatformView::new(platform_url);
        self.tabs.enable_platform();
        // The tab lists paired devices even without `--experimental-remote`.
        self.refresh_remote_devices().await;
        if let Some(stored) = self.stored_platform_sign_in().await {
            self.platform.token = Some(AccessToken::new(stored.access_token));
            self.platform.view.state = PlatformState::SignedIn(PlatformAccountView {
                keep_on: stored.keep_on,
                ..PlatformAccountView::checking(None)
            });
            self.refresh_platform(stored.keep_on).await;
        }
    }

    /// Runs the Platform tab's `Enter` action: starts or cancels a sign-in,
    /// starts or stops remote control through the platform's relay for this
    /// launch, or checks the account again, which connects with keep on.
    pub(crate) async fn activate_platform(&mut self) {
        match &self.platform.view.state {
            // Signing in again waits until the old registration is removed.
            PlatformState::SigningOut { .. } => {}
            PlatformState::SignedOut { .. } => self.start_platform_sign_in(),
            PlatformState::Requesting | PlatformState::SigningIn(_) => {
                self.platform.next_request();
                self.platform.view.state = PlatformState::SignedOut {
                    notice: Some("Sign-in cancelled.".to_string()),
                };
            }
            PlatformState::SignedIn(account) => match account.access {
                PlatformAccess::Checking => {}
                PlatformAccess::Registered { .. }
                    if self.remote_control.view().listener.is_some() =>
                {
                    self.stop_remote_control().await;
                }
                // Checks the sign-in and the registration first, so a token
                // revoked meanwhile signs out instead of dialling the relay.
                PlatformAccess::Registered { .. } => self.refresh_platform(true).await,
                PlatformAccess::NoRelay
                | PlatformAccess::Unavailable { .. }
                | PlatformAccess::Waitlisted => {
                    let starts_remote_control =
                        account.keep_on || self.platform.connects_on_next_check;
                    self.refresh_platform(starts_remote_control).await;
                }
            },
        }
    }

    /// Runs the Platform tab's keep-on toggle while signed in and not
    /// checking: saves whether remote control through the platform starts
    /// whenever Agentty starts. Turning it on while registered with remote
    /// control off also connects now, after an account check; turning it
    /// off leaves running remote control alone.
    pub(crate) async fn toggle_platform_keep_on(&mut self) {
        let (PlatformState::SignedIn(account), Some(token)) =
            (&mut self.platform.view.state, self.platform.token.clone())
        else {
            return;
        };
        if !account.offers_keep_on() {
            return;
        }
        account.keep_on = !account.keep_on;
        let keep_on = account.keep_on;
        let is_registered = matches!(account.access, PlatformAccess::Registered { .. });
        let saved = self.save_platform_sign_in(&token, keep_on).await;
        if let PlatformState::SignedIn(account) = &mut self.platform.view.state {
            account.keep_on_notice = saved.is_err().then(|| KEEP_ON_NOT_SAVED.to_string());
        }
        if keep_on && is_registered && self.remote_control.view().listener.is_none() {
            self.refresh_platform(true).await;
        }
    }

    /// Asks the platform for a sign-in code, then polls until the user
    /// approves or denies it in a browser, or it expires.
    fn start_platform_sign_in(&mut self) {
        let Some(client) = self.platform.client.clone() else {
            return;
        };
        let request_id = self.platform.next_request();
        self.platform.view.state = PlatformState::Requesting;
        let instance_name = self.platform.instance_name.clone();
        let event_tx = self.services.event_sender();
        self.platform.task = Some(spawn::spawn_detached(async move {
            let result = match client.start_device_authorization(&instance_name).await {
                Ok(authorization) => {
                    let _ = event_tx.send(AppEvent::PlatformUpdated {
                        update: PlatformUpdate::Authorized {
                            request_id,
                            authorization: Box::new(authorization.clone()),
                        },
                    });

                    wait_for_sign_in(client.as_ref(), &authorization).await
                }
                Err(error) => Err(error),
            };
            let _ = event_tx.send(AppEvent::PlatformUpdated {
                update: PlatformUpdate::SignInFinished { request_id, result },
            });
        }));
    }

    /// Saves `token` as the sign-in for the configured platform, with
    /// `keep_on`.
    async fn save_platform_sign_in(
        &self,
        token: &AccessToken,
        keep_on: bool,
    ) -> Result<(), DbError> {
        let stored = serde_json::json!({
            "access_token": token.expose(),
            "keep_on": keep_on,
            "platform_url": self.platform.view.platform_url,
        });

        self.services
            .db()
            .settings()
            .upsert_setting(SettingName::PlatformSignIn, &stored.to_string())
            .await
    }

    /// Reads the account in the background and registers this Agentty
    /// when the platform offers a relay. With `starts_remote_control`,
    /// remote control starts once the check confirms the registration.
    async fn refresh_platform(&mut self, starts_remote_control: bool) {
        let (Some(client), Some(token)) =
            (self.platform.client.clone(), self.platform.token.clone())
        else {
            return;
        };
        let request_id = self.platform.next_request();
        self.set_platform_access(PlatformAccess::Checking);
        let instance = match self.remote_instance_key().await {
            Ok(instance_key) => (
                instance_key.public().fingerprint(),
                self.platform.instance_name.clone(),
            ),
            Err(message) => {
                self.set_platform_access(PlatformAccess::Unavailable { message });

                return;
            }
        };
        self.platform.starts_remote_control_after_check = starts_remote_control;
        let event_tx = self.services.event_sender();
        // Kept, so a sign-out or a later check stops it before it registers.
        self.platform.task = Some(spawn::spawn_detached(async move {
            let (login, result) = check_account(client.as_ref(), &token, instance).await;
            let _ = event_tx.send(AppEvent::PlatformUpdated {
                update: PlatformUpdate::Refreshed {
                    request_id,
                    login,
                    result,
                },
            });
        }));
    }

    /// Signs out: forgets the token, stops the platform's relay, and asks
    /// the platform in the background to remove the registration and
    /// revoke the token.
    pub(crate) async fn sign_out_platform(&mut self) {
        let (Some(client), Some(token)) =
            (self.platform.client.clone(), self.platform.token.clone())
        else {
            return;
        };
        let instance_id = self
            .remote_instance_key()
            .await
            .ok()
            .map(|instance_key| instance_key.public().fingerprint());
        // A running account check could register this Agentty again after
        // the removal below, so it stops first.
        let previous = self.platform.task.take();
        if let Some(previous) = &previous {
            previous.abort();
        }
        let request_id = self.forget_platform_sign_in(None).await;
        self.platform.view.state = PlatformState::SigningOut {
            notice: self
                .platform
                .is_saved_sign_in_kept
                .then(|| SAVED_SIGN_IN_KEPT.to_string()),
        };
        let event_tx = self.services.event_sender();
        spawn::spawn_detached(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let removed = match instance_id {
                Some(instance_id) => client.remove_instance(&token, &instance_id).await,
                None => Ok(()),
            };
            let revoked = client.revoke_token(&token).await;
            let _ = event_tx.send(AppEvent::PlatformUpdated {
                update: PlatformUpdate::SignedOut {
                    request_id,
                    result: removed.and(revoked),
                },
            });
        });
    }

    /// Opens the sign-in page in this machine's browser while a sign-in
    /// waits.
    pub(crate) fn open_platform_link(&self) {
        if let Some(link) = self.platform.sign_in_link() {
            self.platform.browser.open(&link);
        }
    }

    /// Copies the sign-in page through the terminal while a sign-in waits.
    pub(crate) fn copy_platform_link(&mut self) {
        if let Some(link) = self.platform.sign_in_link() {
            self.set_platform_link_notice(None);
            self.platform
                .link_copy
                .start(link, self.services.event_sender());
        }
    }

    /// Shows whether the sign-in page copy identified by `request_id`
    /// reached a clipboard.
    pub(crate) fn apply_platform_link_copied(&mut self, request_id: u64, is_copied: bool) {
        if let Some(is_copied) = self.platform.link_copy.finish(request_id, is_copied) {
            let notice = if is_copied {
                LINK_COPIED
            } else {
                LINK_NOT_COPIED
            };
            self.set_platform_link_notice(Some(notice.to_string()));
        }
    }

    /// Applies one background platform update, dropping it when a later
    /// request replaced the one it answers.
    pub(crate) async fn apply_platform_update(&mut self, update: PlatformUpdate) {
        match update {
            PlatformUpdate::Authorized {
                request_id,
                authorization,
            } => {
                if request_id == self.platform.request_id {
                    let authorization = *authorization;
                    self.platform.view.state = PlatformState::SigningIn(PlatformSignInView {
                        link: authorization
                            .verification_uri_complete
                            .unwrap_or(authorization.verification_uri),
                        notice: None,
                        user_code: authorization.user_code,
                    });
                }
            }
            PlatformUpdate::SignInFinished { request_id, result } => {
                if request_id == self.platform.request_id {
                    self.platform.task = None;
                    self.finish_platform_sign_in(result).await;
                }
            }
            PlatformUpdate::Refreshed {
                request_id,
                login,
                result,
            } => {
                if request_id == self.platform.request_id {
                    self.apply_platform_refresh(login, result).await;
                }
            }
            PlatformUpdate::SignedOut { request_id, result } => {
                let is_waiting =
                    matches!(self.platform.view.state, PlatformState::SigningOut { .. });
                if request_id == self.platform.request_id && is_waiting {
                    let notice = match result {
                        Ok(()) | Err(PlatformError::InvalidToken) => "Signed out.".to_string(),
                        Err(error) => format!(
                            "Signed out on this computer, but the platform did not confirm it: \
                             {error}"
                        ),
                    };
                    self.platform.view.state = PlatformState::SignedOut {
                        notice: Some(self.platform.with_kept_sign_in_warning(notice)),
                    };
                }
            }
        }
    }

    /// Saves the token of a finished sign-in with keep on off and checks
    /// the account, whose answer then starts remote control; or shows why
    /// the sign-in ended without a token.
    async fn finish_platform_sign_in(&mut self, result: Result<SignIn, PlatformError>) {
        let sign_in = match result {
            Ok(sign_in) => sign_in,
            Err(error) => {
                let notice = match error {
                    PlatformError::AccessDenied => {
                        "The sign-in was denied in the browser.".to_string()
                    }
                    PlatformError::ExpiredToken => "The sign-in code expired before it was \
                                                    approved. Press Enter to try again."
                        .to_string(),
                    error => format!("Sign-in failed: {error}"),
                };
                self.platform.view.state = PlatformState::SignedOut {
                    notice: Some(notice),
                };

                return;
            }
        };
        let saved = self
            .save_platform_sign_in(&sign_in.access_token, false)
            .await;
        self.platform.token = Some(sign_in.access_token);
        self.platform.view.state =
            PlatformState::SignedIn(PlatformAccountView::checking(Some(sign_in.login)));
        if saved.is_err() {
            // Checking the account now would hide this until the next check.
            self.set_platform_access(PlatformAccess::Unavailable {
                message: "Could not save the sign-in, so it lasts only until Agentty quits. Press \
                          Enter to use it anyway."
                    .to_string(),
            });
            // The `Enter` that uses it anyway runs the sign-in's check.
            self.platform.connects_on_next_check = true;

            return;
        }
        // Only `Enter` starts a sign-in, so it ends connected.
        self.refresh_platform(true).await;
    }

    /// Shows the checked account, hands its relay, if any, to remote
    /// control, and starts remote control when the check was to connect,
    /// this Agentty is registered, and remote control is off.
    async fn apply_platform_refresh(
        &mut self,
        login: Option<String>,
        result: Result<Access, PlatformError>,
    ) {
        if let (Some(login), PlatformState::SignedIn(account)) =
            (login, &mut self.platform.view.state)
        {
            account.login = Some(login);
        }
        let (access, relay) = match result {
            Ok(Access::NoRelay) => (PlatformAccess::NoRelay, None),
            Ok(Access::Registered { name, service }) => {
                (PlatformAccess::Registered { name }, Some(service))
            }
            Err(PlatformError::InvalidToken) => {
                self.forget_platform_sign_in(Some(SIGNED_OUT_BY_PLATFORM.to_string()))
                    .await;

                return;
            }
            Err(PlatformError::Waitlisted) => (PlatformAccess::Waitlisted, None),
            Err(error) => (
                PlatformAccess::Unavailable {
                    message: error.to_string(),
                },
                None,
            ),
        };
        let starts_remote_control =
            std::mem::take(&mut self.platform.starts_remote_control_after_check);
        let is_registered = relay.is_some();
        self.set_platform_access(access);
        self.set_platform_relay(relay).await;
        let is_running = self.remote_control.view().listener.is_some();
        if starts_remote_control && is_registered && !is_running {
            self.start_remote_control().await;
        }
    }

    /// Forgets the token and keep on here, stops remote control through
    /// the platform, and shows `notice`. Returns the request a sign-out
    /// reports under.
    async fn forget_platform_sign_in(&mut self, notice: Option<String>) -> u64 {
        let request_id = self.platform.next_request();
        self.platform.token = None;
        // An empty value leaves no token, and no keep on, behind.
        let cleared = self
            .services
            .db()
            .settings()
            .upsert_setting(SettingName::PlatformSignIn, "")
            .await;
        self.platform.is_saved_sign_in_kept = cleared.is_err();
        self.platform.view.state = PlatformState::SignedOut {
            notice: notice.map(|notice| self.platform.with_kept_sign_in_warning(notice)),
        };
        self.set_platform_relay(None).await;

        request_id
    }

    /// Loads the sign-in saved for the configured platform, ignoring one
    /// another platform issued together with its keep on.
    async fn stored_platform_sign_in(&self) -> Option<StoredSignIn> {
        let stored = self
            .services
            .db()
            .settings()
            .get_setting(SettingName::PlatformSignIn)
            .await
            .ok()
            .flatten()?;
        let stored: StoredSignIn = serde_json::from_str(&stored).ok()?;
        let is_this_platform = stored.platform_url == self.platform.view.platform_url;

        (is_this_platform && !stored.access_token.is_empty()).then_some(stored)
    }

    /// Replaces what the signed-in account may use.
    fn set_platform_access(&mut self, access: PlatformAccess) {
        if let PlatformState::SignedIn(account) = &mut self.platform.view.state {
            account.access = access;
        }
    }

    /// Replaces the notice shown under a waiting sign-in.
    fn set_platform_link_notice(&mut self, notice: Option<String>) {
        if let PlatformState::SigningIn(sign_in) = &mut self.platform.view.state {
            sign_in.notice = notice;
        }
    }
}

/// Polls for the token of `authorization` no faster than the platform
/// allows, until the user decides or the code expires. The lifetime counts
/// requests as well as waits.
pub(crate) async fn wait_for_sign_in(
    client: &dyn PlatformClient,
    authorization: &DeviceAuthorization,
) -> Result<SignIn, PlatformError> {
    timeout(
        authorization.expires_in,
        poll_until_decided(client, authorization),
    )
    .await
    .unwrap_or(Err(PlatformError::ExpiredToken))
}

/// Polls for the token of `authorization` until the user decides, waiting
/// the platform's interval, 5 seconds more after each `slow_down`, and at
/// least as long as a `Retry-After` asks.
async fn poll_until_decided(
    client: &dyn PlatformClient,
    authorization: &DeviceAuthorization,
) -> Result<SignIn, PlatformError> {
    let mut interval = authorization.interval;
    let mut wait = interval;

    loop {
        sleep(wait).await;
        wait = interval;
        match client.poll_device_token(&authorization.device_code).await {
            Ok(sign_in) => return Ok(sign_in),
            // A dropped connection may recover before the code expires.
            Err(PlatformError::AuthorizationPending | PlatformError::Unreachable) => {}
            Err(PlatformError::SlowDown) => {
                interval += SLOW_DOWN_STEP;
                wait = interval;
            }
            Err(PlatformError::RateLimited { retry_after }) => {
                interval += SLOW_DOWN_STEP;
                wait = retry_after.map_or(interval, |retry_after| retry_after.max(interval));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Reads the account behind `token` and, when the platform offers a relay,
/// registers this Agentty under the identifier and name `instance` gives.
/// Returns the login when the account could be read.
pub(crate) async fn check_account(
    client: &dyn PlatformClient,
    token: &AccessToken,
    instance: (String, String),
) -> (Option<String>, Result<Access, PlatformError>) {
    let account = match client.account(token).await {
        Ok(account) => account,
        Err(error) => return (None, Err(error)),
    };
    let (instance_id, name) = instance;
    let result = match account.remote_control {
        None => Ok(Access::NoRelay),
        Some(service) => client
            .register_instance(token, &instance_id, &name)
            .await
            .map(|registered| Access::Registered {
                name: registered.name,
                service,
            }),
    };

    (Some(account.login), result)
}

#[cfg(test)]
#[path = "platform_test.rs"]
mod tests;

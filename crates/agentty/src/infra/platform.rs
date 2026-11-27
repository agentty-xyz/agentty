//! Agentty Platform boundary: device sign-in, account and service
//! discovery, instance registration, and the tasks of the platform's Sync
//! over version 1 of the platform's HTTP API.
//!
//! The client talks to whichever platform origin it is given. Every answer
//! is untrusted: text the terminal shows is stripped of control characters
//! and bounded, and addresses Agentty opens or links to must be web or
//! `WebSocket` URLs. Access tokens, device codes, user codes, and Sync
//! claims never appear in `Debug` output, and nothing here logs.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::RETRY_AFTER;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::domain::platform_sync;
use crate::domain::remote_url::{parse_page_url, parse_relay_url};

/// Platform origin used unless `--platform-url` names another.
pub const DEFAULT_PLATFORM_URL: &str = "https://platform.agentty.xyz";
/// Longest a platform request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest response body read from the platform, in bytes.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// Longest login, instance name, or error message kept, in characters.
const MAX_TEXT_CHARS: usize = 128;
/// Longest user code accepted, in characters.
const MAX_USER_CODE_CHARS: usize = 32;
/// Polling interval used when the platform names none, as RFC 8628 says.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Shortest polling interval honoured, so a platform cannot make Agentty
/// poll in a tight loop.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Longest polling interval honoured, so a sign-in stays responsive.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Longest a sign-in may stay open, whatever the platform says.
const MAX_EXPIRES_IN: Duration = Duration::from_secs(3600);
/// Characters in an instance identifier: a hexadecimal SHA-256 digest.
const INSTANCE_ID_CHARS: usize = 64;
/// Longest Sync project name accepted, in characters.
const MAX_SYNC_PROJECT_CHARS: usize = 100;
/// Longest Sync claim accepted, in characters.
const MAX_SYNC_CLAIM_CHARS: usize = 256;
/// Most other tasks in progress kept from one start answer.
const MAX_SYNC_IN_PROGRESS: usize = 20;

/// Calls one platform on behalf of this Agentty.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait PlatformClient: Send + Sync {
    /// Starts a device sign-in that the user approves in a browser, naming
    /// this Agentty `client_name` there.
    ///
    /// # Errors
    /// Returns a [`PlatformError`] when the platform refuses or cannot be
    /// reached.
    async fn start_device_authorization(
        &self,
        client_name: &str,
    ) -> Result<DeviceAuthorization, PlatformError>;

    /// Asks once whether the user approved the sign-in of `device_code`.
    ///
    /// # Errors
    /// Returns [`PlatformError::AuthorizationPending`] or
    /// [`PlatformError::SlowDown`] while the user has not decided yet,
    /// [`PlatformError::AccessDenied`] or [`PlatformError::ExpiredToken`]
    /// when the sign-in ended without a token, and other variants when the
    /// platform fails.
    async fn poll_device_token(&self, device_code: &DeviceCode) -> Result<SignIn, PlatformError>;

    /// Reads the signed-in account and the services the platform offers it.
    ///
    /// # Errors
    /// Returns [`PlatformError::InvalidToken`] for a revoked or unknown
    /// token and [`PlatformError::Waitlisted`] for an account the platform
    /// does not admit yet.
    async fn account(&self, token: &AccessToken) -> Result<PlatformAccount, PlatformError>;

    /// Registers the instance `instance_id` under the account as `name`, or
    /// renames it. The platform's relay admits registered instances.
    ///
    /// # Errors
    /// Returns [`PlatformError::InstanceLimit`] when the account holds too
    /// many instances, and the account errors of [`Self::account`].
    async fn register_instance(
        &self,
        token: &AccessToken,
        instance_id: &str,
        name: &str,
    ) -> Result<RegisteredInstance, PlatformError>;

    /// Removes the instance `instance_id` from the account; succeeds when
    /// it was not registered.
    ///
    /// # Errors
    /// Returns a [`PlatformError`] when the platform refuses or cannot be
    /// reached.
    async fn remove_instance(
        &self,
        token: &AccessToken,
        instance_id: &str,
    ) -> Result<(), PlatformError>;

    /// Revokes `token`.
    ///
    /// # Errors
    /// Returns a [`PlatformError`] when the platform refuses or cannot be
    /// reached.
    async fn revoke_token(&self, token: &AccessToken) -> Result<(), PlatformError>;

    /// Asks the platform's Sync whether `work` exists, and takes the task
    /// for the session when no other session holds it.
    ///
    /// # Errors
    /// Returns [`SyncError::TaskInProgress`] when another session holds the
    /// work, [`SyncError::SyncDisabled`] when the platform does not offer
    /// Sync to the account, and [`SyncError::Failed`] otherwise, including
    /// for `work` the contract does not allow, which is never sent.
    async fn start_sync_work(
        &self,
        token: &AccessToken,
        work: &SyncWork,
    ) -> Result<SyncStart, SyncError>;

    /// Reports that the session holding `task` with `claim_id` is active.
    ///
    /// # Errors
    /// Returns [`SyncError::ClaimLost`] or [`SyncError::TaskClosed`] when
    /// the session no longer holds the task, and the errors of
    /// [`Self::start_sync_work`].
    async fn report_sync_activity(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
    ) -> Result<(), SyncError>;

    /// Finishes `task` with `summary` and, when there is one, the
    /// session's review request at `result_url`.
    ///
    /// # Errors
    /// Returns the errors of [`Self::report_sync_activity`].
    async fn finish_sync_task(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
        summary: &str,
        result_url: Option<String>,
    ) -> Result<(), SyncError>;

    /// Gives `task` up, as a session deleted or canceled before it is done
    /// does.
    ///
    /// # Errors
    /// Returns the errors of [`Self::report_sync_activity`].
    async fn release_sync_task(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
    ) -> Result<(), SyncError>;
}

/// Bearer token that signs this Agentty in to a platform. Its `Debug` form
/// hides the value.
#[derive(Clone, Eq, PartialEq)]
pub struct AccessToken(String);

impl AccessToken {
    /// Wraps a token the platform issued or that was stored earlier.
    pub fn new(token: String) -> Self {
        Self(token)
    }

    /// Returns the token for storage or an `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(<redacted>)")
    }
}

/// Secret code that identifies one device sign-in while it is polled. Its
/// `Debug` form hides the value.
#[derive(Clone, Eq, PartialEq)]
pub struct DeviceCode(String);

impl DeviceCode {
    /// Wraps a device code the platform issued.
    pub fn new(device_code: String) -> Self {
        Self(device_code)
    }
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceCode(<redacted>)")
    }
}

/// One device sign-in the user approves in a browser.
#[derive(Clone, Eq, PartialEq)]
pub struct DeviceAuthorization {
    /// Code Agentty polls with.
    pub device_code: DeviceCode,
    /// Time after which the sign-in expires, at most an hour.
    pub expires_in: Duration,
    /// Shortest wait between polls, from one second to a minute.
    pub interval: Duration,
    /// Code the user confirms in the browser.
    pub user_code: String,
    /// Page where the user enters [`Self::user_code`].
    pub verification_uri: String,
    /// Page that carries the user code already, when the platform offers
    /// one.
    pub verification_uri_complete: Option<String>,
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceAuthorization")
            .field("device_code", &self.device_code)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            // Either page may carry the user code in its query.
            .field("user_code", &"<redacted>")
            .field("verification_uri", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Result of an approved device sign-in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignIn {
    /// Token for later requests.
    pub access_token: AccessToken,
    /// Login of the account that approved the sign-in.
    pub login: String,
}

/// The signed-in account and what the platform offers it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformAccount {
    /// Account login.
    pub login: String,
    /// Hosted remote control, when the platform offers a relay.
    pub remote_control: Option<RemoteControlService>,
    /// Whether the platform has Sync enabled for the account.
    pub sync: bool,
}

/// Proof that a session holds a Sync task. It goes only to the platform
/// that issued it, and its `Debug` form hides the value.
#[derive(Clone, Eq, PartialEq)]
pub struct ClaimId(String);

impl ClaimId {
    /// Wraps a claim the platform issued or that was stored earlier.
    pub fn new(claim_id: String) -> Self {
        Self(claim_id)
    }

    /// Returns the claim for storage or a request body.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClaimId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClaimId(<redacted>)")
    }
}

/// Work a session's first message describes, as a start request sends it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncWork {
    /// The message, cut to 4,000 characters.
    pub brief: String,
    /// Task numbers the user declared to be other work.
    pub not_duplicates: Vec<u64>,
    /// The repository's `origin` remote as `host/path`.
    pub repository: String,
    /// Label of the session for the account's other sessions.
    pub session: String,
    /// Task the session takes over from the session holding it.
    pub takeover: Option<u64>,
    /// The message's first line that is not blank, cut to
    /// 200 characters.
    pub title: String,
}

impl SyncWork {
    /// Returns whether every field fits the platform contract; anything
    /// else is never sent.
    fn is_valid(&self) -> bool {
        platform_sync::is_sync_repository(&self.repository)
            && (1..=platform_sync::MAX_TITLE_CHARS).contains(&self.title.chars().count())
            && self.brief.chars().count() <= platform_sync::MAX_BRIEF_CHARS
            && (1..=platform_sync::MAX_SESSION_CHARS).contains(&self.session.chars().count())
    }
}

/// A Sync task, as an answer names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncTask {
    /// Task number within its project.
    pub number: u64,
    /// Project of the task, safe in a request path.
    pub project: String,
    /// Task title, fit for the terminal.
    pub title: String,
    /// Task page, kept only when it is one of the platform's own pages.
    pub url: Option<String>,
}

impl SyncTask {
    /// Returns the reference later requests about the task use.
    pub fn reference(&self) -> SyncTaskRef {
        SyncTaskRef {
            number: self.number,
            project: self.project.clone(),
        }
    }
}

/// The project and number later requests about a task name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncTaskRef {
    /// Task number within its project.
    pub number: u64,
    /// Project of the task.
    pub project: String,
}

/// Answer to a start request: the task the session now holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncStart {
    /// Proof that the session holds the task.
    pub claim_id: ClaimId,
    /// Whether the platform created the task, rather than handing the
    /// session an open task that said the same thing.
    pub created: bool,
    /// The repository's other work that sessions hold, latest activity
    /// first, at most 20 tasks.
    pub in_progress: Vec<SyncWorkInProgress>,
    /// Task the session holds.
    pub task: SyncTask,
}

/// Another session's task in progress in the same repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncWorkInProgress {
    /// Last activity of the holding session, in milliseconds since the
    /// Unix epoch.
    pub last_activity_at_ms: u64,
    /// Task number within its project.
    pub number: u64,
    /// Label of the holding session, fit for the terminal, when the
    /// holding client gave one.
    pub session: Option<String>,
    /// Task title, fit for the terminal.
    pub title: String,
}

/// The session that holds work another session asked to start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncHolder {
    /// Last activity of the holding session, in milliseconds since the
    /// Unix epoch.
    pub last_activity_at_ms: u64,
    /// Label of the holding session, fit for the terminal, when the
    /// holding client gave one.
    pub session: Option<String>,
}

/// Why a Sync request did not succeed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SyncError {
    /// Another session holds the work.
    #[error("Another session holds task #{}.", task.number)]
    TaskInProgress {
        /// The session holding it.
        holder: SyncHolder,
        /// The task it holds.
        task: SyncTask,
    },
    /// Another session took the task over.
    #[error("Another session took the task over.")]
    ClaimLost,
    /// The task was finished or canceled elsewhere.
    #[error("The task was finished or canceled elsewhere.")]
    TaskClosed,
    /// The platform does not have Sync enabled for the account.
    #[error("The platform does not offer Sync to this account.")]
    SyncDisabled,
    /// Any other failure, a timeout included.
    #[error("{0}")]
    Failed(PlatformError),
}

/// Hosted remote control that a platform offers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteControlService {
    /// Page a pairing link opens, in place of the relay's own page.
    pub pairing_url: String,
    /// Relay registered instances connect to, such as
    /// `wss://relay.example`.
    pub relay_url: String,
}

/// An instance as the platform registered it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredInstance {
    /// Identifier the relay routes by.
    pub instance_id: String,
    /// Name the platform shows for the instance.
    pub name: String,
}

/// Why a platform request did not succeed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlatformError {
    /// The user has not approved or denied the sign-in yet.
    #[error("The sign-in is waiting for approval.")]
    AuthorizationPending,
    /// The user denied the sign-in.
    #[error("The sign-in was denied.")]
    AccessDenied,
    /// The sign-in expired before the user approved it.
    #[error("The sign-in code expired.")]
    ExpiredToken,
    /// The account holds as many instances as the platform allows.
    #[error("This account has as many instances registered as the platform allows.")]
    InstanceLimit,
    /// The instance identifier is not a hexadecimal SHA-256 digest, so no
    /// request was sent.
    #[error("This Agentty's instance identifier is malformed.")]
    InvalidInstance,
    /// A value of the request does not fit the platform contract, so no
    /// request was sent.
    #[error("The request does not fit what the platform accepts, so it was not sent.")]
    InvalidRequest,
    /// The answer was not what the platform contract describes.
    #[error("The platform sent an answer Agentty does not understand.")]
    InvalidResponse,
    /// The token was revoked or never existed.
    #[error("The platform no longer accepts this sign-in.")]
    InvalidToken,
    /// The platform refused the request with another error.
    #[error("The platform refused the request ({status}){}", message_suffix(message.as_deref()))]
    Rejected {
        /// Safe text the platform sent, when any.
        message: Option<String>,
        /// HTTP status code.
        status: u16,
    },
    /// The platform asks for fewer requests.
    #[error("The platform asks Agentty to wait before trying again.")]
    RateLimited {
        /// Wait the platform asked for, when it named one.
        retry_after: Option<Duration>,
    },
    /// The user has not decided yet and Agentty should poll less often.
    #[error("The platform asks Agentty to poll less often.")]
    SlowDown,
    /// The platform could not be reached or did not answer in time.
    #[error("Could not reach the platform.")]
    Unreachable,
    /// The account is on the platform's waitlist and has no access yet.
    #[error("This account is on the platform's waitlist and has no access yet.")]
    Waitlisted,
}

/// Formats a platform message after the status in
/// [`PlatformError::Rejected`].
fn message_suffix(message: Option<&str>) -> String {
    message.map_or_else(|| ".".to_string(), |message| format!(": {message}"))
}

/// [`PlatformClient`] over HTTPS.
pub struct HttpPlatformClient {
    base_url: String,
    http: Client,
}

impl HttpPlatformClient {
    /// Builds a client for the platform at `base_url`, which
    /// [`crate::domain::remote_url::parse_platform_url`] accepted.
    ///
    /// # Errors
    /// Returns [`PlatformError::Unreachable`] when no HTTP client can be
    /// built.
    pub fn new(base_url: &str) -> Result<Self, PlatformError> {
        // Idempotent: TLS connections need a process-wide provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let builder = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("agentty/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none());
        let http = builder.build().map_err(|_| PlatformError::Unreachable)?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Starts a request to `path` on the platform.
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
    }

    /// Starts a request to `path` that presents `token`.
    fn authorized(&self, method: Method, path: &str, token: &AccessToken) -> RequestBuilder {
        // Marks the header sensitive, so it never shows in `Debug` output.
        self.request(method, path).bearer_auth(token.expose())
    }

    /// Sends `request` and decodes a successful JSON answer.
    async fn fetch<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, PlatformError> {
        let body = send(request).await?;

        serde_json::from_slice(&body).map_err(|_| PlatformError::InvalidResponse)
    }

    /// Sends a Sync `request` and returns the body of a successful answer,
    /// or the Sync error an unsuccessful one describes.
    async fn send_sync(&self, request: RequestBuilder) -> Result<Vec<u8>, SyncError> {
        let (status, retry_after, body) = send_raw(request).await.map_err(SyncError::Failed)?;
        if status.is_success() {
            return Ok(body);
        }

        Err(sync_error_for(status, &body, retry_after, &self.base_url))
    }
}

#[async_trait]
impl PlatformClient for HttpPlatformClient {
    async fn start_device_authorization(
        &self,
        client_name: &str,
    ) -> Result<DeviceAuthorization, PlatformError> {
        let request = self
            .request(Method::POST, "/api/v1/device/authorizations")
            .json(&serde_json::json!({ "client_name": client_name }));
        let answer: DeviceAuthorizationBody = self.fetch(request).await?;

        answer.validate()
    }

    async fn poll_device_token(&self, device_code: &DeviceCode) -> Result<SignIn, PlatformError> {
        let request = self
            .request(Method::POST, "/api/v1/device/token")
            .json(&serde_json::json!({ "device_code": device_code.0 }));
        let answer: DeviceTokenBody = self.fetch(request).await?;
        if answer.access_token.is_empty() {
            return Err(PlatformError::InvalidResponse);
        }

        Ok(SignIn {
            access_token: AccessToken::new(answer.access_token),
            login: answer.account.validated_login()?,
        })
    }

    async fn account(&self, token: &AccessToken) -> Result<PlatformAccount, PlatformError> {
        let request = self.authorized(Method::GET, "/api/v1/account", token);
        let answer: AccountBody = self.fetch(request).await?;
        let remote_control = answer
            .services
            .remote_control
            .map(|service| {
                Ok(RemoteControlService {
                    pairing_url: parse_page_url(&service.pairing_url)
                        .map_err(|_| PlatformError::InvalidResponse)?,
                    relay_url: parse_relay_url(&service.relay_url)
                        .map_err(|_| PlatformError::InvalidResponse)?,
                })
            })
            .transpose()?;

        Ok(PlatformAccount {
            login: answer.account.validated_login()?,
            remote_control,
            sync: matches!(answer.services.sync, Some(serde_json::Value::Object(_))),
        })
    }

    async fn register_instance(
        &self,
        token: &AccessToken,
        instance_id: &str,
        name: &str,
    ) -> Result<RegisteredInstance, PlatformError> {
        let request = self
            .authorized(Method::PUT, &instance_path(instance_id)?, token)
            .json(&serde_json::json!({ "name": name }));
        let answer: InstanceBody = self.fetch(request).await?;
        let name = display_text(&answer.name).ok_or(PlatformError::InvalidResponse)?;
        if answer.instance_id != instance_id {
            return Err(PlatformError::InvalidResponse);
        }

        Ok(RegisteredInstance {
            instance_id: answer.instance_id,
            name,
        })
    }

    async fn remove_instance(
        &self,
        token: &AccessToken,
        instance_id: &str,
    ) -> Result<(), PlatformError> {
        let request = self.authorized(Method::DELETE, &instance_path(instance_id)?, token);
        send(request).await?;

        Ok(())
    }

    async fn revoke_token(&self, token: &AccessToken) -> Result<(), PlatformError> {
        send(self.authorized(Method::DELETE, "/api/v1/token", token)).await?;

        Ok(())
    }

    async fn start_sync_work(
        &self,
        token: &AccessToken,
        work: &SyncWork,
    ) -> Result<SyncStart, SyncError> {
        if !work.is_valid() {
            return Err(SyncError::Failed(PlatformError::InvalidRequest));
        }
        let mut body = serde_json::json!({
            "brief": work.brief,
            "repository": work.repository,
            "session": work.session,
            "title": work.title,
        });
        if let Some(takeover) = work.takeover {
            body["takeover"] = serde_json::json!(takeover);
        }
        if !work.not_duplicates.is_empty() {
            body["not_duplicates"] = serde_json::json!(work.not_duplicates);
        }
        let request = self
            .authorized(Method::POST, "/api/v1/sync/work", token)
            .json(&body);
        let body = self.send_sync(request).await?;
        let answer: SyncStartBody =
            serde_json::from_slice(&body).map_err(|_| invalid_sync_response())?;

        answer.validate(&self.base_url)
    }

    async fn report_sync_activity(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
    ) -> Result<(), SyncError> {
        let request = self
            .authorized(Method::POST, &sync_task_path(task, "activity")?, token)
            .json(&serde_json::json!({ "claim_id": claim_id.expose() }));
        self.send_sync(request).await?;

        Ok(())
    }

    async fn finish_sync_task(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
        summary: &str,
        result_url: Option<String>,
    ) -> Result<(), SyncError> {
        let summary_chars = summary.chars().count();
        if !(1..=platform_sync::MAX_SUMMARY_CHARS).contains(&summary_chars) {
            return Err(SyncError::Failed(PlatformError::InvalidRequest));
        }
        let mut body = serde_json::json!({ "claim_id": claim_id.expose(), "summary": summary });
        if let Some(result_url) = result_url {
            body["result_url"] = serde_json::json!(result_url);
        }
        let request = self
            .authorized(Method::POST, &sync_task_path(task, "finish")?, token)
            .json(&body);
        self.send_sync(request).await?;

        Ok(())
    }

    async fn release_sync_task(
        &self,
        token: &AccessToken,
        task: &SyncTaskRef,
        claim_id: &ClaimId,
    ) -> Result<(), SyncError> {
        let request = self
            .authorized(Method::POST, &sync_task_path(task, "release")?, token)
            .json(&serde_json::json!({ "claim_id": claim_id.expose() }));
        self.send_sync(request).await?;

        Ok(())
    }
}

/// Returns the API path of `route` for `task`. A project an answer named
/// could change the path a token is sent to unless it is a plain name, so
/// anything else is refused before any request.
fn sync_task_path(task: &SyncTaskRef, route: &str) -> Result<String, SyncError> {
    if !is_sync_project(&task.project) {
        return Err(SyncError::Failed(PlatformError::InvalidRequest));
    }

    Ok(format!(
        "/api/v1/sync/projects/{}/tasks/{}/{route}",
        task.project, task.number
    ))
}

/// Returns whether `project` is a name safe in a request path.
fn is_sync_project(project: &str) -> bool {
    !project.is_empty()
        && project.chars().count() <= MAX_SYNC_PROJECT_CHARS
        && !matches!(project, "." | "..")
        && project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
}

/// Returns the error for a Sync answer Agentty does not understand.
fn invalid_sync_response() -> SyncError {
    SyncError::Failed(PlatformError::InvalidResponse)
}

/// Maps an unsuccessful Sync answer to the error its status and `error`
/// code describe; a conflict that does not name its task and holder is
/// one Agentty does not understand.
fn sync_error_for(
    status: StatusCode,
    body: &[u8],
    retry_after: Option<Duration>,
    base_url: &str,
) -> SyncError {
    let answer: SyncErrorBody = serde_json::from_slice(body).unwrap_or_default();
    match (status, answer.error.as_str()) {
        (StatusCode::CONFLICT, "task_in_progress") => {
            let (Some(task), Some(holder)) = (answer.task, answer.holder) else {
                return invalid_sync_response();
            };
            let Ok(task) = task.validate(base_url) else {
                return invalid_sync_response();
            };

            SyncError::TaskInProgress {
                holder: SyncHolder {
                    last_activity_at_ms: holder.last_activity_at_ms,
                    session: session_label_text(holder.session.as_deref()),
                },
                task,
            }
        }
        (StatusCode::CONFLICT, "claim_lost") => SyncError::ClaimLost,
        (StatusCode::CONFLICT, "task_closed") => SyncError::TaskClosed,
        (StatusCode::FORBIDDEN, "sync_disabled") => SyncError::SyncDisabled,
        _ => SyncError::Failed(error_for(status, body, retry_after)),
    }
}

/// Returns a session label from an answer fit for the terminal, or `None`
/// for a missing label or one with nothing printable.
fn session_label_text(session: Option<&str>) -> Option<String> {
    session.and_then(|session| sync_text(session, platform_sync::MAX_SESSION_CHARS))
}

/// Returns `text` without control characters and cut to `max_chars`, or
/// `None` when nothing printable is left.
fn sync_text(text: &str, max_chars: usize) -> Option<String> {
    let text: String = text
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let text: String = text.trim().chars().take(max_chars).collect();
    let text = text.trim_end();

    (!text.is_empty()).then(|| text.to_string())
}

/// Returns the API path of the instance `instance_id`, the hexadecimal
/// fingerprint of its relay key. Anything else could change the path a
/// token is sent to, so it is refused before any request.
fn instance_path(instance_id: &str) -> Result<String, PlatformError> {
    let is_fingerprint = instance_id.len() == INSTANCE_ID_CHARS
        && instance_id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if !is_fingerprint {
        return Err(PlatformError::InvalidInstance);
    }

    Ok(format!("/api/v1/instances/{instance_id}"))
}

/// Sends `request` and returns the body of a successful answer, or the
/// error an unsuccessful one describes.
async fn send(request: RequestBuilder) -> Result<Vec<u8>, PlatformError> {
    let (status, retry_after, body) = send_raw(request).await?;
    if status.is_success() {
        return Ok(body);
    }

    Err(error_for(status, &body, retry_after))
}

/// Sends `request` and returns the status, the wait a `Retry-After` asks
/// for, and the body of any answer; fails only when no answer arrives or
/// it is too large.
async fn send_raw(
    request: RequestBuilder,
) -> Result<(StatusCode, Option<Duration>, Vec<u8>), PlatformError> {
    let response = request
        .send()
        .await
        .map_err(|_| PlatformError::Unreachable)?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = read_body(response).await?;

    Ok((status, retry_after, body))
}

/// Reads at most [`MAX_RESPONSE_BYTES`] of `response`, failing on a larger
/// body.
async fn read_body(mut response: Response) -> Result<Vec<u8>, PlatformError> {
    let mut body = Vec::new();
    loop {
        let next = response.chunk().await;
        let Some(chunk) = next.map_err(|_| PlatformError::Unreachable)? else {
            break;
        };
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(PlatformError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// Maps an unsuccessful answer to the error its status and `error` code
/// describe.
fn error_for(status: StatusCode, body: &[u8], retry_after: Option<Duration>) -> PlatformError {
    let answer: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
    match (status, answer.error.as_str()) {
        (StatusCode::UNAUTHORIZED, _) => PlatformError::InvalidToken,
        (StatusCode::TOO_MANY_REQUESTS, _) => PlatformError::RateLimited { retry_after },
        (StatusCode::FORBIDDEN, "waitlisted") => PlatformError::Waitlisted,
        (StatusCode::FORBIDDEN, "instance_limit") => PlatformError::InstanceLimit,
        (StatusCode::BAD_REQUEST, "authorization_pending") => PlatformError::AuthorizationPending,
        (StatusCode::BAD_REQUEST, "slow_down") => PlatformError::SlowDown,
        (StatusCode::BAD_REQUEST, "access_denied") => PlatformError::AccessDenied,
        (StatusCode::BAD_REQUEST, "expired_token") => PlatformError::ExpiredToken,
        _ => PlatformError::Rejected {
            message: answer.message.as_deref().and_then(display_text),
            status: status.as_u16(),
        },
    }
}

/// Returns `text` without control characters and cut to
/// [`MAX_TEXT_CHARS`], or `None` when nothing printable is left.
fn display_text(text: &str) -> Option<String> {
    let text: String = text
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_TEXT_CHARS)
        .collect();
    let text = text.trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// Body of an error answer.
#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: Option<String>,
}

/// Body of `POST /api/v1/device/authorizations`.
#[derive(Deserialize)]
struct DeviceAuthorizationBody {
    device_code: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
}

impl DeviceAuthorizationBody {
    /// Checks every field the terminal shows or the browser opens.
    fn validate(self) -> Result<DeviceAuthorization, PlatformError> {
        let is_user_code = !self.user_code.is_empty()
            && self.user_code.chars().count() <= MAX_USER_CODE_CHARS
            && self
                .user_code
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-');
        if !is_user_code || self.device_code.is_empty() || self.expires_in == 0 {
            return Err(PlatformError::InvalidResponse);
        }
        let page = |url: &str| parse_page_url(url).map_err(|_| PlatformError::InvalidResponse);
        let verification_uri = page(&self.verification_uri)?;
        let verification_uri_complete = self
            .verification_uri_complete
            .as_deref()
            .map(page)
            .transpose()?;
        let interval = self
            .interval
            .map_or(DEFAULT_POLL_INTERVAL, Duration::from_secs)
            .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL);

        Ok(DeviceAuthorization {
            device_code: DeviceCode::new(self.device_code),
            expires_in: Duration::from_secs(self.expires_in).min(MAX_EXPIRES_IN),
            interval,
            user_code: self.user_code,
            verification_uri,
            verification_uri_complete,
        })
    }
}

/// Body of a successful `POST /api/v1/device/token`.
#[derive(Deserialize)]
struct DeviceTokenBody {
    access_token: String,
    account: AccountField,
}

/// The `account` object of several answers.
#[derive(Deserialize)]
struct AccountField {
    login: String,
}

impl AccountField {
    /// Returns the login fit for the terminal.
    fn validated_login(&self) -> Result<String, PlatformError> {
        display_text(&self.login).ok_or(PlatformError::InvalidResponse)
    }
}

/// Body of `GET /api/v1/account`.
#[derive(Deserialize)]
struct AccountBody {
    account: AccountField,
    #[serde(default)]
    services: ServicesField,
}

/// The `services` object of the account answer.
#[derive(Default, Deserialize)]
struct ServicesField {
    #[serde(default)]
    remote_control: Option<RemoteControlField>,
    /// Present, as an object Agentty reads nothing from, while the
    /// platform has Sync enabled for the account.
    #[serde(default)]
    sync: Option<serde_json::Value>,
}

/// The `services.remote_control` object of the account answer.
#[derive(Deserialize)]
struct RemoteControlField {
    pairing_url: String,
    relay_url: String,
}

/// Body of `PUT /api/v1/instances/{instance_id}`.
#[derive(Deserialize)]
struct InstanceBody {
    instance_id: String,
    name: String,
}

/// Body of a successful `POST /api/v1/sync/work`.
#[derive(Deserialize)]
struct SyncStartBody {
    claim_id: String,
    #[serde(default)]
    created: bool,
    #[serde(default)]
    in_progress: Vec<SyncInProgressField>,
    task: SyncTaskField,
}

impl SyncStartBody {
    /// Checks the claim and the task, and keeps the other tasks in
    /// progress whose text the terminal can show.
    fn validate(self, base_url: &str) -> Result<SyncStart, SyncError> {
        let is_claim = !self.claim_id.is_empty()
            && self.claim_id.chars().count() <= MAX_SYNC_CLAIM_CHARS
            && self
                .claim_id
                .chars()
                .all(|character| !character.is_control());
        if !is_claim {
            return Err(invalid_sync_response());
        }
        let in_progress = self
            .in_progress
            .into_iter()
            .filter_map(|work| {
                Some(SyncWorkInProgress {
                    last_activity_at_ms: work.last_activity_at_ms,
                    number: work.number,
                    session: session_label_text(work.session.as_deref()),
                    title: sync_text(&work.title, platform_sync::MAX_TITLE_CHARS)?,
                })
            })
            .take(MAX_SYNC_IN_PROGRESS)
            .collect();

        Ok(SyncStart {
            claim_id: ClaimId::new(self.claim_id),
            created: self.created,
            in_progress,
            task: self.task.validate(base_url)?,
        })
    }
}

/// The `task` object of Sync answers.
#[derive(Deserialize)]
struct SyncTaskField {
    number: u64,
    project: String,
    title: String,
    #[serde(default)]
    url: Option<String>,
}

impl SyncTaskField {
    /// Checks that the project is safe in a request path and the title fit
    /// for the terminal, and keeps the page only when it is one of the
    /// platform's own.
    fn validate(self, base_url: &str) -> Result<SyncTask, SyncError> {
        let title = sync_text(&self.title, platform_sync::MAX_TITLE_CHARS);
        let (true, Some(title)) = (is_sync_project(&self.project) && self.number > 0, title) else {
            return Err(invalid_sync_response());
        };
        let url = self
            .url
            .and_then(|url| parse_page_url(&url).ok())
            .filter(|url| url.starts_with(&format!("{base_url}/")));

        Ok(SyncTask {
            number: self.number,
            project: self.project,
            title,
            url,
        })
    }
}

/// One entry of the `in_progress` list of a start answer.
#[derive(Deserialize)]
struct SyncInProgressField {
    last_activity_at_ms: u64,
    number: u64,
    #[serde(default)]
    session: Option<String>,
    title: String,
}

/// Body of an unsuccessful Sync answer.
#[derive(Default, Deserialize)]
struct SyncErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    holder: Option<SyncHolderField>,
    #[serde(default)]
    task: Option<SyncTaskField>,
}

/// The `holder` object of a `task_in_progress` answer.
#[derive(Deserialize)]
struct SyncHolderField {
    last_activity_at_ms: u64,
    #[serde(default)]
    session: Option<String>,
}

#[cfg(test)]
#[path = "platform_test.rs"]
mod tests;

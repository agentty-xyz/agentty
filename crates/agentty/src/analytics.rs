//! Optional application events sent to `PostHog` and mirrored to a local
//! OTLP event log.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, IsTerminal as _};
use std::path::Path;
use std::time::Duration;

use ag_telemetry::otlp::EventLog;
use reqwest::Client;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppError;
use crate::domain::agent::{AgentModel, AgentSelection, AgentSelectionMetadata, ServedModel};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::install_receipt::ShellInstallReceipt;

/// Public, write-only ingestion token shared with the documentation site.
const POSTHOG_PROJECT_TOKEN: &str = "phc_DoaheXXuZ8t7jnkvb9SYuhhpvYFmYr8AMaAHJraKRtGW";

/// Ingestion host for the Agentty `PostHog` project.
const POSTHOG_API_HOST: &str = "https://us.i.posthog.com";

/// Environment variables that mark a CI run. Most providers, including
/// GitHub Actions, GitLab CI/CD, `CircleCI`, Buildkite, and Bitbucket
/// Pipelines, set `CI`; the others identify GitHub Actions explicitly and the
/// Azure Pipelines, Jenkins, and AWS `CodeBuild` runners that do not.
const CI_ENVIRONMENT_VARIABLES: [&str; 5] = [
    "CI",
    "GITHUB_ACTIONS",
    "TF_BUILD",
    "JENKINS_URL",
    "CODEBUILD_BUILD_ID",
];

/// A telemetry sender for `PostHog`, a local OTLP event log, or both.
/// Callers skip the `PostHog` destination when `--disable-telemetry` is set.
#[derive(Clone)]
pub struct Analytics {
    environment: LaunchEnvironment,
    event_log: Option<EventLog>,
    install_method: InstallMethod,
    posthog: Option<PostHog>,
}

/// Bounded creation types reported without session or project identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionType {
    /// Independent session with an eagerly prepared worktree.
    Regular,
    /// Session whose worktree is prepared on first send.
    Draft,
    /// Independent session copied from an existing conversation.
    Fork,
}

impl SessionType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Draft => "draft",
            Self::Fork => "fork",
        }
    }
}

/// Completed execution outcomes reported without turn contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnOutcome {
    /// The turn and its post-processing completed successfully.
    Finished,
    /// The running turn was stopped by the user.
    Interrupted,
}

impl TurnOutcome {
    fn event_name(self) -> &'static str {
        match self {
            Self::Finished => "agentty_turn_finish",
            Self::Interrupted => "agentty_turn_interrupt",
        }
    }
}

/// Bounded turn settings reported with every turn event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnSelection {
    /// Provider and model captured when the turn was queued.
    pub agent: AgentSelection,
}

impl TurnSelection {
    fn properties(self) -> [(&'static str, &'static str); 2] {
        [
            ("agent", self.agent.kind().name()),
            ("model", reported_model(self.agent)),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallMethod {
    Npm,
    Sh,
    Cargo,
    Unknown,
}

impl InstallMethod {
    fn detect() -> Self {
        let executable = std::env::current_exe().ok();
        let receipt = ShellInstallReceipt::host_path();

        executable.as_deref().map_or(Self::Unknown, |path| {
            Self::from_paths(path, receipt.as_deref())
        })
    }

    fn from_paths(executable: &Path, receipt: Option<&Path>) -> Self {
        if executable.ends_with("node_modules/.bin_real/agentty")
            && executable.ancestors().nth(3).and_then(Path::file_name)
                == Some(OsStr::new("agentty"))
        {
            return Self::Npm;
        }

        if receipt.is_some_and(|path| Self::matches_shell_receipt(executable, path)) {
            return Self::Sh;
        }

        if Self::matches_cargo_install(executable) {
            return Self::Cargo;
        }

        Self::Unknown
    }

    fn matches_shell_receipt(executable: &Path, receipt: &Path) -> bool {
        fs::read(receipt).is_ok_and(|contents| {
            ShellInstallReceipt::for_running_build(&contents, executable, |path| {
                fs::canonicalize(path).ok()
            })
            .is_some()
        })
    }

    fn matches_cargo_install(executable: &Path) -> bool {
        if executable.file_name() != Some(OsStr::new("agentty")) {
            return false;
        }
        let Some(root) = executable
            .parent()
            .filter(|parent| parent.file_name() == Some(OsStr::new("bin")))
            .and_then(Path::parent)
        else {
            return false;
        };
        let Ok(contents) = fs::read(root.join(".crates2.json")) else {
            return false;
        };
        let Ok(data) = serde_json::from_slice::<Value>(&contents) else {
            return false;
        };
        let Some(installs) = data["installs"].as_object() else {
            return false;
        };
        let prefix = format!("agentty {} (", env!("CARGO_PKG_VERSION"));

        installs.iter().any(|(package, metadata)| {
            package.starts_with(&prefix)
                && metadata["bins"]
                    .as_array()
                    .is_some_and(|bins| bins.iter().any(|bin| bin == "agentty"))
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Sh => "sh",
            Self::Cargo => "cargo",
            Self::Unknown => "unknown",
        }
    }
}

/// Launch facts that separate automated runs from people at a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaunchEnvironment {
    ci: bool,
    interactive: bool,
}

impl LaunchEnvironment {
    fn detect() -> Self {
        Self::from_signals(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            |name| std::env::var_os(name),
        )
    }

    /// Builds the facts from whether stdin and stdout are both terminals and
    /// an environment lookup. A CI variable counts when it is set to a value
    /// other than empty, `0`, or `false`.
    fn from_signals(interactive: bool, variable: impl Fn(&str) -> Option<OsString>) -> Self {
        let ci = CI_ENVIRONMENT_VARIABLES.iter().any(|name| {
            variable(name).is_some_and(|value| {
                !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"))
            })
        });

        Self { ci, interactive }
    }

    fn properties(self) -> [(&'static str, &'static str); 2] {
        [
            ("ci", Self::flag(self.ci)),
            ("interactive", Self::flag(self.interactive)),
        ]
    }

    fn flag(value: bool) -> &'static str {
        if value { "true" } else { "false" }
    }
}

impl Analytics {
    /// Builds a sender for the bundled Agentty `PostHog` project, identified
    /// by this installation's [`Self::installation_id`].
    pub fn posthog(installation_id: &str) -> Option<Self> {
        Self::new(POSTHOG_PROJECT_TOKEN, POSTHOG_API_HOST, installation_id)
    }

    /// Returns the random installation ID, creating and saving it on first
    /// use. A failed save leaves the new ID in use for this launch only.
    /// `PostHog` events use it as their distinct ID and OTLP export as its
    /// `user.id` resource attribute.
    pub async fn installation_id(repositories: &AppRepositories) -> String {
        let settings = repositories.settings();
        let stored_id = settings
            .get_setting(SettingName::TelemetryInstallationId)
            .await
            .ok()
            .flatten()
            .filter(|stored_id| !stored_id.is_empty());
        if let Some(stored_id) = stored_id {
            return stored_id;
        }

        let installation_id = Uuid::new_v4().to_string();
        let _ = settings
            .upsert_setting(SettingName::TelemetryInstallationId, &installation_id)
            .await;

        installation_id
    }

    /// Builds a sender for an explicit project token, ingestion host, and
    /// event identity.
    pub fn new(token: &str, host: &str, distinct_id: &str) -> Option<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok();

        Self::with_client(token, host, distinct_id, client)
    }

    /// Adds a local OTLP event log that receives every event with the same
    /// name and properties, whether or not `PostHog` is enabled.
    pub fn mirror(analytics: Option<Self>, event_log: Option<EventLog>) -> Option<Self> {
        let Some(event_log) = event_log else {
            return analytics;
        };

        Some(match analytics {
            Some(analytics) => Self {
                event_log: Some(event_log),
                ..analytics
            },
            None => Self {
                environment: LaunchEnvironment::detect(),
                event_log: Some(event_log),
                install_method: InstallMethod::detect(),
                posthog: None,
            },
        })
    }

    /// Records one application launch attempt with whether it runs in CI and
    /// at an interactive terminal.
    pub async fn record_launch(&self) {
        self.send("agentty_launch", &self.environment.properties())
            .await;
    }

    /// Records a newly reserved session's creation type, provider, and model.
    pub async fn record_session_start(&self, session_type: SessionType, agent: AgentSelection) {
        self.send(
            "agentty_session_start",
            &[
                ("session_type", session_type.as_str()),
                ("agent", agent.kind().name()),
                ("model", reported_model(agent)),
            ],
        )
        .await;
    }

    /// Records an accepted first or follow-up message without its contents,
    /// including the provider and model it runs with.
    pub async fn record_turn_start(&self, turn: TurnSelection) {
        self.send("agentty_turn_start", &turn.properties()).await;
    }

    /// Records a successful or user-interrupted turn with its queued selection.
    pub async fn record_turn_end(&self, turn: TurnSelection, outcome: TurnOutcome) {
        self.send(outcome.event_name(), &turn.properties()).await;
    }

    /// Records a launch-ending failure using only its bounded category and
    /// stage plus the launch environment, never its message.
    pub async fn record_failure(&self, error: &AppError) {
        let (category, stage) = Self::failure_classification(error);
        let [ci, interactive] = self.environment.properties();

        self.send(
            "agentty_failure",
            &[
                ("failure_category", category),
                ("stage", stage),
                ci,
                interactive,
            ],
        )
        .await;
    }

    fn with_client(
        token: &str,
        host: &str,
        distinct_id: &str,
        client: Option<Client>,
    ) -> Option<Self> {
        let client = client?;

        Some(Self {
            environment: LaunchEnvironment::detect(),
            event_log: None,
            install_method: InstallMethod::detect(),
            posthog: Some(PostHog {
                client,
                distinct_id: distinct_id.to_string(),
                endpoint: format!("{}/i/v0/e/", host.trim_end_matches('/')),
                token: token.to_string(),
            }),
        })
    }

    async fn send(&self, name: &'static str, event_properties: &[(&'static str, &'static str)]) {
        let mut properties = vec![
            ("app_source", "cli"),
            ("app_version", env!("CARGO_PKG_VERSION")),
            ("install_method", self.install_method.as_str()),
        ];
        properties.extend_from_slice(event_properties);

        if let Some(event_log) = &self.event_log {
            event_log.emit(name, &properties);
        }
        if let Some(posthog) = &self.posthog {
            posthog.send(name, &properties).await;
        }
    }

    /// Returns the fixed category and stage for each error kind.
    ///
    /// The stage is `startup` until the terminal UI is prepared, and
    /// `runtime` afterward. Only the running terminal UI can end a launch
    /// after startup, so every other kind comes from building the
    /// application.
    fn failure_classification(error: &AppError) -> (&'static str, &'static str) {
        match error {
            AppError::Session(_) => ("session", "startup"),
            AppError::Db(_) => ("database", "startup"),
            AppError::Git(_) => ("git", "startup"),
            AppError::OneShot(_) => ("one_shot", "startup"),
            AppError::NoAgentAvailable => ("no_agent", "startup"),
            AppError::TerminalSetup(_) => ("terminal", "startup"),
            AppError::Terminal(_) => ("terminal", "runtime"),
            AppError::Workflow(_) => ("workflow", "startup"),
        }
    }
}

/// Returns the model id reported in events. A local server names its models
/// freely, so served models report only their bounded `openai-compatible`
/// family.
fn reported_model(agent: AgentSelection) -> &'static str {
    match agent.model() {
        AgentModel::OpenAiCompatible(_) => ServedModel::CONFIGURED.id(),
        model => model.as_str(),
    }
}

/// The bundled or explicitly configured `PostHog` ingestion destination.
#[derive(Clone)]
struct PostHog {
    client: Client,
    distinct_id: String,
    endpoint: String,
    token: String,
}

impl PostHog {
    async fn send(&self, name: &str, event_properties: &[(&str, &str)]) {
        let mut properties = json!({ "$process_person_profile": false });
        for &(key, value) in event_properties {
            properties[key] = Value::String(value.to_string());
        }

        let event = json!({
            "api_key": self.token,
            "distinct_id": self.distinct_id,
            "event": name,
            "properties": properties,
        });
        let _ = self.client.post(&self.endpoint).json(&event).send().await;
    }
}

#[cfg(test)]
#[path = "analytics_test.rs"]
mod tests;

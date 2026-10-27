//! Optional application events sent to `PostHog` and mirrored to a local
//! OTLP event log.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ag_telemetry::otlp::EventLog;
use reqwest::Client;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppError;
use crate::domain::agent::{AgentModel, AgentSelection, AgentSelectionMetadata, ServedModel};
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;

/// Public, write-only ingestion token shared with the documentation site.
const POSTHOG_PROJECT_TOKEN: &str = "phc_DoaheXXuZ8t7jnkvb9SYuhhpvYFmYr8AMaAHJraKRtGW";

/// Ingestion host for the Agentty `PostHog` project.
const POSTHOG_API_HOST: &str = "https://us.i.posthog.com";

/// Environment variable that controls telemetry; `0` disables it.
pub const TELEMETRY_ENABLED_ENV: &str = "AGENTTY_TELEMETRY_ENABLED";

/// A telemetry sender for `PostHog`, a local OTLP event log, or both.
/// Callers check [`Analytics::is_enabled`] before building the `PostHog`
/// destination.
#[derive(Clone)]
pub struct Analytics {
    event_log: Option<EventLog>,
    install_method: InstallMethod,
    posthog: Option<PostHog>,
}

/// Bounded creation types reported without session or project identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionType {
    /// Independent session with an eagerly prepared worktree.
    Regular,
    /// Root session whose worktree is prepared on first send.
    Draft,
    /// Draft based on another session's branch.
    Stacked,
    /// Independent session copied from an existing conversation.
    Fork,
}

impl SessionType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Draft => "draft",
            Self::Stacked => "stacked",
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
        let receipt = Self::receipt_path(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        );

        executable.as_deref().map_or(Self::Unknown, |path| {
            Self::from_paths(path, receipt.as_deref())
        })
    }

    fn receipt_path(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
        xdg_config_home
            .or_else(|| home.map(|home| home.join(".config")))
            .map(|home| home.join("agentty/agentty-receipt.json"))
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
        let Ok(contents) = fs::read(receipt) else {
            return false;
        };
        let Ok(data) = serde_json::from_slice::<Value>(&contents) else {
            return false;
        };
        if data["provider"]["source"] != "cargo-dist"
            || data["source"]["app_name"] != "agentty"
            || data["version"] != env!("CARGO_PKG_VERSION")
        {
            return false;
        }
        let Some(prefix) = data["install_prefix"].as_str() else {
            return false;
        };
        let expected = match data["install_layout"].as_str() {
            Some("cargo-home") => Path::new(prefix).join("bin/agentty"),
            Some("flat") => Path::new(prefix).join("agentty"),
            _ => return false,
        };

        fs::canonicalize(executable)
            .ok()
            .zip(fs::canonicalize(expected).ok())
            .is_some_and(|(actual, expected)| actual == expected)
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

impl Analytics {
    /// Returns whether a `TELEMETRY_ENABLED_ENV` value allows telemetry.
    ///
    /// Unset and `1` enable it; any other value, including `0`, disables it.
    pub fn is_enabled(value: Option<&OsStr>) -> bool {
        value.is_none_or(|value| value == "1")
    }

    /// Builds a sender for the bundled Agentty `PostHog` project, identified
    /// by this installation's stored ID.
    pub async fn posthog(repositories: &AppRepositories) -> Option<Self> {
        let installation_id = Self::installation_id(repositories).await;

        Self::new(POSTHOG_PROJECT_TOKEN, POSTHOG_API_HOST, &installation_id)
    }

    /// Returns the random installation ID, creating and saving it on first
    /// use. A failed save leaves the new ID in use for this launch only.
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
                event_log: Some(event_log),
                install_method: InstallMethod::detect(),
                posthog: None,
            },
        })
    }

    /// Records one application launch attempt.
    pub async fn record_launch(&self) {
        self.send("agentty_launch", &[]).await;
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

    /// Records an accepted first or follow-up message without its contents.
    pub async fn record_turn_start(&self, agent: AgentSelection) {
        self.send(
            "agentty_turn_start",
            &[
                ("agent", agent.kind().name()),
                ("model", reported_model(agent)),
            ],
        )
        .await;
    }

    /// Records a successful or user-interrupted turn with its queued selection.
    pub async fn record_turn_end(&self, agent: AgentSelection, outcome: TurnOutcome) {
        self.send(
            outcome.event_name(),
            &[
                ("agent", agent.kind().name()),
                ("model", reported_model(agent)),
            ],
        )
        .await;
    }

    /// Records a failure using only a bounded category, never its message.
    pub async fn record_failure(&self, error: &AppError) {
        let category = match error {
            AppError::Db(_) => "database",
            _ => "application",
        };

        self.send("agentty_failure", &[("failure_category", category)])
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

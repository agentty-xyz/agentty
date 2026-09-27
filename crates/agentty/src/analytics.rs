//! Optional application events sent to `PostHog`.

use std::ffi::OsStr;
use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppError;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;

/// Public, write-only ingestion token shared with the documentation site.
const POSTHOG_PROJECT_TOKEN: &str = "phc_DoaheXXuZ8t7jnkvb9SYuhhpvYFmYr8AMaAHJraKRtGW";

/// Ingestion host for the Agentty `PostHog` project.
const POSTHOG_API_HOST: &str = "https://us.i.posthog.com";

/// Environment variable that controls telemetry; `0` disables it.
pub const TELEMETRY_ENABLED_ENV: &str = "AGENTTY_TELEMETRY_ENABLED";

/// A telemetry sender. Callers check [`Analytics::is_enabled`] before use.
pub struct Analytics {
    client: Client,
    distinct_id: String,
    endpoint: String,
    token: String,
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
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;

        Some(Self {
            client,
            distinct_id: distinct_id.to_string(),
            endpoint: format!("{}/i/v0/e/", host.trim_end_matches('/')),
            token: token.to_string(),
        })
    }

    /// Records one application launch attempt.
    pub async fn record_launch(&self) {
        self.send("agentty_launch", None).await;
    }

    /// Records a failure using only a bounded category, never its message.
    pub async fn record_failure(&self, error: &AppError) {
        let category = match error {
            AppError::Db(_) => "database",
            _ => "application",
        };

        self.send("agentty_failure", Some(category)).await;
    }

    async fn send(&self, name: &str, category: Option<&str>) {
        let mut properties = json!({
            "$process_person_profile": false,
            "app_version": env!("CARGO_PKG_VERSION"),
        });
        if let Some(category) = category {
            properties["failure_category"] = Value::String(category.to_string());
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

//! Provider account and subscription usage collection.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::agent_usage::{AgentUsageRow, AgentUsageSnapshot, AgentUsageStatus};
use crate::infra::app_server::AppServerError;

/// Boxed async result used by [`AgentUsageProbe`] implementations.
pub(crate) type AgentUsageFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Request context for loading provider account usage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentUsageRequest {
    /// Agent CLIs currently runnable on this machine.
    pub available_agent_kinds: Vec<AgentKind>,
    /// Working directory used when a provider CLI requires a current project.
    pub working_dir: PathBuf,
}

/// Boundary for collecting provider account, subscription, and quota usage.
pub trait AgentUsageProbe: Send + Sync {
    /// Loads a best-effort provider usage snapshot.
    fn load_usage(&self, request: AgentUsageRequest) -> AgentUsageFuture<AgentUsageSnapshot>;
}

/// Production usage probe backed by each provider CLI's supported interface.
pub struct RealAgentUsageProbe;

impl AgentUsageProbe for RealAgentUsageProbe {
    fn load_usage(&self, request: AgentUsageRequest) -> AgentUsageFuture<AgentUsageSnapshot> {
        Box::pin(async move { load_usage_snapshot(request).await })
    }
}

/// Static usage probe used by tests and injected environments.
pub struct StaticAgentUsageProbe {
    /// Snapshot returned from every usage request.
    pub snapshot: AgentUsageSnapshot,
}

impl AgentUsageProbe for StaticAgentUsageProbe {
    fn load_usage(&self, _request: AgentUsageRequest) -> AgentUsageFuture<AgentUsageSnapshot> {
        let snapshot = self.snapshot.clone();

        Box::pin(async move { snapshot })
    }
}

/// Loads usage rows for all supported provider families in stable display
/// order.
async fn load_usage_snapshot(request: AgentUsageRequest) -> AgentUsageSnapshot {
    let rows = AgentKind::ALL
        .iter()
        .copied()
        .map(|agent_kind| load_usage_row(&request, agent_kind))
        .collect::<Vec<_>>();
    let mut loaded_rows = Vec::with_capacity(rows.len());

    for row_future in rows {
        loaded_rows.push(row_future.await);
    }

    AgentUsageSnapshot::new(loaded_rows)
}

/// Loads one provider usage row or an explicit placeholder status.
async fn load_usage_row(request: &AgentUsageRequest, agent_kind: AgentKind) -> AgentUsageRow {
    if !request.available_agent_kinds.contains(&agent_kind) {
        return AgentUsageRow::new(agent_kind, AgentUsageStatus::MissingCli);
    }

    match agent_kind {
        AgentKind::Codex => load_codex_usage_row(request).await,
        AgentKind::Claude => AgentUsageRow::new(
            agent_kind,
            AgentUsageStatus::NotImplemented {
                message: "Claude `/usage` exists, but Agentty does not yet have a structured \
                          collector."
                    .to_string(),
            },
        ),
        AgentKind::Gemini => AgentUsageRow::new(
            agent_kind,
            AgentUsageStatus::NotImplemented {
                message: "Gemini `/stats model` exists, but Agentty does not yet collect it from \
                          ACP."
                    .to_string(),
            },
        ),
    }
}

/// Loads the Codex account usage row through the app-server account API.
async fn load_codex_usage_row(request: &AgentUsageRequest) -> AgentUsageRow {
    let status = match super::app_server::load_codex_account_usage(
        request.working_dir.as_path(),
        AgentModel::Gpt54.as_str(),
    )
    .await
    {
        Ok(details) => AgentUsageStatus::Available(details),
        Err(error) => codex_usage_error_status(error),
    };

    AgentUsageRow::new(AgentKind::Codex, status)
}

/// Maps expected Codex account/network failures to muted unavailable states
/// while preserving unexpected integration failures as errors.
fn codex_usage_error_status(error: AppServerError) -> AgentUsageStatus {
    let message = error.to_string();
    if is_codex_usage_unavailable_error(&message) {
        return AgentUsageStatus::Unavailable {
            message: format!("Unavailable: {message}. Reopen Stats to retry."),
        };
    }

    AgentUsageStatus::Error { message }
}

/// Returns whether an account-usage error looks like a transient auth or
/// network condition the user can fix outside Agentty.
fn is_codex_usage_unavailable_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "auth",
        "forbidden",
        "login",
        "network",
        "offline",
        "sign in",
        "timeout",
        "timed out",
        "unauthorized",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent_usage::AgentUsageStatus;

    #[tokio::test]
    async fn real_probe_reports_placeholders_for_available_gemini_and_claude() {
        // Arrange
        let request = AgentUsageRequest {
            available_agent_kinds: vec![AgentKind::Gemini, AgentKind::Claude],
            working_dir: PathBuf::from("/tmp/project"),
        };

        // Act
        let snapshot = RealAgentUsageProbe.load_usage(request).await;

        // Assert
        assert_eq!(snapshot.rows.len(), 3);
        assert_eq!(
            snapshot.rows[0].status,
            AgentUsageStatus::NotImplemented {
                message: "Gemini `/stats model` exists, but Agentty does not yet collect it from \
                          ACP."
                    .to_string()
            }
        );
        assert_eq!(
            snapshot.rows[1].status,
            AgentUsageStatus::NotImplemented {
                message: "Claude `/usage` exists, but Agentty does not yet have a structured \
                          collector."
                    .to_string()
            }
        );
        assert_eq!(snapshot.rows[2].status, AgentUsageStatus::MissingCli);
    }

    #[tokio::test]
    async fn static_probe_returns_configured_snapshot() {
        // Arrange
        let expected_snapshot = AgentUsageSnapshot::new(vec![AgentUsageRow::new(
            AgentKind::Codex,
            AgentUsageStatus::MissingCli,
        )]);
        let probe = StaticAgentUsageProbe {
            snapshot: expected_snapshot.clone(),
        };
        let request = AgentUsageRequest {
            available_agent_kinds: Vec::new(),
            working_dir: PathBuf::from("/tmp/project"),
        };

        // Act
        let snapshot = probe.load_usage(request).await;

        // Assert
        assert_eq!(snapshot, expected_snapshot);
    }

    #[test]
    fn codex_usage_error_status_treats_auth_failures_as_unavailable() {
        // Arrange
        let error = AppServerError::Provider("Codex auth token is expired".to_string());

        // Act
        let status = codex_usage_error_status(error);

        // Assert
        assert!(matches!(
            status,
            AgentUsageStatus::Unavailable { ref message }
                if message.contains("Codex auth token is expired")
                    && message.contains("Reopen Stats to retry")
        ));
    }

    #[test]
    fn codex_usage_error_status_keeps_unexpected_failures_as_errors() {
        // Arrange
        let error = AppServerError::Provider("Malformed rate-limit payload".to_string());

        // Act
        let status = codex_usage_error_status(error);

        // Assert
        assert_eq!(
            status,
            AgentUsageStatus::Error {
                message: "Malformed rate-limit payload".to_string(),
            }
        );
    }
}

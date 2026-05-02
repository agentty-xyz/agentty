use super::agent::AgentKind;

/// Account and quota usage snapshot for all provider CLIs shown on the stats
/// page.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentUsageSnapshot {
    /// Provider rows rendered in stable agent order.
    pub rows: Vec<AgentUsageRow>,
}

impl AgentUsageSnapshot {
    /// Creates a snapshot from precomputed provider rows.
    #[must_use]
    pub fn new(rows: Vec<AgentUsageRow>) -> Self {
        Self { rows }
    }
}

/// Account or usage state for one provider CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentUsageRow {
    /// Provider family described by this row.
    pub kind: AgentKind,
    /// Current provider usage state.
    pub status: AgentUsageStatus,
}

impl AgentUsageRow {
    /// Creates one provider usage row.
    #[must_use]
    pub fn new(kind: AgentKind, status: AgentUsageStatus) -> Self {
        Self { kind, status }
    }
}

/// Provider-specific account and quota usage status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentUsageStatus {
    /// Structured account and rate-limit data was loaded successfully.
    Available(AgentUsageDetails),
    /// The provider CLI is not currently runnable on this machine.
    MissingCli,
    /// Usage could not be loaded because the provider account or network is
    /// temporarily unavailable.
    Unavailable { message: String },
    /// The provider has a placeholder integration but no collector yet.
    NotImplemented { message: String },
    /// Usage collection was attempted and failed.
    Error { message: String },
}

/// Structured usage data loaded from one provider CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentUsageDetails {
    /// Subscription or account plan label, when the provider reports one.
    pub plan: Option<String>,
    /// Provider rate-limit buckets shown in display order.
    pub rate_limits: Vec<AgentRateLimit>,
    /// Provider-specific limit state when a limit has been reached.
    pub reached_type: Option<String>,
}

/// One provider-reported usage or quota bucket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRateLimit {
    /// Human-readable bucket label, such as `Primary` or `Secondary`.
    pub label: String,
    /// Percentage used within the current quota window.
    pub used_percent: Option<String>,
    /// Quota reset timestamp expressed as Unix seconds.
    pub resets_at_unix_seconds: Option<i64>,
    /// Quota window length in minutes.
    pub window_duration_mins: Option<u64>,
}

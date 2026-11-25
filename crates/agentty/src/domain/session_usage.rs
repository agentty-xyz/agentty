//! Per-session token usage and estimated cost: recorded usage per model and
//! purpose, folded with the running turn's live usage.
//!
//! A `None` detail is unknown, never zero. Invocations recorded before
//! detailed usage count in [`UsageRecord::invocation_count`] but not in
//! [`UsageRecord::detailed_invocation_count`], and report no detail.

use ag_contracts::{CostBasis, ModelUsage, TurnUsage, UsagePurpose};

use crate::domain::session::SessionId;

/// Usage of one model under one purpose, accumulated over invocations the
/// way the store accumulates its rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageRecord {
    /// Input tokens served from the prompt cache, when every invocation
    /// reported them.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to the prompt cache, when every invocation
    /// reported them.
    pub cache_write_tokens: Option<u64>,
    /// Largest request input observed.
    pub context_peak_tokens: Option<u64>,
    /// Context window of the model, when known.
    pub context_window_tokens: Option<u64>,
    /// Price basis of the priced invocations.
    pub cost_basis: Option<CostBasis>,
    /// Estimated charge of the priced invocations in micro-USD.
    pub cost_micro_usd: Option<u64>,
    /// Invocations recorded with detailed usage.
    pub detailed_invocation_count: u64,
    /// Input tokens, cache included.
    pub input_tokens: u64,
    /// Invocations included in the totals.
    pub invocation_count: u64,
    /// Provider model identifier.
    pub model: String,
    /// Output tokens, reasoning included.
    pub output_tokens: u64,
    /// Work the usage is recorded under.
    pub purpose: UsagePurpose,
    /// Reasoning tokens, when every invocation reported them.
    pub reasoning_tokens: Option<u64>,
    /// Provider requests of the detailed invocations.
    pub request_count: u64,
    /// Detailed invocations that had no price.
    pub unpriced_invocation_count: u64,
    /// Requests whose backend reported no input or output count.
    pub unreported_request_count: u64,
}

impl UsageRecord {
    /// Returns the record of one detailed invocation of `usage` under
    /// `purpose`.
    pub fn from_model_usage(purpose: UsagePurpose, usage: &ModelUsage) -> Self {
        Self {
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            context_peak_tokens: usage.context.map(|context| context.peak_input_tokens),
            context_window_tokens: usage.context.and_then(|context| context.window_tokens),
            cost_basis: usage.cost.map(|cost| cost.basis),
            cost_micro_usd: usage.cost.map(|cost| cost.micro_usd),
            detailed_invocation_count: 1,
            input_tokens: usage.input_tokens,
            invocation_count: 1,
            model: usage.model.clone(),
            output_tokens: usage.output_tokens,
            purpose,
            reasoning_tokens: usage.reasoning_tokens,
            request_count: u64::from(usage.requests),
            unpriced_invocation_count: u64::from(usage.cost.is_none()),
            unreported_request_count: u64::from(usage.unreported_requests),
        }
    }

    /// Adds the later record `next` of the same model and purpose. A detail
    /// stays known only when both sides know it, costs sum over priced
    /// invocations, and different bases sum to [`CostBasis::Mixed`].
    pub fn add(&mut self, next: &Self) {
        let both = |total: Option<u64>, tokens: Option<u64>| {
            total
                .zip(tokens)
                .map(|(total, tokens)| total.saturating_add(tokens))
        };
        self.cache_read_tokens = both(self.cache_read_tokens, next.cache_read_tokens);
        self.cache_write_tokens = both(self.cache_write_tokens, next.cache_write_tokens);
        self.context_peak_tokens = match (self.context_peak_tokens, next.context_peak_tokens) {
            (Some(peak), Some(next_peak)) => Some(peak.max(next_peak)),
            (peak, next_peak) => peak.or(next_peak),
        };
        self.context_window_tokens = next.context_window_tokens.or(self.context_window_tokens);
        self.cost_basis = match (self.cost_basis, next.cost_basis) {
            (Some(basis), Some(next_basis)) if basis != next_basis => Some(CostBasis::Mixed),
            (basis, next_basis) => next_basis.or(basis),
        };
        self.cost_micro_usd = match (self.cost_micro_usd, next.cost_micro_usd) {
            (Some(cost), Some(next_cost)) => Some(cost.saturating_add(next_cost)),
            (cost, next_cost) => cost.or(next_cost),
        };
        self.detailed_invocation_count = self
            .detailed_invocation_count
            .saturating_add(next.detailed_invocation_count);
        self.input_tokens = self.input_tokens.saturating_add(next.input_tokens);
        self.invocation_count = self.invocation_count.saturating_add(next.invocation_count);
        self.output_tokens = self.output_tokens.saturating_add(next.output_tokens);
        self.reasoning_tokens = both(self.reasoning_tokens, next.reasoning_tokens);
        self.request_count = self.request_count.saturating_add(next.request_count);
        self.unpriced_invocation_count = self
            .unpriced_invocation_count
            .saturating_add(next.unpriced_invocation_count);
        self.unreported_request_count = self
            .unreported_request_count
            .saturating_add(next.unreported_request_count);
    }

    /// Returns how many invocations were recorded before detailed usage.
    pub fn undetailed_invocation_count(&self) -> u64 {
        self.invocation_count
            .saturating_sub(self.detailed_invocation_count)
    }

    /// Returns whether every request reported no token count, so no token
    /// figure of this record is known.
    pub fn is_fully_unreported(&self) -> bool {
        self.unreported_request_count > 0
            && self.unreported_request_count >= self.request_count
            && self.input_tokens == 0
            && self.output_tokens == 0
    }
}

/// Input tokens of the session's latest model request against its window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextNow {
    /// Input tokens of the latest request, cache included.
    pub tokens: u64,
    /// Context window of the model that answered it, when known.
    pub window_tokens: Option<u64>,
}

/// Run whose live usage a session reports while it runs.
///
/// Ordinary session turns, and rebase assists, run one at a time, so each
/// kind shares one key; task and graph turns can overlap and are keyed by
/// their attempt. Runs order attempts by identifier, then the rebase
/// assist, then the session turn.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LiveUsageRun {
    /// A task-agent or graph-node turn, identified by its attempt.
    Attempt(String),
    /// A conflict-resolution assist while rebasing the session branch.
    Rebase,
    /// An ordinary session turn.
    SessionTurn,
}

impl LiveUsageRun {
    /// Returns the purpose the run's usage is recorded under.
    pub fn purpose(&self) -> UsagePurpose {
        match self {
            Self::Attempt(_) | Self::SessionTurn => UsagePurpose::Turn,
            Self::Rebase => UsagePurpose::Rebase,
        }
    }
}

/// Identifies one running turn's live usage.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LiveUsageKey {
    /// Run that reports the usage.
    pub run: LiveUsageRun,
    /// Session the run belongs to.
    pub session_id: SessionId,
}

/// Recorded usage of one session, optionally folded with its running turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionUsage {
    /// Latest context fill, when any request reported its input.
    pub context: Option<ContextNow>,
    /// One record per model and purpose, ordered by model, session turns
    /// first.
    pub records: Vec<UsageRecord>,
    /// Prompts that started a user-visible or generated turn.
    pub turn_count: u64,
}

impl SessionUsage {
    /// Returns this usage with the cumulative usage of each running turn in
    /// `live` folded into its records under the run's purpose.
    ///
    /// The latest context fill comes from the running turns in a fixed
    /// order whatever order `live` yields them in: attempts by identifier,
    /// then a rebase assist, then the session turn, whose fill wins when it
    /// reported one.
    #[must_use]
    pub fn with_live<'usage>(
        &self,
        live: impl IntoIterator<Item = (&'usage LiveUsageRun, &'usage TurnUsage)>,
    ) -> Self {
        let mut live = live.into_iter().collect::<Vec<_>>();
        live.sort_by_key(|&(run, _)| run);
        let mut usage = self.clone();
        for (run, turn_usage) in live {
            let purpose = run.purpose();
            for model_usage in &turn_usage.models {
                let record = UsageRecord::from_model_usage(purpose, model_usage);
                match usage
                    .records
                    .iter_mut()
                    .find(|existing| existing.model == record.model && existing.purpose == purpose)
                {
                    Some(existing) => existing.add(&record),
                    None => usage.records.push(record),
                }
            }
            if let Some(context) = turn_usage.context() {
                usage.context = Some(ContextNow {
                    tokens: context.last_input_tokens,
                    window_tokens: context.window_tokens,
                });
            }
        }
        usage.sort_records();

        usage
    }

    /// Orders records by model, with session turns before utility work.
    pub fn sort_records(&mut self) {
        self.records.sort_by(|left, right| {
            left.model
                .cmp(&right.model)
                .then_with(|| purpose_rank(left.purpose).cmp(&purpose_rank(right.purpose)))
        });
    }
}

/// Parses a persisted purpose label; unknown labels read as
/// [`UsagePurpose::Other`].
pub fn parse_usage_purpose(label: &str) -> UsagePurpose {
    [
        UsagePurpose::Turn,
        UsagePurpose::Title,
        UsagePurpose::CommitAssist,
        UsagePurpose::Review,
        UsagePurpose::Rebase,
    ]
    .into_iter()
    .find(|purpose| purpose.as_str() == label)
    .unwrap_or(UsagePurpose::Other)
}

/// Parses a persisted cost-basis label; unknown labels read as
/// [`CostBasis::Mixed`], which claims no specific source.
pub fn parse_cost_basis(label: &str) -> CostBasis {
    [
        CostBasis::ListPrice,
        CostBasis::HostPrice,
        CostBasis::AgentReported,
    ]
    .into_iter()
    .find(|basis| basis.as_str() == label)
    .unwrap_or(CostBasis::Mixed)
}

/// Returns the display order of `purpose`: session turns first.
fn purpose_rank(purpose: UsagePurpose) -> u8 {
    match purpose {
        UsagePurpose::Turn => 0,
        UsagePurpose::Title => 1,
        UsagePurpose::CommitAssist => 2,
        UsagePurpose::Review => 3,
        UsagePurpose::Rebase => 4,
        UsagePurpose::Other => 5,
    }
}

#[cfg(test)]
#[path = "session_usage_test.rs"]
mod tests;

//! Provider-neutral token, cost, and context-fill usage that every agent
//! backend reports for a turn or isolated prompt.
//!
//! Counts a backend does not report stay `None` rather than zero, so totals
//! never present an unknown figure as a known one.

/// Work an invocation's usage is recorded under, so utility work stays
/// distinguishable from session turns.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UsagePurpose {
    /// A session turn, including its compaction and summary requests.
    Turn,
    /// Generation of a session title.
    Title,
    /// Generation of a commit message.
    CommitAssist,
    /// A focused review of session changes.
    Review,
    /// Conflict resolution while rebasing a session branch.
    Rebase,
    /// Any other utility prompt.
    Other,
}

impl UsagePurpose {
    /// Returns the stable persisted label of this purpose.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Title => "title",
            Self::CommitAssist => "commit_assist",
            Self::Review => "review",
            Self::Rebase => "rebase",
            Self::Other => "other",
        }
    }
}

/// Where the prices of a [`CostEstimate`] came from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CostBasis {
    /// Provider list prices from the built-in model catalog.
    ListPrice,
    /// Prices the host supplied, such as a negotiated rate.
    HostPrice,
    /// The agent's own estimate, such as Claude Code's per-model `costUSD`.
    AgentReported,
    /// A sum of estimates priced from more than one basis.
    Mixed,
}

impl CostBasis {
    /// Returns the stable persisted label of this basis.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListPrice => "list",
            Self::HostPrice => "host",
            Self::AgentReported => "agent_reported",
            Self::Mixed => "mixed",
        }
    }
}

/// Estimated charge of some usage in integer micro-USD, one millionth of a
/// US dollar. Provider bills stay authoritative.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CostEstimate {
    /// Where the prices came from.
    pub basis: CostBasis,
    /// Estimated charge in micro-USD.
    pub micro_usd: u64,
}

impl CostEstimate {
    /// Adds `other`, saturating; estimates from different bases sum to
    /// [`CostBasis::Mixed`].
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            basis: if self.basis == other.basis {
                self.basis
            } else {
                CostBasis::Mixed
            },
            micro_usd: self.micro_usd.saturating_add(other.micro_usd),
        }
    }
}

/// How full a model's context window was, measured by the reported input
/// tokens of its requests, cache included.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContextFill {
    /// Input tokens of the latest request: what the model last saw.
    pub last_input_tokens: u64,
    /// Largest request input observed.
    pub peak_input_tokens: u64,
    /// Context window of the model, or `None` when unknown.
    pub window_tokens: Option<u64>,
}

impl ContextFill {
    /// Returns the fill of one request with `input_tokens` of input.
    pub fn of_request(input_tokens: u64, window_tokens: Option<u64>) -> Self {
        Self {
            last_input_tokens: input_tokens,
            peak_input_tokens: input_tokens,
            window_tokens,
        }
    }

    /// Folds the later fill `next` into this one: the latest request comes
    /// from `next`, the peak is the larger of both.
    #[must_use]
    pub fn then(self, next: Self) -> Self {
        Self {
            last_input_tokens: next.last_input_tokens,
            peak_input_tokens: self.peak_input_tokens.max(next.peak_input_tokens),
            window_tokens: next.window_tokens.or(self.window_tokens),
        }
    }
}

/// Tokens and estimated charge of one model within one turn or isolated
/// prompt.
///
/// The counts overlap: `cache_read_tokens` and `cache_write_tokens` are part
/// of `input_tokens`, and `reasoning_tokens` is part of `output_tokens`.
/// A request whose backend reported no input or output count is counted in
/// `unreported_requests`, which makes the token totals lower bounds.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ModelUsage {
    /// Input tokens served from the provider's prompt cache, or `None` when
    /// the backend does not report them.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to the provider's prompt cache, or `None` when
    /// the backend does not report them.
    pub cache_write_tokens: Option<u64>,
    /// Context fill of this model's requests, or `None` when no request
    /// reported its input.
    pub context: Option<ContextFill>,
    /// Estimated charge, or `None` when no price is known for every request.
    pub cost: Option<CostEstimate>,
    /// All input tokens, cache included.
    pub input_tokens: u64,
    /// Model identifier, as sessions record it.
    pub model: String,
    /// Output tokens, reasoning included.
    pub output_tokens: u64,
    /// Reasoning tokens, or `None` when the backend does not report them.
    pub reasoning_tokens: Option<u64>,
    /// Provider requests counted, including unreported ones.
    pub requests: u32,
    /// Requests whose backend reported no input or output count.
    pub unreported_requests: u32,
}

impl ModelUsage {
    /// Returns usage of `model` with no requests and every detail unknown.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            cache_read_tokens: None,
            cache_write_tokens: None,
            context: None,
            cost: None,
            input_tokens: 0,
            model: model.into(),
            output_tokens: 0,
            reasoning_tokens: None,
            requests: 0,
            unreported_requests: 0,
        }
    }

    /// Returns usage of `model` for requests whose backend reported nothing.
    pub fn unreported(model: impl Into<String>, requests: u32) -> Self {
        Self {
            requests,
            unreported_requests: requests,
            ..Self::new(model)
        }
    }

    /// Adds the later usage `next` of the same model, saturating. A detail
    /// stays known only when both sides know it.
    pub fn add(&mut self, next: &Self) {
        let both = |total: Option<u64>, tokens: Option<u64>| {
            total
                .zip(tokens)
                .map(|(total, tokens)| total.saturating_add(tokens))
        };
        self.cache_read_tokens = both(self.cache_read_tokens, next.cache_read_tokens);
        self.cache_write_tokens = both(self.cache_write_tokens, next.cache_write_tokens);
        self.context = match (self.context, next.context) {
            (Some(context), Some(next_context)) => Some(context.then(next_context)),
            (context, next_context) => next_context.or(context),
        };
        self.cost = self
            .cost
            .zip(next.cost)
            .map(|(cost, next_cost)| cost.saturating_add(next_cost));
        self.input_tokens = self.input_tokens.saturating_add(next.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(next.output_tokens);
        self.reasoning_tokens = both(self.reasoning_tokens, next.reasoning_tokens);
        self.requests = self.requests.saturating_add(next.requests);
        self.unreported_requests = self
            .unreported_requests
            .saturating_add(next.unreported_requests);
    }

    /// Returns whether this usage records nothing worth persisting: no
    /// tokens and no unreported request.
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0 && self.unreported_requests == 0
    }
}

/// Usage of every model one turn or isolated prompt used.
///
/// Entries are ordered by their latest request, so the last entry with a
/// context fill describes what the most recent request saw.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct TurnUsage {
    /// One entry per model.
    pub models: Vec<ModelUsage>,
}

impl TurnUsage {
    /// Returns the usage of one model.
    pub fn single(usage: ModelUsage) -> Self {
        Self {
            models: vec![usage],
        }
    }

    /// Folds the later usage `next` of one model into its entry, which
    /// moves last, or appends it for a model not seen yet.
    pub fn record(&mut self, next: &ModelUsage) {
        let entry = match self
            .models
            .iter()
            .position(|usage| usage.model == next.model)
        {
            Some(index) => {
                let mut entry = self.models.remove(index);
                entry.add(next);

                entry
            }
            None => next.clone(),
        };
        self.models.push(entry);
    }

    /// Folds every entry of the later usage `next` into this one.
    pub fn merge(&mut self, next: &Self) {
        for usage in &next.models {
            self.record(usage);
        }
    }

    /// Returns the context fill of the most recent request that reported
    /// its input.
    pub fn context(&self) -> Option<ContextFill> {
        self.models.iter().rev().find_map(|usage| usage.context)
    }

    /// Returns the input tokens of every model, cache included.
    pub fn input_tokens(&self) -> u64 {
        self.models
            .iter()
            .fold(0, |total, usage| total.saturating_add(usage.input_tokens))
    }

    /// Returns the output tokens of every model, reasoning included.
    pub fn output_tokens(&self) -> u64 {
        self.models
            .iter()
            .fold(0, |total, usage| total.saturating_add(usage.output_tokens))
    }
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;

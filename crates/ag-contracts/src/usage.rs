//! Provider-neutral token, cost, and context-fill usage that every agent
//! backend reports for a turn or isolated prompt.
//!
//! Counts a backend does not report stay `None` rather than zero, so totals
//! never present an unknown figure as a known one.

use std::path::PathBuf;

/// Cumulative usage totals an agent reported for the most recent
/// conversation in `folder`, which a later run there continues.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct UsageCheckpoint {
    /// Working directory whose most recent conversation the totals cover.
    pub folder: PathBuf,
    /// Opaque totals that only the reporting agent interprets.
    pub totals: String,
}

impl UsageCheckpoint {
    /// Returns the checkpoint of `totals` reported in `folder`.
    pub fn new(folder: impl Into<PathBuf>, totals: impl Into<String>) -> Self {
        Self {
            folder: folder.into(),
            totals: totals.into(),
        }
    }
}

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

/// Where the prices of a [`PricedCost`] came from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CostBasis {
    /// Provider list prices from the built-in model catalog.
    ListPrice,
    /// Prices the host supplied, such as a negotiated rate.
    HostPrice,
    /// The agent's own estimate, such as Claude Code's per-model `costUSD`.
    AgentReported,
    /// A sum of amounts priced from more than one basis.
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

    /// Returns the basis of a sum of amounts priced on `self` and `other`:
    /// their shared basis, or [`Self::Mixed`] when they differ.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        if self == other { self } else { Self::Mixed }
    }
}

/// Subscription login that bills usage a flat fee, whose priced figure is
/// therefore only an API equivalent.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PlanLogin {
    /// A `ChatGPT` plan, through the harness or Codex.
    ChatGpt,
    /// A Claude subscription through Claude Code.
    Claude,
}

impl PlanLogin {
    /// Returns the stable persisted label of this login.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Claude => "claude",
        }
    }
}

/// One priced amount in integer micro-USD, one millionth of a US dollar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PricedCost {
    /// Where the prices came from.
    pub basis: CostBasis,
    /// Amount in micro-USD.
    pub micro_usd: u64,
    /// Whether the amount is an upper bound because cache reads were priced
    /// at the input price, their price being undeclared.
    pub upper_bound: bool,
}

impl PricedCost {
    /// Returns an exact amount of `micro_usd` priced on `basis`.
    pub fn new(basis: CostBasis, micro_usd: u64) -> Self {
        Self {
            basis,
            micro_usd,
            upper_bound: false,
        }
    }

    /// Adds `other`, saturating; the bases combine as
    /// [`CostBasis::combine`] describes, and the sum is an upper bound when
    /// either side is.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            basis: self.basis.combine(other.basis),
            micro_usd: self.micro_usd.saturating_add(other.micro_usd),
            upper_bound: self.upper_bound || other.upper_bound,
        }
    }
}

/// What usage billed to a subscription plan would cost at the provider's
/// API list prices. It is no charge: the plan bills a flat fee.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ApiEquivalentCost {
    /// Amount at API list prices.
    pub cost: PricedCost,
    /// Login whose plan billed the usage, or `None` for a sum over several
    /// logins.
    pub login: Option<PlanLogin>,
}

/// Estimated cost of some usage: the charge of metered usage and the API
/// equivalent of plan-billed usage, kept apart so an API equivalent is
/// never summed into a charge. An estimate always holds at least one of
/// them; usage with no price has no estimate. Provider bills stay
/// authoritative.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CostEstimate(CostParts);

/// The amounts a [`CostEstimate`] holds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CostParts {
    /// Only metered usage.
    Charged(PricedCost),
    /// Only plan-billed usage.
    ApiEquivalent(ApiEquivalentCost),
    /// Both, kept apart.
    Both(PricedCost, ApiEquivalentCost),
}

impl CostEstimate {
    /// Returns an exact charge of `micro_usd` priced on `basis`.
    pub fn new(basis: CostBasis, micro_usd: u64) -> Self {
        Self::charged(PricedCost::new(basis, micro_usd))
    }

    /// Returns the estimate of metered usage charged `cost`.
    pub fn charged(cost: PricedCost) -> Self {
        Self(CostParts::Charged(cost))
    }

    /// Returns the estimate of plan-billed usage whose API equivalent is
    /// `equivalent`.
    pub fn plan_billed(equivalent: ApiEquivalentCost) -> Self {
        Self(CostParts::ApiEquivalent(equivalent))
    }

    /// Returns the estimated charge of the metered usage, or `None` when
    /// none was metered.
    pub fn charge(self) -> Option<PricedCost> {
        match self.0 {
            CostParts::Charged(charged) | CostParts::Both(charged, _) => Some(charged),
            CostParts::ApiEquivalent(_) => None,
        }
    }

    /// Returns the API equivalent of the plan-billed usage, or `None` when
    /// none was plan-billed.
    pub fn api_equivalent(self) -> Option<ApiEquivalentCost> {
        match self.0 {
            CostParts::ApiEquivalent(equivalent) | CostParts::Both(_, equivalent) => {
                Some(equivalent)
            }
            CostParts::Charged(_) => None,
        }
    }

    /// Returns this estimate as plan-billed usage of `login`: every amount
    /// becomes its API equivalent.
    #[must_use]
    pub fn billed_to(self, login: PlanLogin) -> Self {
        let cost = match self.0 {
            CostParts::Charged(cost) | CostParts::ApiEquivalent(ApiEquivalentCost { cost, .. }) => {
                cost
            }
            CostParts::Both(charged, equivalent) => charged.saturating_add(equivalent.cost),
        };

        Self::plan_billed(ApiEquivalentCost {
            cost,
            login: Some(login),
        })
    }

    /// Adds `other`, saturating, part by part: charges to charges and API
    /// equivalents to API equivalents. API equivalents of different logins
    /// sum to an equivalent of no single login.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        let charged = match (self.charge(), other.charge()) {
            (Some(charged), Some(next)) => Some(charged.saturating_add(next)),
            (charged, next) => charged.or(next),
        };
        let api_equivalent = match (self.api_equivalent(), other.api_equivalent()) {
            (Some(equivalent), Some(next)) => Some(ApiEquivalentCost {
                cost: equivalent.cost.saturating_add(next.cost),
                login: equivalent.login.filter(|login| next.login == Some(*login)),
            }),
            (equivalent, next) => equivalent.or(next),
        };

        Self::from_parts(charged, api_equivalent).unwrap_or(self)
    }

    /// Returns the estimate holding `charged` and `api_equivalent`, or
    /// `None` when it would hold neither.
    fn from_parts(
        charged: Option<PricedCost>,
        api_equivalent: Option<ApiEquivalentCost>,
    ) -> Option<Self> {
        match (charged, api_equivalent) {
            (Some(charged), Some(equivalent)) => Some(Self(CostParts::Both(charged, equivalent))),
            (Some(charged), None) => Some(Self::charged(charged)),
            (None, Some(equivalent)) => Some(Self::plan_billed(equivalent)),
            (None, None) => None,
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
    /// Cumulative totals an agent reported for the conversation it left,
    /// which the host stores and passes back through
    /// [`crate::TurnContinuation::with_usage_checkpoint`] so the next turn
    /// continuing that conversation can derive its own share; `None` for
    /// agents and runs that report per-turn usage.
    pub checkpoint: Option<UsageCheckpoint>,
    /// One entry per model.
    pub models: Vec<ModelUsage>,
}

impl TurnUsage {
    /// Returns the usage of one model.
    pub fn single(usage: ModelUsage) -> Self {
        Self {
            checkpoint: None,
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

    /// Folds every entry of the later usage `next` into this one; a later
    /// checkpoint replaces this one.
    pub fn merge(&mut self, next: &Self) {
        for usage in &next.models {
            self.record(usage);
        }
        if next.checkpoint.is_some() {
            self.checkpoint.clone_from(&next.checkpoint);
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

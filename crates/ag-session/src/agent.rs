use std::fmt;
use std::str::FromStr;

use ag_contracts::{ReasoningLevel, SpeedMode};

/// Supported agent provider families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Google Antigravity CLI/backend.
    Antigravity,
    /// Google Gemini CLI/backend.
    Gemini,
    /// Anthropic Claude Code CLI/backend.
    Claude,
    /// `OpenAI` Codex CLI/backend.
    Codex,
}

// Generate typed identities, exhaustive metadata, and ordered provider lists
// from the same declaration so adding a model cannot omit one of those
// surfaces.
macro_rules! define_model_catalog {
    ($($provider:ident { $(
        $(#[$attribute:meta])*
        $model:ident = $discriminant:literal => {
            id: $id:literal,
            description: $description:literal,
            fast: $fast:literal,
            context: $context:expr,
        },
    )* })*) => {
        /// Supported agent model names across all providers.
        ///
        /// Gemini ids are shared by Gemini and Antigravity; provider ownership
        /// lives on [`AgentSelection`]. Removing a variant is a Rust source API
        /// change even when [`AgentModel::retired_replacement`] preserves saved ids.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum AgentModel {
            $($( $(#[$attribute])* $model = $discriminant, )*)*
        }

        impl AgentModel {
            /// All selectable models, grouped in catalog provider order.
            pub const ALL: &[Self] = &[$($(Self::$model,)*)*];

            const fn descriptor(self) -> ModelDescriptor {
                match self {
                    $($(Self::$model => ModelDescriptor {
                        context_limits: $context,
                        description: $description,
                        id: $id,
                        supports_fast_mode: $fast,
                    },)*)*
                }
            }
        }

        impl AgentKind {
            /// Returns this provider's models in their catalog display order.
            pub fn models(self) -> &'static [AgentModel] {
                match self {
                    Self::Antigravity => Self::Gemini.models(),
                    $(Self::$provider => &[$(AgentModel::$model,)*],)*
                }
            }
        }
    };
}

define_model_catalog! {
    Gemini {
        /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
        Gemini31Pro = 6 => {
            id: "gemini-3.1-pro-preview",
            description: "Higher-quality Gemini model for deeper reasoning.",
            fast: false,
            context: None,
        },
        /// Fast Gemini model backed by `gemini-3.8-flash`.
        Gemini38Flash = 4 => {
            id: "gemini-3.8-flash",
            description: "Fast Gemini model for agentic and multimodal tasks.",
            fast: false,
            context: None,
        },
        /// Lightweight Gemini model backed by `gemini-3.5-flash-lite`.
        Gemini35FlashLite = 5 => {
            id: "gemini-3.5-flash-lite",
            description: "Lightweight Gemini model for fast, cost-conscious workloads.",
            fast: false,
            context: None,
        },
    }
    Claude {
        /// Claude Fable model backed by `claude-fable-5-1`.
        ClaudeFable51 = 10 => {
            id: "claude-fable-5-1",
            description: "Claude Fable model for creative, narrative-heavy tasks.",
            fast: false,
            context: None,
        },
        /// Claude Opus model backed by `claude-opus-5-5`.
        ClaudeOpus55 = 8 => {
            id: "claude-opus-5-5",
            description: "Latest Claude Opus model for complex agentic tasks.",
            fast: true,
            context: None,
        },
        /// Claude Sonnet model backed by `claude-sonnet-5`.
        ClaudeSonnet5 = 9 => {
            id: "claude-sonnet-5",
            description: "Balanced Claude model for quality and latency.",
            fast: false,
            context: None,
        },
        /// Claude Haiku model backed by `claude-haiku-4-5-20251001`.
        ClaudeHaiku4520251001 = 11 => {
            id: "claude-haiku-4-5-20251001",
            description: "Fast Claude model for lighter tasks.",
            fast: false,
            context: None,
        },
    }
    Codex {
        /// Codex Astra model backed by `gpt-6-astra`.
        Gpt6Astra = 0 => {
            id: "gpt-6-astra",
            description: "Most capable Codex model for the hardest end-to-end work.",
            fast: true,
            context: Some(ModelContextLimits::CODEX_LARGE),
        },
        /// Codex Sol model backed by `gpt-6.1-sol`.
        Gpt61Sol = 1 => {
            id: "gpt-6.1-sol",
            description: "Near-Astra Codex model for complex work at a lower cost.",
            fast: true,
            context: Some(ModelContextLimits::CODEX_LARGE),
        },
        /// Codex Luna model backed by `gpt-6-luna`.
        Gpt6Luna = 2 => {
            id: "gpt-6-luna",
            description: "Efficient Codex model for focused, high-volume tasks.",
            fast: true,
            context: Some(ModelContextLimits::CODEX_LARGE),
        },
        /// Codex Terra model backed by `gpt-5.6-terra`.
        Gpt56Terra = 3 => {
            id: "gpt-5.6-terra",
            description: "Current Codex model for balanced coding performance.",
            fast: true,
            context: Some(ModelContextLimits::CODEX_LARGE),
        },
        /// Codex spark model backed by `gpt-5.3-codex-spark`.
        Gpt53CodexSpark = 7 => {
            id: "gpt-5.3-codex-spark",
            description: "Codex spark model for quick coding iterations.",
            fast: false,
            context: Some(ModelContextLimits::CODEX_SPARK),
        },
    }
}

/// Declared context capacity and Agentty's proactive input-budget reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelContextLimits {
    /// Total context capacity in tokens.
    pub context_window_tokens: u64,
    /// Tokens reserved for output and provider overhead before compaction.
    /// This is an input-budget policy, not a model's maximum output limit.
    pub input_headroom_tokens: u64,
}

impl ModelContextLimits {
    const CODEX_LARGE: Self = Self {
        context_window_tokens: 1_050_000,
        input_headroom_tokens: 128_000,
    };
    const CODEX_SPARK: Self = Self {
        context_window_tokens: 128_000,
        input_headroom_tokens: 8_000,
    };

    /// Returns the input-token budget after reserving output and overhead.
    #[must_use]
    pub const fn input_token_budget(self) -> u64 {
        self.context_window_tokens
            .saturating_sub(self.input_headroom_tokens)
    }
}

struct ModelDescriptor {
    context_limits: Option<ModelContextLimits>,
    description: &'static str,
    id: &'static str,
    supports_fast_mode: bool,
}

/// Session-level agent selection that keeps provider kind and model together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentSelection {
    kind: AgentKind,
    model: AgentModel,
}

impl AgentSelection {
    /// Creates a coherent session agent selection.
    ///
    /// If `model` is not supported by `kind`, the provider's default model is
    /// used so the selection never carries unrelated provider/model values.
    #[must_use]
    pub fn new(kind: AgentKind, model: AgentModel) -> Self {
        let model = if kind.supports_model(model) {
            model
        } else {
            kind.default_model()
        };

        Self { kind, model }
    }

    /// Returns the selected agent provider kind.
    #[must_use]
    pub fn kind(self) -> AgentKind {
        self.kind
    }

    /// Returns the selected agent model.
    #[must_use]
    pub fn model(self) -> AgentModel {
        self.model
    }

    /// Returns whether this exact provider/model pair supports Fast mode.
    #[must_use]
    pub fn supports_fast_mode(self) -> bool {
        self.model.descriptor().supports_fast_mode
    }

    /// Returns the compatible provider/model pair for one speed preference.
    ///
    /// Fast Claude requests require Opus, while incompatible Codex models move
    /// to the provider's default model. Providers without a speed control and
    /// already compatible selections remain unchanged.
    #[must_use]
    pub fn compatible_with_speed_mode(self, speed_mode: SpeedMode) -> Self {
        if speed_mode == SpeedMode::Normal || self.supports_fast_mode() {
            return self;
        }

        match self.kind {
            AgentKind::Claude => Self::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
            AgentKind::Codex => Self::new(AgentKind::Codex, AgentKind::Codex.default_model()),
            AgentKind::Antigravity | AgentKind::Gemini => self,
        }
    }
}

/// Human-readable metadata for slash-menu selectable items.
pub trait AgentSelectionMetadata {
    /// Returns a stable item name shown in menus.
    fn name(&self) -> &'static str;

    /// Returns a short descriptive subtitle shown in menus.
    fn description(&self) -> &'static str;
}

impl AgentModel {
    /// Returns the stable wire/model identifier used in persistence and CLI
    /// invocations.
    pub const fn as_str(self) -> &'static str {
        self.descriptor().id
    }

    /// Returns declared proactive compaction limits, if known for this model.
    #[must_use]
    pub const fn context_limits(self) -> Option<ModelContextLimits> {
        self.descriptor().context_limits
    }

    /// Returns the model identifier passed to provider transports.
    ///
    /// Antigravity and Gemini use the same raw Gemini CLI model ids; callers
    /// route those ids through the provider stored on [`AgentSelection`].
    pub fn provider_model_str(self) -> &'static str {
        self.as_str()
    }

    /// Parses one persisted model identifier and upgrades retired ids to
    /// their replacement models.
    ///
    /// Stored retired Gemini, Claude, and Codex ids are migrated forward
    /// through [`AgentModel::retired_replacement`] so existing projects and
    /// sessions continue loading after model retirements.
    /// Raw `gemini-*` ids parse to shared Gemini model variants; persisted
    /// session agents decide whether Gemini or Antigravity owns the session.
    ///
    /// # Errors
    /// Returns an error when `value` is not a known current or retired model
    /// identifier.
    pub fn parse_persisted(value: &str) -> Result<Self, String> {
        if let Some(replacement) = Self::retired_replacement(value) {
            return Ok(replacement);
        }

        value.parse()
    }

    /// Returns the current replacement model for a retired persisted model
    /// id, or `None` when `value` is not a retired id.
    ///
    /// This registry is the single source of truth for model retirement.
    /// Retiring a model means removing its selectable [`AgentModel`] variant
    /// and mapping its persisted id to the replacement model here. Retired
    /// ids never appear in selectable model lists; they stay in the database
    /// as history for finished sessions, while sessions that are still active
    /// are switched to the replacement automatically.
    #[must_use]
    pub fn retired_replacement(value: &str) -> Option<Self> {
        const RETIRED_MODEL_REPLACEMENTS: &[(&str, AgentModel)] = &[
            ("gemini-3-pro-preview", AgentModel::Gemini31Pro),
            ("gemini-3.1-pro", AgentModel::Gemini31Pro),
            ("gemini-3-flash-preview", AgentModel::Gemini38Flash),
            ("gemini-3.7-flash", AgentModel::Gemini38Flash),
            ("gemini-3.6-flash", AgentModel::Gemini38Flash),
            ("gemini-3.5-flash", AgentModel::Gemini35FlashLite),
            (
                "gemini-3.1-flash-lite-preview",
                AgentModel::Gemini35FlashLite,
            ),
            ("claude-fable-5", AgentModel::ClaudeFable51),
            ("claude-opus-5", AgentModel::ClaudeOpus55),
            ("gpt-6-sol", AgentModel::Gpt61Sol),
            ("gpt-5.6-sol", AgentModel::Gpt61Sol),
            ("gpt-5.6-luna", AgentModel::Gpt6Luna),
            ("claude-opus-4-8", AgentModel::ClaudeOpus55),
            ("claude-opus-4-6", AgentModel::ClaudeOpus55),
            ("claude-opus-4-7", AgentModel::ClaudeOpus55),
            ("claude-sonnet-4-6", AgentModel::ClaudeSonnet5),
            ("gpt-5.5", AgentModel::Gpt61Sol),
            ("gpt-5.4-mini", AgentModel::Gpt6Luna),
            ("gpt-5.4", AgentModel::Gpt61Sol),
            ("gpt-5.3-codex", AgentModel::Gpt61Sol),
            ("gpt-5.2-codex", AgentModel::Gpt53CodexSpark),
        ];

        RETIRED_MODEL_REPLACEMENTS
            .iter()
            .find(|(retired_id, _)| *retired_id == value)
            .map(|(_, replacement)| *replacement)
    }
}

/// Parses one persisted session agent/model pair without deriving the agent
/// from the model when the saved agent kind is available.
///
/// Existing databases did not persist `agent`, so rows with a missing or
/// invalid agent value fall back to a compatibility provider inferred from
/// the persisted model string. Ambiguous raw `gemini-*` rows default to
/// Antigravity because direct Gemini support had been removed before the new
/// `agent` column was introduced.
pub fn parse_persisted_session_agent_model(
    agent_value: Option<&str>,
    model_value: &str,
) -> AgentSelection {
    let parsed_agent = agent_value
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| value.parse::<AgentKind>().ok());

    if let Some(agent_kind) = parsed_agent {
        return parse_model_for_persisted_agent(agent_kind, model_value);
    }

    let model = AgentModel::parse_persisted(model_value)
        .unwrap_or_else(|_| AgentKind::Antigravity.default_model());
    let agent_kind = legacy_agent_kind_for_model_value(model_value, model);

    AgentSelection::new(agent_kind, model)
}

/// Parses a persisted model using the already persisted agent kind as the
/// source of truth for provider ownership.
fn parse_model_for_persisted_agent(agent_kind: AgentKind, model_value: &str) -> AgentSelection {
    if let Some(model) = agent_kind.parse_model(model_value) {
        return AgentSelection::new(agent_kind, model);
    }

    if let Ok(model) = AgentModel::parse_persisted(model_value)
        && agent_kind.supports_model(model)
    {
        return AgentSelection::new(agent_kind, model);
    }

    AgentSelection::new(agent_kind, agent_kind.default_model())
}

/// Returns the provider used for legacy rows that predate explicit session
/// agent persistence.
fn legacy_agent_kind_for_model_value(model_value: &str, model: AgentModel) -> AgentKind {
    if model_value.starts_with("claude-") {
        return AgentKind::Claude;
    }

    if model_value.starts_with("gpt-") {
        return AgentKind::Codex;
    }

    if model_value.starts_with("gemini-") {
        return AgentKind::Antigravity;
    }

    AgentKind::ALL
        .iter()
        .copied()
        .find(|agent_kind| agent_kind.supports_model(model))
        .unwrap_or(AgentKind::Antigravity)
}

/// Returns all selectable models owned by the provided agent kinds in stable
/// settings and slash-menu order.
#[must_use]
pub fn selectable_models_for_agent_kinds(agent_kinds: &[AgentKind]) -> Vec<AgentModel> {
    let mut models = Vec::new();
    for model in agent_kinds
        .iter()
        .flat_map(|agent_kind| agent_kind.models())
        .copied()
    {
        if !models.contains(&model) {
            models.push(model);
        }
    }

    models
}

/// Resolves one model against the currently available agent kinds.
///
/// When `model` is unsupported by every available provider, this prefers
/// `fallback_model` when any available provider supports it and otherwise falls
/// back to the first available provider default in `agent_kinds`. When no
/// providers are available, it returns `fallback_model` unchanged.
#[must_use]
pub fn resolve_model_for_available_agent_kinds(
    model: AgentModel,
    agent_kinds: &[AgentKind],
    fallback_model: AgentModel,
) -> AgentModel {
    if agent_kinds
        .iter()
        .any(|agent_kind| agent_kind.supports_model(model))
    {
        return model;
    }

    if agent_kinds
        .iter()
        .any(|agent_kind| agent_kind.supports_model(fallback_model))
    {
        return fallback_model;
    }

    agent_kinds
        .first()
        .copied()
        .map_or(fallback_model, AgentKind::default_model)
}

/// Resolves a provider kind that can run `model` from the available provider
/// list.
///
/// Shared Gemini model ids can be run by both Gemini and Antigravity. This
/// helper uses the order of `agent_kinds` as the tie-breaker and returns
/// `fallback_agent_kind` when no available provider supports `model`.
#[must_use]
pub fn resolve_agent_kind_for_model(
    model: AgentModel,
    agent_kinds: &[AgentKind],
    fallback_agent_kind: AgentKind,
) -> AgentKind {
    agent_kinds
        .iter()
        .copied()
        .find(|agent_kind| agent_kind.supports_model(model))
        .unwrap_or(fallback_agent_kind)
}

/// Resolves an [`AgentSelection`] for a model-only setting.
///
/// `preferred_agent_kind` is kept when it can run `model`, which preserves the
/// current session provider for shared Gemini model ids. Otherwise the
/// available provider order decides the owning provider, falling back to
/// `preferred_agent_kind` when no available provider supports the model.
#[must_use]
pub fn resolve_agent_selection_for_model(
    model: AgentModel,
    preferred_agent_kind: AgentKind,
    agent_kinds: &[AgentKind],
) -> AgentSelection {
    let agent_kind = if preferred_agent_kind.supports_model(model) {
        preferred_agent_kind
    } else {
        resolve_agent_kind_for_model(model, agent_kinds, preferred_agent_kind)
    };

    AgentSelection::new(agent_kind, model)
}

/// Resolves the agent kind used for prompt-side `/model` selection.
///
/// This preserves `session_agent_kind` when that backend is still available
/// and otherwise falls back to the first available backend. When no backends
/// are available, it returns `None`.
#[must_use]
pub fn resolve_prompt_model_agent_kind(
    session_agent_kind: AgentKind,
    agent_kinds: &[AgentKind],
) -> Option<AgentKind> {
    if agent_kinds.contains(&session_agent_kind) {
        return Some(session_agent_kind);
    }

    agent_kinds.first().copied()
}

impl FromStr for AgentModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|model| model.as_str() == value)
            .ok_or_else(|| format!("unknown model: {value}"))
    }
}

impl AgentSelectionMetadata for AgentModel {
    fn name(&self) -> &'static str {
        (*self).provider_model_str()
    }

    fn description(&self) -> &'static str {
        self.descriptor().description
    }
}

impl AgentKind {
    /// All available agent kinds, in display order.
    pub const ALL: &[AgentKind] = &[
        AgentKind::Gemini,
        AgentKind::Antigravity,
        AgentKind::Claude,
        AgentKind::Codex,
    ];

    /// Returns the provider CLI executable name.
    pub fn executable_name(self) -> &'static str {
        match self {
            Self::Antigravity => "agy",
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Returns the default model for this agent kind.
    pub fn default_model(self) -> AgentModel {
        match self {
            Self::Antigravity | Self::Gemini => AgentModel::Gemini31Pro,
            Self::Claude => AgentModel::ClaudeFable51,
            Self::Codex => AgentModel::Gpt61Sol,
        }
    }

    /// Returns the model string when it belongs to this agent kind.
    pub fn model_str(self, model: AgentModel) -> Option<&'static str> {
        if !self.supports_model(model) {
            return None;
        }

        Some(model.as_str())
    }

    /// Parses a provider-specific model string for this agent kind.
    pub fn parse_model(self, value: &str) -> Option<AgentModel> {
        let model = value.parse::<AgentModel>().ok()?;
        if !self.supports_model(model) {
            return None;
        }

        Some(model)
    }

    /// Returns whether this provider can run the given model.
    pub fn supports_model(self, model: AgentModel) -> bool {
        self.models().contains(&model)
    }

    /// Returns whether this provider exposes a response-speed control.
    ///
    /// Claude and Codex accept a per-turn speed selection, so `/speed` offers
    /// the picker and session surfaces display the active [`SpeedMode`].
    /// Gemini and Antigravity have no equivalent control, so neither the
    /// command nor the speed display is offered for them.
    ///
    /// [`SpeedMode`]: ag_contracts::SpeedMode
    pub fn supports_speed_mode(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }
}

impl AgentSelectionMetadata for AgentKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Antigravity => "antigravity",
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Antigravity => "Google Antigravity CLI agent.",
            Self::Gemini => "Google Gemini CLI agent.",
            Self::Claude => "Anthropic Claude Code agent.",
            Self::Codex => "OpenAI Codex CLI agent.",
        }
    }
}

impl AgentSelectionMetadata for ReasoningLevel {
    fn name(&self) -> &'static str {
        (*self).as_str()
    }

    fn description(&self) -> &'static str {
        (*self).description()
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "antigravity" | "agy" => Ok(Self::Antigravity),
            "gemini" => Ok(Self::Gemini),
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!("unknown agent kind: {other}")),
        }
    }
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;

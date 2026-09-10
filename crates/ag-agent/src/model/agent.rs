use std::fmt;
use std::str::FromStr;

use super::session::SpeedMode;

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

/// Automatic update and version probe state for one locally runnable agent
/// CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCliVersion {
    /// Startup update plus version detection is still running in the
    /// background.
    Loading,
    /// Version detection finished, but the executable did not report a usable
    /// version.
    Unknown,
    /// Version detection finished with a parsed display value.
    Value(String),
}

/// One locally runnable agent CLI and the installed version refreshed at
/// startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliInfo {
    /// Executable name used to launch the provider CLI.
    pub executable_name: &'static str,
    /// Agent provider family backed by the executable.
    pub kind: AgentKind,
    /// Current automatic update and version probe state for this executable.
    pub version: AgentCliVersion,
}

impl AgentCliInfo {
    /// Creates one CLI availability row for a provider and optional version.
    pub fn new(kind: AgentKind, version: Option<String>) -> Self {
        Self {
            executable_name: kind.executable_name(),
            kind,
            version: version.map_or(AgentCliVersion::Unknown, AgentCliVersion::Value),
        }
    }

    /// Creates one CLI availability row whose update/version refresh is still
    /// loading.
    pub fn loading(kind: AgentKind) -> Self {
        Self {
            executable_name: kind.executable_name(),
            kind,
            version: AgentCliVersion::Loading,
        }
    }

    /// Builds unknown-version CLI rows for an existing provider availability
    /// list.
    pub fn from_kinds(agent_kinds: &[AgentKind]) -> Vec<Self> {
        agent_kinds
            .iter()
            .copied()
            .map(|agent_kind| Self::new(agent_kind, None))
            .collect()
    }

    /// Builds loading CLI rows for an existing provider availability list
    /// while the background update/version refresh is running.
    pub fn loading_from_kinds(agent_kinds: &[AgentKind]) -> Vec<Self> {
        agent_kinds.iter().copied().map(Self::loading).collect()
    }
}

/// Supported agent model names across all providers.
///
/// Gemini model ids are shared by the direct Gemini and Antigravity providers,
/// so provider ownership lives on [`AgentSelection`] rather than on these
/// variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentModel {
    /// Codex Astra model backed by `gpt-6-astra`.
    Gpt6Astra,
    /// Codex Sol model backed by `gpt-5.6-sol`.
    Gpt56Sol,
    /// Codex Terra model backed by `gpt-5.6-terra`.
    Gpt56Terra,
    /// Codex Luna model backed by `gpt-5.6-luna`.
    Gpt56Luna,
    /// Fast Gemini model backed by `gemini-3.8-flash`.
    Gemini38Flash,
    /// Lightweight Gemini model backed by `gemini-3.5-flash-lite`.
    Gemini35FlashLite,
    /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
    Gemini31Pro,
    /// Codex spark model backed by `gpt-5.3-codex-spark`.
    Gpt53CodexSpark,
    /// Claude Opus model backed by `claude-opus-5`.
    ClaudeOpus5,
    /// Claude Sonnet model backed by `claude-sonnet-5`.
    ClaudeSonnet5,
    /// Claude Fable model backed by `claude-fable-5`.
    ClaudeFable5,
    /// Claude Haiku model backed by `claude-haiku-4-5-20251001`.
    ClaudeHaiku4520251001,
}

/// Supported reasoning-effort levels for task execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningLevel {
    /// Low reasoning effort for faster responses.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort for deeper reasoning.
    #[default]
    High,
    /// Extra-high reasoning effort for deeper analysis.
    XHigh,
    /// Maximum reasoning effort for the hardest tasks.
    Max,
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
        matches!(
            (self.kind, self.model),
            (AgentKind::Claude, AgentModel::ClaudeOpus5)
                | (
                    AgentKind::Codex,
                    AgentModel::Gpt6Astra
                        | AgentModel::Gpt56Sol
                        | AgentModel::Gpt56Terra
                        | AgentModel::Gpt56Luna
                )
        )
    }

    /// Returns the compatible provider/model pair for one speed preference.
    ///
    /// Fast Claude requests require Opus, while Codex Spark requests move to
    /// the provider's default model. Providers without a speed control and
    /// already compatible selections remain unchanged.
    #[must_use]
    pub fn compatible_with_speed_mode(self, speed_mode: SpeedMode) -> Self {
        if speed_mode == SpeedMode::Normal || self.supports_fast_mode() {
            return self;
        }

        match self.kind {
            AgentKind::Claude => Self::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
            AgentKind::Codex if self.model == AgentModel::Gpt53CodexSpark => {
                Self::new(AgentKind::Codex, AgentModel::Gpt56Sol)
            }
            AgentKind::Antigravity | AgentKind::Gemini | AgentKind::Codex => self,
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpt6Astra => "gpt-6-astra",
            Self::Gpt56Sol => "gpt-5.6-sol",
            Self::Gpt56Terra => "gpt-5.6-terra",
            Self::Gpt56Luna => "gpt-5.6-luna",
            Self::Gemini38Flash => "gemini-3.8-flash",
            Self::Gemini35FlashLite => "gemini-3.5-flash-lite",
            Self::Gemini31Pro => "gemini-3.1-pro-preview",
            Self::Gpt53CodexSpark => "gpt-5.3-codex-spark",
            Self::ClaudeOpus5 => "claude-opus-5",
            Self::ClaudeSonnet5 => "claude-sonnet-5",
            Self::ClaudeFable5 => "claude-fable-5",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
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
            ("claude-opus-4-8", AgentModel::ClaudeOpus5),
            ("claude-opus-4-6", AgentModel::ClaudeOpus5),
            ("claude-opus-4-7", AgentModel::ClaudeOpus5),
            ("claude-sonnet-4-6", AgentModel::ClaudeSonnet5),
            ("gpt-5.5", AgentModel::Gpt56Sol),
            ("gpt-5.4-mini", AgentModel::Gpt56Luna),
            ("gpt-5.4", AgentModel::Gpt56Sol),
            ("gpt-5.3-codex", AgentModel::Gpt56Sol),
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

impl ReasoningLevel {
    /// All selectable reasoning-effort levels in UI display order.
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max];

    /// Returns the stable persisted identifier for this level.
    ///
    /// This value is stored in the database and remains independent from any
    /// provider-specific transport string changes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Returns the Codex reasoning-effort identifier for this level.
    pub fn codex(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Returns the Antigravity `--effort` value for this level.
    ///
    /// Antigravity accepts `low`, `medium`, and `high`, so higher generic
    /// reasoning levels map to its highest supported value.
    pub fn antigravity(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::XHigh | Self::Max => "high",
        }
    }

    /// Returns the Claude `--effort` value for this level.
    ///
    /// Maps `XHigh` and `Max` to `"max"`, which is currently only supported on
    /// `claude-opus-5`. The Claude CLI enforces this
    /// restriction and will surface an error for other models.
    pub fn claude(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh | Self::Max => "max",
        }
    }

    /// Returns a short UI description for this reasoning level.
    pub fn description(self) -> &'static str {
        match self {
            Self::Low => "Fastest responses with lighter reasoning.",
            Self::Medium => "Balanced speed and reasoning depth.",
            Self::High => "Deeper reasoning for tougher tasks.",
            Self::XHigh => "Extra-high reasoning for complex tasks.",
            Self::Max => "Maximum reasoning effort for the hardest tasks.",
        }
    }
}

impl FromStr for ReasoningLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(format!("unknown reasoning level: {other}")),
        }
    }
}

impl FromStr for AgentModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini-3.8-flash" => Ok(Self::Gemini38Flash),
            "gemini-3.5-flash-lite" => Ok(Self::Gemini35FlashLite),
            "gemini-3.1-pro-preview" => Ok(Self::Gemini31Pro),
            "gpt-6-astra" => Ok(Self::Gpt6Astra),
            "gpt-5.6-sol" => Ok(Self::Gpt56Sol),
            "gpt-5.6-terra" => Ok(Self::Gpt56Terra),
            "gpt-5.6-luna" => Ok(Self::Gpt56Luna),
            "gpt-5.3-codex-spark" => Ok(Self::Gpt53CodexSpark),
            "claude-opus-5" => Ok(Self::ClaudeOpus5),
            "claude-sonnet-5" => Ok(Self::ClaudeSonnet5),
            "claude-fable-5" => Ok(Self::ClaudeFable5),
            "claude-haiku-4-5-20251001" => Ok(Self::ClaudeHaiku4520251001),
            other => Err(format!("unknown model: {other}")),
        }
    }
}

impl AgentSelectionMetadata for AgentModel {
    fn name(&self) -> &'static str {
        (*self).provider_model_str()
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Gemini31Pro => "Higher-quality Gemini model for deeper reasoning.",
            Self::Gemini38Flash => "Fast Gemini model for agentic and multimodal tasks.",
            Self::Gemini35FlashLite => {
                "Lightweight Gemini model for fast, cost-conscious workloads."
            }
            Self::Gpt6Astra => "Most capable Codex model for the hardest end-to-end work.",
            Self::Gpt56Sol => "Flagship Codex model for complex professional work.",
            Self::Gpt56Terra => "Current Codex model for balanced coding performance.",
            Self::Gpt56Luna => "Current Codex model for lighter coding iterations.",
            Self::Gpt53CodexSpark => "Codex spark model for quick coding iterations.",
            Self::ClaudeOpus5 => "Latest Claude Opus model for complex tasks.",
            Self::ClaudeSonnet5 => "Balanced Claude model for quality and latency.",
            Self::ClaudeFable5 => "Claude Fable model for creative, narrative-heavy tasks.",
            Self::ClaudeHaiku4520251001 => "Fast Claude model for lighter tasks.",
        }
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
            Self::Claude => AgentModel::ClaudeFable5,
            Self::Codex => AgentModel::Gpt56Sol,
        }
    }

    /// Returns the model string when it belongs to this agent kind.
    pub fn model_str(self, model: AgentModel) -> Option<&'static str> {
        if !self.supports_model(model) {
            return None;
        }

        Some(model.as_str())
    }

    /// Returns the curated model list for this agent kind.
    pub fn models(self) -> &'static [AgentModel] {
        const ANTIGRAVITY_MODELS: &[AgentModel] = &[
            AgentModel::Gemini31Pro,
            AgentModel::Gemini38Flash,
            AgentModel::Gemini35FlashLite,
        ];
        const GEMINI_MODELS: &[AgentModel] = &[
            AgentModel::Gemini31Pro,
            AgentModel::Gemini38Flash,
            AgentModel::Gemini35FlashLite,
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::ClaudeFable5,
            AgentModel::ClaudeOpus5,
            AgentModel::ClaudeSonnet5,
            AgentModel::ClaudeHaiku4520251001,
        ];
        const CODEX_MODELS: &[AgentModel] = &[
            AgentModel::Gpt6Astra,
            AgentModel::Gpt56Sol,
            AgentModel::Gpt56Terra,
            AgentModel::Gpt56Luna,
            AgentModel::Gpt53CodexSpark,
        ];

        match self {
            Self::Antigravity => ANTIGRAVITY_MODELS,
            Self::Gemini => GEMINI_MODELS,
            Self::Claude => CLAUDE_MODELS,
            Self::Codex => CODEX_MODELS,
        }
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
    /// [`SpeedMode`]: crate::model::session::SpeedMode
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

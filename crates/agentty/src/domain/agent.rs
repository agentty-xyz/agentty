use std::fmt;
use std::str::FromStr;

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
    /// Codex model backed by `gpt-5.5`.
    Gpt55,
    /// Fast Gemini preview model backed by `gemini-3-flash-preview`.
    Gemini3FlashPreview,
    /// Fast Gemini model backed by `gemini-3.5-flash`.
    Gemini35Flash,
    /// Lightweight Gemini preview model backed by
    /// `gemini-3.1-flash-lite-preview`.
    Gemini31FlashLitePreview,
    /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
    Gemini31ProPreview,
    /// Smaller Codex model backed by `gpt-5.4-mini`.
    Gpt54Mini,
    /// Codex spark model backed by `gpt-5.3-codex-spark`.
    Gpt53CodexSpark,
    /// Claude Opus model backed by `claude-opus-4-8`.
    ClaudeOpus48,
    /// Claude Sonnet model backed by `claude-sonnet-5`.
    ClaudeSonnet5,
    /// Claude Fable model backed by `claude-fable-5`.
    ClaudeFable5,
    /// Claude Haiku model backed by `claude-haiku-4-5-20251001`.
    ClaudeHaiku4520251001,
}

/// Session-level agent selection that keeps provider kind and model together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentSelection {
    kind: AgentKind,
    model: AgentModel,
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
            Self::Gpt55 => "gpt-5.5",
            Self::Gemini3FlashPreview => "gemini-3-flash-preview",
            Self::Gemini35Flash => "gemini-3.5-flash",
            Self::Gemini31FlashLitePreview => "gemini-3.1-flash-lite-preview",
            Self::Gemini31ProPreview => "gemini-3.1-pro-preview",
            Self::Gpt54Mini => "gpt-5.4-mini",
            Self::Gpt53CodexSpark => "gpt-5.3-codex-spark",
            Self::ClaudeOpus48 => "claude-opus-4-8",
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

    /// Parses one persisted model identifier and upgrades retired aliases.
    ///
    /// Stored retired Claude Opus, Claude Sonnet, and Codex aliases are
    /// migrated forward so existing projects and sessions continue loading
    /// after model removals.
    /// Raw `gemini-*` ids parse to shared Gemini model variants; persisted
    /// session agents decide whether Gemini or Antigravity owns the session.
    pub(crate) fn parse_persisted(value: &str) -> Result<Self, String> {
        match value {
            "claude-opus-4-6" | "claude-opus-4-7" => Ok(Self::ClaudeOpus48),
            "claude-sonnet-4-6" => Ok(Self::ClaudeSonnet5),
            "gpt-5.4" => Ok(Self::Gpt55),
            _ => value.parse(),
        }
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
pub(crate) fn parse_persisted_session_agent_model(
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
    /// All selectable reasoning-effort levels in UI cycle order.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::XHigh];

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
        }
    }

    /// Returns the Codex reasoning-effort identifier for this level.
    pub fn codex(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    /// Returns the Claude `--effort` value for this level.
    ///
    /// Maps `XHigh` to `"max"`, which is currently only supported on
    /// `claude-opus-4-8`. The Claude CLI enforces this restriction and will
    /// surface an error for other models.
    pub fn claude(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "max",
        }
    }

    /// Returns a short UI description for this reasoning level.
    pub fn description(self) -> &'static str {
        match self {
            Self::Low => "Fastest responses with lighter reasoning.",
            Self::Medium => "Balanced speed and reasoning depth.",
            Self::High => "Deeper reasoning for tougher tasks.",
            Self::XHigh => "Maximum reasoning effort for the hardest tasks.",
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
            other => Err(format!("unknown reasoning level: {other}")),
        }
    }
}

impl FromStr for AgentModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gemini-3-flash-preview" => Ok(Self::Gemini3FlashPreview),
            "gemini-3.5-flash" => Ok(Self::Gemini35Flash),
            "gemini-3.1-flash-lite-preview" => Ok(Self::Gemini31FlashLitePreview),
            "gemini-3.1-pro-preview" => Ok(Self::Gemini31ProPreview),
            "gpt-5.5" => Ok(Self::Gpt55),
            "gpt-5.4-mini" => Ok(Self::Gpt54Mini),
            "gpt-5.3-codex-spark" => Ok(Self::Gpt53CodexSpark),
            "claude-opus-4-8" => Ok(Self::ClaudeOpus48),
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
            Self::Gemini31ProPreview => "Higher-quality Gemini model for deeper reasoning.",
            Self::Gemini35Flash => "Fast Gemini model for current Flash workloads.",
            Self::Gemini31FlashLitePreview => {
                "Lightweight Gemini model for fast, cost-conscious iterations."
            }
            Self::Gemini3FlashPreview => "Fast Gemini model for quick iterations.",
            Self::Gpt55 => "Newer Codex model with stronger coding performance when available.",
            Self::Gpt54Mini => "Small, fast Codex model for simpler coding tasks.",
            Self::Gpt53CodexSpark => "Codex spark model for quick coding iterations.",
            Self::ClaudeOpus48 => "Latest Claude Opus model for complex tasks.",
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
            Self::Antigravity | Self::Gemini => AgentModel::Gemini31ProPreview,
            Self::Claude => AgentModel::ClaudeOpus48,
            Self::Codex => AgentModel::Gpt55,
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
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini35Flash,
            AgentModel::Gemini31FlashLitePreview,
            AgentModel::Gemini3FlashPreview,
        ];
        const GEMINI_MODELS: &[AgentModel] = &[
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini35Flash,
            AgentModel::Gemini31FlashLitePreview,
            AgentModel::Gemini3FlashPreview,
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::ClaudeOpus48,
            AgentModel::ClaudeSonnet5,
            AgentModel::ClaudeFable5,
            AgentModel::ClaudeHaiku4520251001,
        ];
        const CODEX_MODELS: &[AgentModel] = &[
            AgentModel::Gpt55,
            AgentModel::Gpt54Mini,
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
mod tests {
    use super::*;

    #[test]
    /// Ensures model parsing is constrained to the selected provider.
    fn test_parse_model_returns_none_for_models_from_other_providers() {
        // Arrange
        let claude_kind = AgentKind::Claude;
        let antigravity_model = AgentModel::Gemini3FlashPreview.as_str();

        // Act
        let parsed_model = claude_kind.parse_model(antigravity_model);

        // Assert
        assert_eq!(parsed_model, None);
    }

    #[test]
    /// Ensures `gpt-5.5` parses as a Codex model.
    fn test_parse_model_parses_gpt_55() {
        // Arrange
        let codex_kind = AgentKind::Codex;

        // Act
        let parsed_model = codex_kind.parse_model("gpt-5.5");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gpt55));
    }

    #[test]
    /// Ensures Antigravity parses raw Gemini model ids as shared Gemini model
    /// selections.
    fn test_parse_model_parses_antigravity() {
        // Arrange
        let antigravity_kind = AgentKind::Antigravity;

        // Act
        let parsed_model = antigravity_kind.parse_model("gemini-3.5-flash");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gemini35Flash));
    }

    #[test]
    /// Ensures direct Gemini parses the same raw Gemini model ids as
    /// Antigravity.
    fn test_parse_model_parses_direct_gemini() {
        // Arrange
        let gemini_kind = AgentKind::Gemini;

        // Act
        let parsed_model = gemini_kind.parse_model("gemini-3.5-flash");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gemini35Flash));
    }

    #[test]
    /// Ensures retired `gpt-5.4` no longer parses as a selectable model.
    fn test_parse_model_rejects_retired_gpt_54() {
        // Arrange
        let codex_kind = AgentKind::Codex;

        // Act
        let parsed_gpt_54 = codex_kind.parse_model("gpt-5.4");

        // Assert
        assert_eq!(parsed_gpt_54, None);
    }

    #[test]
    /// Ensures `gpt-5.4-mini` parses as a Codex model.
    fn test_parse_model_parses_gpt_54_mini() {
        // Arrange
        let codex_kind = AgentKind::Codex;

        // Act
        let parsed_model = codex_kind.parse_model("gpt-5.4-mini");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gpt54Mini));
    }

    #[test]
    /// Ensures retired Claude aliases no longer parse as selectable
    /// models.
    fn test_parse_model_rejects_retired_claude_aliases() {
        // Arrange
        let claude_kind = AgentKind::Claude;

        // Act
        let parsed_opus_46 = claude_kind.parse_model("claude-opus-4-6");
        let parsed_opus_47 = claude_kind.parse_model("claude-opus-4-7");
        let parsed_sonnet_46 = claude_kind.parse_model("claude-sonnet-4-6");

        // Assert
        assert_eq!(parsed_opus_46, None);
        assert_eq!(parsed_opus_47, None);
        assert_eq!(parsed_sonnet_46, None);
    }

    #[test]
    /// Ensures current Claude model ids parse as supported Claude models.
    fn test_parse_model_parses_current_claude_models() {
        // Arrange
        let claude_kind = AgentKind::Claude;

        // Act
        let parsed_sonnet_5 = claude_kind.parse_model("claude-sonnet-5");
        let parsed_fable_5 = claude_kind.parse_model("claude-fable-5");
        let parsed_haiku_45 = claude_kind.parse_model("claude-haiku-4-5-20251001");

        // Assert
        assert_eq!(parsed_sonnet_5, Some(AgentModel::ClaudeSonnet5));
        assert_eq!(parsed_fable_5, Some(AgentModel::ClaudeFable5));
        assert_eq!(parsed_haiku_45, Some(AgentModel::ClaudeHaiku4520251001));
    }

    #[test]
    /// Ensures persisted model ids parse or migrate to supported
    /// models.
    fn test_parse_persisted_handles_supported_and_retired_models() {
        // Arrange

        // Act
        let parsed_opus_46 = AgentModel::parse_persisted("claude-opus-4-6");
        let parsed_opus_47 = AgentModel::parse_persisted("claude-opus-4-7");
        let parsed_sonnet_46 = AgentModel::parse_persisted("claude-sonnet-4-6");
        let parsed_sonnet_5 = AgentModel::parse_persisted("claude-sonnet-5");
        let parsed_gpt_54 = AgentModel::parse_persisted("gpt-5.4");
        let parsed_gemini_35_flash = AgentModel::parse_persisted("gemini-3.5-flash");

        // Assert
        assert_eq!(parsed_opus_46, Ok(AgentModel::ClaudeOpus48));
        assert_eq!(parsed_opus_47, Ok(AgentModel::ClaudeOpus48));
        assert_eq!(parsed_sonnet_46, Ok(AgentModel::ClaudeSonnet5));
        assert_eq!(parsed_sonnet_5, Ok(AgentModel::ClaudeSonnet5));
        assert_eq!(parsed_gpt_54, Ok(AgentModel::Gpt55));
        assert_eq!(parsed_gemini_35_flash, Ok(AgentModel::Gemini35Flash));
    }

    #[test]
    /// Ensures persisted session agent values constrain model parsing instead
    /// of deriving provider ownership from the model string.
    fn test_parse_persisted_session_agent_model_prefers_saved_agent() {
        // Arrange

        // Act
        let selection = parse_persisted_session_agent_model(Some("codex"), "gemini-3.5-flash");

        // Assert
        assert_eq!(selection.kind(), AgentKind::Codex);
        assert_eq!(selection.model(), AgentKind::Codex.default_model());
    }

    #[test]
    /// Ensures Antigravity-persisted sessions keep Antigravity ownership for
    /// raw Gemini model ids shared with the direct Gemini backend.
    fn test_parse_persisted_session_agent_model_preserves_antigravity_models() {
        // Arrange

        // Act
        let selection =
            parse_persisted_session_agent_model(Some("antigravity"), "gemini-3.5-flash");

        // Assert
        assert_eq!(selection.kind(), AgentKind::Antigravity);
        assert_eq!(selection.model(), AgentModel::Gemini35Flash);
    }

    #[test]
    /// Ensures older persisted rows without `agent` still load through the
    /// legacy model-derived compatibility path.
    fn test_parse_persisted_session_agent_model_supports_legacy_rows() {
        // Arrange

        // Act
        let selection = parse_persisted_session_agent_model(None, "claude-opus-4-6");

        // Assert
        assert_eq!(selection.kind(), AgentKind::Claude);
        assert_eq!(selection.model(), AgentModel::ClaudeOpus48);
    }

    #[test]
    /// Ensures older `gemini-*` rows without `agent` keep the post-removal
    /// Antigravity compatibility default.
    fn test_parse_persisted_session_agent_model_defaults_legacy_gemini_to_antigravity() {
        // Arrange

        // Act
        let selection = parse_persisted_session_agent_model(None, "gemini-3.5-flash");

        // Assert
        assert_eq!(selection.kind(), AgentKind::Antigravity);
        assert_eq!(selection.model(), AgentModel::Gemini35Flash);
    }

    #[test]
    /// Ensures reasoning-level parsing accepts all supported persisted values.
    fn test_reasoning_level_from_str_parses_supported_values() {
        // Arrange

        // Act
        let low_level = "low".parse::<ReasoningLevel>();
        let medium_level = "medium".parse::<ReasoningLevel>();
        let high_level = "high".parse::<ReasoningLevel>();
        let xhigh_level = "xhigh".parse::<ReasoningLevel>();

        // Assert
        assert_eq!(low_level, Ok(ReasoningLevel::Low));
        assert_eq!(medium_level, Ok(ReasoningLevel::Medium));
        assert_eq!(high_level, Ok(ReasoningLevel::High));
        assert_eq!(xhigh_level, Ok(ReasoningLevel::XHigh));
    }

    #[test]
    /// Ensures unsupported reasoning values return a parse error.
    fn test_reasoning_level_from_str_rejects_unknown_values() {
        // Arrange

        // Act
        let parse_result = "minimal".parse::<ReasoningLevel>();

        // Assert
        assert!(parse_result.is_err());
    }

    #[test]
    /// Ensures Codex models are supported by Codex only.
    fn test_codex_models_are_supported_by_codex() {
        // Arrange
        let models = [
            AgentModel::Gpt55,
            AgentModel::Gpt54Mini,
            AgentModel::Gpt53CodexSpark,
        ];

        // Act
        let supported = models.map(|model| AgentKind::Codex.supports_model(model));
        let unsupported = models.map(|model| AgentKind::Claude.supports_model(model));

        // Assert
        assert_eq!(supported, [true; 3]);
        assert_eq!(unsupported, [false; 3]);
    }

    #[test]
    /// Ensures Claude models are supported by Claude only.
    fn test_claude_models_are_supported_by_claude() {
        // Arrange
        let models = [
            AgentModel::ClaudeOpus48,
            AgentModel::ClaudeSonnet5,
            AgentModel::ClaudeFable5,
            AgentModel::ClaudeHaiku4520251001,
        ];

        // Act
        let supported = models.map(|model| AgentKind::Claude.supports_model(model));
        let unsupported = models.map(|model| AgentKind::Codex.supports_model(model));

        // Assert
        assert_eq!(supported, [true; 4]);
        assert_eq!(unsupported, [false; 4]);
    }

    #[test]
    /// Ensures shared Gemini models are supported by both Google providers.
    fn test_gemini_models_are_supported_by_gemini_and_antigravity() {
        // Arrange
        let models = [
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini35Flash,
            AgentModel::Gemini31FlashLitePreview,
            AgentModel::Gemini3FlashPreview,
        ];

        // Act
        let antigravity_supported =
            models.map(|model| AgentKind::Antigravity.supports_model(model));
        let gemini_supported = models.map(|model| AgentKind::Gemini.supports_model(model));
        let codex_supported = models.map(|model| AgentKind::Codex.supports_model(model));

        // Assert
        assert_eq!(antigravity_supported, [true; 4]);
        assert_eq!(gemini_supported, [true; 4]);
        assert_eq!(codex_supported, [false; 4]);
    }

    #[test]
    /// Ensures Antigravity model selections pass raw Gemini ids to provider
    /// transports.
    fn test_antigravity_provider_model_str_returns_raw_gemini_model() {
        // Arrange
        let model = AgentModel::Gemini31FlashLitePreview;

        // Act
        let persisted_model = model.as_str();
        let provider_model = model.provider_model_str();

        // Assert
        assert_eq!(persisted_model, "gemini-3.1-flash-lite-preview");
        assert_eq!(provider_model, "gemini-3.1-flash-lite-preview");
    }

    #[test]
    /// Ensures `ReasoningLevel::claude()` maps all levels to the correct
    /// Claude `--effort` values, including `XHigh` → `"max"`.
    fn test_reasoning_level_claude_maps_all_levels() {
        // Arrange / Act / Assert
        assert_eq!(ReasoningLevel::Low.claude(), "low");
        assert_eq!(ReasoningLevel::Medium.claude(), "medium");
        assert_eq!(ReasoningLevel::High.claude(), "high");
        assert_eq!(ReasoningLevel::XHigh.claude(), "max");
    }

    #[test]
    /// Ensures persisted reasoning identifiers stay stable even if provider
    /// transport names change in the future.
    fn test_reasoning_level_as_str_returns_stable_persisted_values() {
        // Arrange / Act / Assert
        assert_eq!(ReasoningLevel::Low.as_str(), "low");
        assert_eq!(ReasoningLevel::Medium.as_str(), "medium");
        assert_eq!(ReasoningLevel::High.as_str(), "high");
        assert_eq!(ReasoningLevel::XHigh.as_str(), "xhigh");
    }

    #[test]
    /// Ensures selectable-model ordering follows the provided provider order.
    fn test_selectable_models_for_agent_kinds_uses_provider_order() {
        // Arrange
        let agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];

        // Act
        let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

        // Assert
        assert_eq!(
            selectable_models,
            vec![
                AgentModel::Gpt55,
                AgentModel::Gpt54Mini,
                AgentModel::Gpt53CodexSpark,
                AgentModel::Gemini31ProPreview,
                AgentModel::Gemini35Flash,
                AgentModel::Gemini31FlashLitePreview,
                AgentModel::Gemini3FlashPreview,
            ]
        );
    }

    #[test]
    /// Ensures shared Gemini models appear once even when both Google
    /// providers are available.
    fn test_selectable_models_for_agent_kinds_deduplicates_shared_models() {
        // Arrange
        let agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

        // Act
        let selectable_models = selectable_models_for_agent_kinds(&agent_kinds);

        // Assert
        assert_eq!(
            selectable_models,
            vec![
                AgentModel::Gemini31ProPreview,
                AgentModel::Gemini35Flash,
                AgentModel::Gemini31FlashLitePreview,
                AgentModel::Gemini3FlashPreview,
            ]
        );
    }

    #[test]
    /// Ensures unavailable models fall back to an available preferred model
    /// when possible.
    fn test_resolve_model_for_available_agent_kinds_prefers_available_fallback() {
        // Arrange
        let unavailable_model = AgentModel::ClaudeOpus48;
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];
        let fallback_model = AgentModel::Gpt55;

        // Act
        let resolved_model = resolve_model_for_available_agent_kinds(
            unavailable_model,
            &available_agent_kinds,
            fallback_model,
        );

        // Assert
        assert_eq!(resolved_model, AgentModel::Gpt55);
    }

    #[test]
    /// Ensures unavailable models fall back to the first available provider
    /// default when the preferred fallback is also unavailable.
    fn test_resolve_model_for_available_agent_kinds_uses_first_available_default() {
        // Arrange
        let unavailable_model = AgentModel::ClaudeOpus48;
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Antigravity];
        let unavailable_fallback_model = AgentModel::ClaudeSonnet5;

        // Act
        let resolved_model = resolve_model_for_available_agent_kinds(
            unavailable_model,
            &available_agent_kinds,
            unavailable_fallback_model,
        );

        // Assert
        assert_eq!(resolved_model, AgentKind::Codex.default_model());
    }

    #[test]
    /// Ensures model-only settings preserve a preferred provider when it can
    /// run the selected model.
    fn test_resolve_agent_selection_for_model_preserves_preferred_shared_provider() {
        // Arrange
        let model = AgentModel::Gemini35Flash;
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

        // Act
        let resolved_selection = resolve_agent_selection_for_model(
            model,
            AgentKind::Antigravity,
            &available_agent_kinds,
        );

        // Assert
        assert_eq!(
            resolved_selection,
            AgentSelection::new(AgentKind::Antigravity, model)
        );
    }

    #[test]
    /// Ensures model-only settings use available provider order when the
    /// preferred provider cannot run the selected model.
    fn test_resolve_agent_selection_for_model_uses_available_provider_order() {
        // Arrange
        let model = AgentModel::Gemini35Flash;
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Antigravity];

        // Act
        let resolved_selection =
            resolve_agent_selection_for_model(model, AgentKind::Codex, &available_agent_kinds);

        // Assert
        assert_eq!(
            resolved_selection,
            AgentSelection::new(AgentKind::Gemini, model)
        );
    }

    #[test]
    /// Ensures prompt model selection keeps the current backend when it is
    /// still available locally.
    fn test_resolve_prompt_model_agent_kind_prefers_current_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Antigravity, AgentKind::Codex];

        // Act
        let resolved_agent_kind =
            resolve_prompt_model_agent_kind(AgentKind::Codex, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent_kind, Some(AgentKind::Codex));
    }

    #[test]
    /// Ensures prompt model selection falls back to the first locally
    /// available backend when the current backend is unavailable.
    fn test_resolve_prompt_model_agent_kind_uses_first_available_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Antigravity, AgentKind::Codex];

        // Act
        let resolved_agent_kind =
            resolve_prompt_model_agent_kind(AgentKind::Claude, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent_kind, Some(AgentKind::Antigravity));
    }
}

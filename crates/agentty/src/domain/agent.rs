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

/// One locally runnable agent CLI and the installed version detected at
/// startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliInfo {
    /// Executable name used to launch the provider CLI.
    pub executable_name: &'static str,
    /// Agent provider family backed by the executable.
    pub kind: AgentKind,
    /// Current version probe state for this executable.
    pub version: AgentCliVersion,
}

/// Version probe state for one locally runnable agent CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCliVersion {
    /// Version detection is still running in the background.
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

    /// Creates one CLI availability row whose version is still loading.
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

    /// Builds loading CLI rows for an existing provider availability list.
    pub fn loading_from_kinds(agent_kinds: &[AgentKind]) -> Vec<Self> {
        agent_kinds.iter().copied().map(Self::loading).collect()
    }
}

/// Supported agent model names across all providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentModel {
    /// Antigravity CLI model backed by `gemini-3.1-pro-preview`.
    AntigravityGemini31ProPreview,
    /// Antigravity CLI model backed by `gemini-3.5-flash`.
    AntigravityGemini35Flash,
    /// Antigravity CLI model backed by `gemini-3.1-flash-lite-preview`.
    AntigravityGemini31FlashLitePreview,
    /// Antigravity CLI model backed by `gemini-3-flash-preview`.
    AntigravityGemini3FlashPreview,
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
    /// Claude Fable model backed by `claude-fable-5`.
    ClaudeFable5,
    /// Claude Sonnet model backed by `claude-sonnet-4-6`.
    ClaudeSonnet46,
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
            Self::AntigravityGemini3FlashPreview | Self::Gemini3FlashPreview => {
                "gemini-3-flash-preview"
            }
            Self::AntigravityGemini35Flash | Self::Gemini35Flash => "gemini-3.5-flash",
            Self::AntigravityGemini31FlashLitePreview | Self::Gemini31FlashLitePreview => {
                "gemini-3.1-flash-lite-preview"
            }
            Self::AntigravityGemini31ProPreview | Self::Gemini31ProPreview => {
                "gemini-3.1-pro-preview"
            }
            Self::Gpt54Mini => "gpt-5.4-mini",
            Self::Gpt53CodexSpark => "gpt-5.3-codex-spark",
            Self::ClaudeOpus48 => "claude-opus-4-8",
            Self::ClaudeFable5 => "claude-fable-5",
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
        }
    }

    /// Returns the model identifier passed to provider transports.
    ///
    /// Antigravity and Gemini use the same raw Gemini CLI model ids, while
    /// their enum variants keep provider ownership distinct in memory.
    pub fn provider_model_str(self) -> &'static str {
        self.as_str()
    }

    /// Parses one persisted model identifier and upgrades retired aliases.
    ///
    /// Stored retired Claude Opus and Codex aliases are migrated forward so
    /// existing projects and sessions continue loading after model removals.
    pub(crate) fn parse_persisted(value: &str) -> Result<Self, String> {
        match value {
            "claude-opus-4-6" | "claude-opus-4-7" => Ok(Self::ClaudeOpus48),
            "gpt-5.4" => Ok(Self::Gpt55),
            _ => value.parse(),
        }
    }

    /// Returns the owning provider family for this model.
    pub fn kind(self) -> AgentKind {
        match self {
            Self::AntigravityGemini31ProPreview
            | Self::AntigravityGemini35Flash
            | Self::AntigravityGemini31FlashLitePreview
            | Self::AntigravityGemini3FlashPreview => AgentKind::Antigravity,
            Self::Gemini3FlashPreview
            | Self::Gemini35Flash
            | Self::Gemini31FlashLitePreview
            | Self::Gemini31ProPreview => AgentKind::Gemini,
            Self::Gpt55 | Self::Gpt54Mini | Self::Gpt53CodexSpark => AgentKind::Codex,
            Self::ClaudeOpus48
            | Self::ClaudeFable5
            | Self::ClaudeSonnet46
            | Self::ClaudeHaiku4520251001 => AgentKind::Claude,
        }
    }

    /// Parses an Antigravity-owned model id.
    ///
    /// Accepts raw Gemini ids shown in the Antigravity `/model` picker.
    fn parse_antigravity_model(value: &str) -> Option<Self> {
        match value {
            "gemini-3.1-pro-preview" => Some(Self::AntigravityGemini31ProPreview),
            "gemini-3.5-flash" => Some(Self::AntigravityGemini35Flash),
            "gemini-3.1-flash-lite-preview" => Some(Self::AntigravityGemini31FlashLitePreview),
            "gemini-3-flash-preview" => Some(Self::AntigravityGemini3FlashPreview),
            _ => None,
        }
    }
}

/// Returns all selectable models owned by the provided agent kinds in stable
/// settings and slash-menu order.
#[must_use]
pub fn selectable_models_for_agent_kinds(agent_kinds: &[AgentKind]) -> Vec<AgentModel> {
    agent_kinds
        .iter()
        .flat_map(|agent_kind| agent_kind.models())
        .copied()
        .collect()
}

/// Resolves one model against the currently available agent kinds.
///
/// When `model` belongs to an unavailable provider, this prefers
/// `fallback_model` when its provider is available and otherwise falls back to
/// the first available provider default in `agent_kinds`. When no providers
/// are available, it returns `fallback_model` unchanged.
#[must_use]
pub fn resolve_model_for_available_agent_kinds(
    model: AgentModel,
    agent_kinds: &[AgentKind],
    fallback_model: AgentModel,
) -> AgentModel {
    if agent_kinds.contains(&model.kind()) {
        return model;
    }

    if agent_kinds.contains(&fallback_model.kind()) {
        return fallback_model;
    }

    agent_kinds
        .first()
        .copied()
        .map_or(fallback_model, AgentKind::default_model)
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
            "claude-fable-5" => Ok(Self::ClaudeFable5),
            "claude-sonnet-4-6" => Ok(Self::ClaudeSonnet46),
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
            Self::AntigravityGemini31ProPreview | Self::Gemini31ProPreview => {
                "Higher-quality Gemini model for deeper reasoning."
            }
            Self::AntigravityGemini35Flash | Self::Gemini35Flash => {
                "Fast Gemini model for current Flash workloads."
            }
            Self::AntigravityGemini31FlashLitePreview | Self::Gemini31FlashLitePreview => {
                "Lightweight Gemini model for fast, cost-conscious iterations."
            }
            Self::AntigravityGemini3FlashPreview | Self::Gemini3FlashPreview => {
                "Fast Gemini model for quick iterations."
            }
            Self::Gpt55 => "Newer Codex model with stronger coding performance when available.",
            Self::Gpt54Mini => "Small, fast Codex model for simpler coding tasks.",
            Self::Gpt53CodexSpark => "Codex spark model for quick coding iterations.",
            Self::ClaudeOpus48 => "Latest Claude Opus model for complex tasks.",
            Self::ClaudeFable5 => "Claude Fable 5 model for complex tasks.",
            Self::ClaudeSonnet46 => "Balanced Claude model for quality and latency.",
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
            Self::Antigravity => AgentModel::AntigravityGemini31ProPreview,
            Self::Gemini => AgentModel::Gemini31ProPreview,
            Self::Claude => AgentModel::ClaudeOpus48,
            Self::Codex => AgentModel::Gpt55,
        }
    }

    /// Returns the model string when it belongs to this agent kind.
    pub fn model_str(self, model: AgentModel) -> Option<&'static str> {
        if model.kind() != self {
            return None;
        }

        Some(model.as_str())
    }

    /// Returns the curated model list for this agent kind.
    pub fn models(self) -> &'static [AgentModel] {
        const GEMINI_MODELS: &[AgentModel] = &[
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini35Flash,
            AgentModel::Gemini31FlashLitePreview,
            AgentModel::Gemini3FlashPreview,
        ];
        const ANTIGRAVITY_MODELS: &[AgentModel] = &[
            AgentModel::AntigravityGemini31ProPreview,
            AgentModel::AntigravityGemini35Flash,
            AgentModel::AntigravityGemini31FlashLitePreview,
            AgentModel::AntigravityGemini3FlashPreview,
        ];
        const CLAUDE_MODELS: &[AgentModel] = &[
            AgentModel::ClaudeOpus48,
            AgentModel::ClaudeFable5,
            AgentModel::ClaudeSonnet46,
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
        if self == Self::Antigravity {
            return AgentModel::parse_antigravity_model(value);
        }

        let model = value.parse::<AgentModel>().ok()?;
        if model.kind() != self {
            return None;
        }

        Some(model)
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
        let gemini_model = AgentModel::Gemini3FlashPreview.as_str();

        // Act
        let parsed_model = claude_kind.parse_model(gemini_model);

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
    /// Ensures Antigravity parses raw Gemini model ids as Antigravity-owned
    /// model selections.
    fn test_parse_model_parses_antigravity() {
        // Arrange
        let antigravity_kind = AgentKind::Antigravity;

        // Act
        let parsed_model = antigravity_kind.parse_model("gemini-3.5-flash");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::AntigravityGemini35Flash));
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
    /// Ensures `gemini-3.1-flash-lite-preview` parses as a Gemini model.
    fn test_parse_model_parses_gemini_31_flash_lite_preview() {
        // Arrange
        let gemini_kind = AgentKind::Gemini;

        // Act
        let parsed_model = gemini_kind.parse_model("gemini-3.1-flash-lite-preview");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gemini31FlashLitePreview));
    }

    #[test]
    /// Ensures `gemini-3.5-flash` parses as a Gemini model.
    fn test_parse_model_parses_gemini_35_flash() {
        // Arrange
        let gemini_kind = AgentKind::Gemini;

        // Act
        let parsed_model = gemini_kind.parse_model("gemini-3.5-flash");

        // Assert
        assert_eq!(parsed_model, Some(AgentModel::Gemini35Flash));
    }

    #[test]
    /// Ensures retired Claude Opus aliases no longer parse as selectable
    /// models.
    fn test_parse_model_rejects_retired_claude_opus_aliases() {
        // Arrange
        let claude_kind = AgentKind::Claude;

        // Act
        let parsed_opus_46 = claude_kind.parse_model("claude-opus-4-6");
        let parsed_opus_47 = claude_kind.parse_model("claude-opus-4-7");

        // Assert
        assert_eq!(parsed_opus_46, None);
        assert_eq!(parsed_opus_47, None);
    }

    #[test]
    /// Ensures `claude-fable-5` parses as a supported Claude model.
    fn test_parse_model_parses_claude_fable_5() {
        // Arrange
        let claude_kind = AgentKind::Claude;

        // Act
        let parsed_fable_5 = claude_kind.parse_model("claude-fable-5");

        // Assert
        assert_eq!(parsed_fable_5, Some(AgentModel::ClaudeFable5));
    }

    #[test]
    /// Ensures persisted model ids parse or migrate to supported
    /// models.
    fn test_parse_persisted_handles_supported_and_retired_models() {
        // Arrange

        // Act
        let parsed_opus_46 = AgentModel::parse_persisted("claude-opus-4-6");
        let parsed_opus_47 = AgentModel::parse_persisted("claude-opus-4-7");
        let parsed_fable_5 = AgentModel::parse_persisted("claude-fable-5");
        let parsed_gpt_54 = AgentModel::parse_persisted("gpt-5.4");

        // Assert
        assert_eq!(parsed_opus_46, Ok(AgentModel::ClaudeOpus48));
        assert_eq!(parsed_opus_47, Ok(AgentModel::ClaudeOpus48));
        assert_eq!(parsed_fable_5, Ok(AgentModel::ClaudeFable5));
        assert_eq!(parsed_gpt_54, Ok(AgentModel::Gpt55));
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
    /// Ensures Codex models still resolve their owning provider correctly.
    fn test_codex_model_kind_is_codex() {
        // Arrange
        let models = [
            AgentModel::Gpt55,
            AgentModel::Gpt54Mini,
            AgentModel::Gpt53CodexSpark,
        ];

        // Act
        let kinds = models.map(AgentModel::kind);

        // Assert
        assert_eq!(kinds, [AgentKind::Codex; 3]);
    }

    #[test]
    /// Ensures Claude models still resolve their owning provider correctly.
    fn test_claude_model_kind_is_claude() {
        // Arrange
        let model = AgentModel::ClaudeSonnet46;

        // Act
        let kind = model.kind();

        // Assert
        assert_eq!(kind, AgentKind::Claude);
    }

    #[test]
    /// Ensures Gemini models still resolve their owning provider correctly.
    fn test_gemini_model_kind_is_gemini() {
        // Arrange
        let models = [
            AgentModel::Gemini31ProPreview,
            AgentModel::Gemini35Flash,
            AgentModel::Gemini31FlashLitePreview,
            AgentModel::Gemini3FlashPreview,
        ];

        // Act
        let kinds = models.map(AgentModel::kind);

        // Assert
        assert_eq!(kinds, [AgentKind::Gemini; 4]);
    }

    #[test]
    /// Ensures Antigravity models resolve to the Antigravity provider.
    fn test_antigravity_model_kind_is_antigravity() {
        // Arrange
        let models = [
            AgentModel::AntigravityGemini31ProPreview,
            AgentModel::AntigravityGemini35Flash,
            AgentModel::AntigravityGemini31FlashLitePreview,
            AgentModel::AntigravityGemini3FlashPreview,
        ];

        // Act
        let kinds = models.map(AgentModel::kind);

        // Assert
        assert_eq!(kinds, [AgentKind::Antigravity; 4]);
    }

    #[test]
    /// Ensures Antigravity model selections pass raw Gemini ids to provider
    /// transports.
    fn test_antigravity_provider_model_str_returns_raw_gemini_model() {
        // Arrange
        let model = AgentModel::AntigravityGemini31FlashLitePreview;

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
        let agent_kinds = [AgentKind::Codex, AgentKind::Gemini];

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
    /// Ensures Antigravity exposes the same selectable Gemini model set in
    /// provider-owned form.
    fn test_antigravity_models_match_gemini_model_set() {
        // Arrange

        // Act
        let antigravity_provider_models = AgentKind::Antigravity
            .models()
            .iter()
            .map(|model| model.provider_model_str())
            .collect::<Vec<_>>();
        let gemini_provider_models = AgentKind::Gemini
            .models()
            .iter()
            .map(|model| model.provider_model_str())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(antigravity_provider_models, gemini_provider_models);
    }

    #[test]
    /// Ensures unavailable models fall back to an available preferred model
    /// when possible.
    fn test_resolve_model_for_available_agent_kinds_prefers_available_fallback() {
        // Arrange
        let unavailable_model = AgentModel::ClaudeOpus48;
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Gemini];
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
        let available_agent_kinds = [AgentKind::Codex, AgentKind::Gemini];
        let unavailable_fallback_model = AgentModel::ClaudeSonnet46;

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
    /// Ensures prompt model selection keeps the current backend when it is
    /// still available locally.
    fn test_resolve_prompt_model_agent_kind_prefers_current_agent() {
        // Arrange
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

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
        let available_agent_kinds = [AgentKind::Gemini, AgentKind::Codex];

        // Act
        let resolved_agent_kind =
            resolve_prompt_model_agent_kind(AgentKind::Claude, &available_agent_kinds);

        // Assert
        assert_eq!(resolved_agent_kind, Some(AgentKind::Gemini));
    }
}

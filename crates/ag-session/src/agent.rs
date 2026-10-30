use std::fmt;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock, PoisonError};

use ag_contracts::{ReasoningLevel, SpeedMode};
use rustc_hash::FxHashMap;

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
    /// Native in-process `ag-harness` backend, which has no CLI executable.
    Harness,
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
        },
    )* })*) => {
        /// Supported agent model names across all providers.
        ///
        /// Gemini ids are shared by Gemini and Antigravity; provider ownership
        /// lives on [`AgentSelection`]. Removing a variant is a Rust source API
        /// change even when [`AgentModel::retired_replacement`] preserves saved ids.
        #[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
        #[repr(u8)]
        pub enum AgentModel {
            $($( $(#[$attribute])* $model = $discriminant, )*)*
            /// Model a local or self-hosted OpenAI-compatible server, such as
            /// Ollama, serves to the native harness. Its id is
            /// `openai-compatible/<served model>`, such as
            /// `openai-compatible/qwen3:latest`.
            OpenAiCompatible(ServedModel) = 18,
        }

        impl AgentModel {
            /// All fixed catalog models, grouped in catalog provider order.
            /// Served [`AgentModel::OpenAiCompatible`] models are discovered at
            /// runtime and never listed here.
            pub const ALL: &[Self] = &[$($(Self::$model,)*)*];

            const fn descriptor(self) -> ModelDescriptor {
                match self {
                    $($(Self::$model => ModelDescriptor {
                        description: $description,
                        id: $id,
                        supports_fast_mode: $fast,
                    },)*)*
                    Self::OpenAiCompatible(served) => ModelDescriptor {
                        description: "Model served by a local or self-hosted \
                                      OpenAI-compatible server, such as Ollama.",
                        id: served.id(),
                        supports_fast_mode: false,
                    },
                }
            }
        }

        impl AgentKind {
            /// Returns the models declared under this provider's catalog
            /// block, without the models it shares with another provider.
            const fn declared_models(self) -> &'static [AgentModel] {
                match self {
                    Self::Antigravity => &[],
                    $(Self::$provider => &[$(AgentModel::$model,)*],)*
                }
            }
        }
    };
}

define_model_catalog! {
    Gemini {
        /// Gemini Argon model backed by `gemini-4-argon`.
        Gemini4Argon = 17 => {
            id: "gemini-4-argon",
            description: "Gemini 4 Argon model.",
            fast: false,
        },
        /// Higher-quality Gemini preview model backed by `gemini-3.1-pro-preview`.
        Gemini31Pro = 6 => {
            id: "gemini-3.1-pro-preview",
            description: "Higher-quality Gemini model for deeper reasoning.",
            fast: false,
        },
        /// Fast Gemini model backed by `gemini-3.8-flash`.
        Gemini38Flash = 4 => {
            id: "gemini-3.8-flash",
            description: "Fast Gemini model for agentic and multimodal tasks.",
            fast: false,
        },
        /// Lightweight Gemini model backed by `gemini-3.5-flash-lite`.
        Gemini35FlashLite = 5 => {
            id: "gemini-3.5-flash-lite",
            description: "Lightweight Gemini model for fast, cost-conscious workloads.",
            fast: false,
        },
    }
    Claude {
        /// Claude Fable model backed by `claude-fable-5-1`.
        ClaudeFable51 = 10 => {
            id: "claude-fable-5-1",
            description: "Claude Fable model for creative, narrative-heavy tasks.",
            fast: false,
        },
        /// Claude Opus model backed by `claude-opus-5-5`.
        ClaudeOpus55 = 8 => {
            id: "claude-opus-5-5",
            description: "Latest Claude Opus model for complex agentic tasks.",
            fast: true,
        },
        /// Claude Sonnet model backed by `claude-sonnet-5-5`.
        ClaudeSonnet55 = 9 => {
            id: "claude-sonnet-5-5",
            description: "Balanced Claude model for quality and latency.",
            fast: false,
        },
        /// Claude Haiku model backed by `claude-haiku-4-5-20251001`.
        ClaudeHaiku4520251001 = 11 => {
            id: "claude-haiku-4-5-20251001",
            description: "Fast Claude model for lighter tasks.",
            fast: false,
        },
    }
    Codex {
        /// Codex Astra model backed by `gpt-6-astra`.
        Gpt6Astra = 0 => {
            id: "gpt-6-astra",
            description: "Most capable Codex model for the hardest end-to-end work.",
            fast: true,
        },
        /// Codex Sol model backed by `gpt-6.1-sol`.
        Gpt61Sol = 1 => {
            id: "gpt-6.1-sol",
            description: "Near-Astra Codex model for complex work at a lower cost.",
            fast: true,
        },
        /// Codex Luna model backed by `gpt-6-luna`.
        Gpt6Luna = 2 => {
            id: "gpt-6-luna",
            description: "Efficient Codex model for focused, high-volume tasks.",
            fast: true,
        },
        /// Codex Terra model backed by `gpt-5.6-terra`.
        Gpt56Terra = 3 => {
            id: "gpt-5.6-terra",
            description: "Current Codex model for balanced coding performance.",
            fast: true,
        },
        /// Codex spark model backed by `gpt-5.3-codex-spark`.
        Gpt53CodexSpark = 7 => {
            id: "gpt-5.3-codex-spark",
            description: "Codex spark model for quick coding iterations.",
            fast: false,
        },
    }
    Harness {
        /// Muse model backed by `muse-spark-1.3`.
        MuseSpark13 = 12 => {
            id: "muse-spark-1.3",
            description: "Muse Spark model served through the native harness.",
            fast: false,
        },
        /// Discounted Muse model backed by `muse-spark-1.3-contributor`,
        /// whose prompts and completions Meta may use for training.
        MuseSpark13Contributor = 19 => {
            id: "muse-spark-1.3-contributor",
            description: "Cheap Muse Spark tier; Meta may train on its prompts and completions.",
            fast: false,
        },
        /// Kimi model backed by `kimi-k3`.
        KimiK3 = 13 => {
            id: "kimi-k3",
            description: "Latest Kimi model served through the native harness.",
            fast: false,
        },
        /// Kimi model backed by `kimi-k2.6`.
        KimiK26 = 14 => {
            id: "kimi-k2.6",
            description: "Previous Kimi model served through the native harness.",
            fast: false,
        },
        /// Qwen model backed by `qwen3.8-max`.
        Qwen38Max = 15 => {
            id: "qwen3.8-max",
            description: "Most capable Qwen model served through the native harness.",
            fast: false,
        },
        /// Qwen model backed by `qwen-plus`.
        QwenPlus = 16 => {
            id: "qwen-plus",
            description: "Balanced Qwen model served through the native harness.",
            fast: false,
        },
        /// Cheap Qwen model backed by `qwen3.8-flash`.
        Qwen38Flash = 20 => {
            id: "qwen3.8-flash",
            description: "Cheap Qwen model for simple tasks such as titles and commit messages.",
            fast: false,
        },
        /// Cheapest Qwen model backed by `qwen3.7-flash`.
        Qwen37Flash = 21 => {
            id: "qwen3.7-flash",
            description: "Cheapest Qwen model for short, simple tasks.",
            fast: false,
        },
    }
}

/// Hosted models the native harness serves, in display order: its own
/// catalog models, then the Codex models `OpenAI`'s Responses API also
/// serves, which the harness shares with [`AgentKind::Codex`] like
/// Antigravity shares the Gemini models. Served
/// [`AgentModel::OpenAiCompatible`] models are discovered at runtime and
/// listed before these.
///
/// The `OpenAI` models come last because `OPENAI_API_KEY` is often set for
/// other tools, so its mere presence must not move sessions from a
/// deliberately configured harness provider to paid `OpenAI` usage. The
/// harness library also catalogs the deprecated `gpt-5.3-codex`, which
/// Agentty retires to [`AgentModel::Gpt61Sol`] instead of offering it. Cheap
/// tiers follow their provider's other models, so adding one never changes
/// which configured model a new session starts on.
const HARNESS_MODELS: &[AgentModel] = &[
    AgentModel::MuseSpark13,
    AgentModel::MuseSpark13Contributor,
    AgentModel::KimiK3,
    AgentModel::KimiK26,
    AgentModel::Qwen38Max,
    AgentModel::QwenPlus,
    AgentModel::Qwen38Flash,
    AgentModel::Qwen37Flash,
    AgentModel::Gpt61Sol,
    AgentModel::Gpt6Astra,
    AgentModel::Gpt6Luna,
    AgentModel::Gpt56Terra,
];

/// Id prefix of every [`AgentModel::OpenAiCompatible`] model.
const OPENAI_COMPATIBLE_ID: &str = "openai-compatible";

struct ModelDescriptor {
    description: &'static str,
    id: &'static str,
    supports_fast_mode: bool,
}

/// Model an OpenAI-compatible server serves, identified as
/// `openai-compatible/<served model>`.
///
/// Servers name their models at runtime, so each distinct id is interned once
/// and kept for the process lifetime, which keeps [`AgentModel`] `Copy` and
/// one pointer wide. A process interns only the names its servers report and
/// its sessions store.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ServedModel(&'static &'static str);

impl ServedModel {
    /// Model named by the host's `OPENAI_COMPATIBLE_MODEL`, stored under the
    /// bare `openai-compatible` id.
    pub const CONFIGURED: Self = Self(&OPENAI_COMPATIBLE_ID);

    /// Returns the served model `name`, such as `qwen3:latest`, or `None`
    /// for a blank name.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }

        Some(Self(intern(format!("{OPENAI_COMPATIBLE_ID}/{name}"))))
    }

    /// Returns the stable model id, such as `openai-compatible/qwen3:latest`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.0
    }

    /// Returns the name the server serves, or `None` for
    /// [`Self::CONFIGURED`], whose name the host environment supplies.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        self.id()
            .strip_prefix(OPENAI_COMPATIBLE_ID)?
            .strip_prefix('/')
    }

    /// Parses an `openai-compatible` or `openai-compatible/<name>` id.
    fn parse(id: &str) -> Option<Self> {
        if id == OPENAI_COMPATIBLE_ID {
            return Some(Self::CONFIGURED);
        }

        Self::new(id.strip_prefix(OPENAI_COMPATIBLE_ID)?.strip_prefix('/')?)
    }
}

/// Returns the process-lifetime copy of `id`, leaking it on first use.
fn intern(id: String) -> &'static &'static str {
    static INTERNED: OnceLock<Mutex<FxHashMap<&'static str, &'static &'static str>>> =
        OnceLock::new();
    let mut interned = INTERNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = interned.get(id.as_str()) {
        return existing;
    }
    let leaked: &'static &'static str = Box::leak(Box::new(&*Box::leak(id.into_boxed_str())));
    interned.insert(leaked, leaked);

    leaked
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

    /// Returns whether this exact provider/model pair supports Fast mode,
    /// which needs a provider with a speed control; the harness serves Codex
    /// models without one.
    #[must_use]
    pub fn supports_fast_mode(self) -> bool {
        self.kind.supports_speed_mode() && self.model.descriptor().supports_fast_mode
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
            AgentKind::Antigravity | AgentKind::Gemini | AgentKind::Harness => self,
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
    /// ids never appear in selectable model lists. Persisted selections resolve
    /// to the replacement, and sessions that are still active are switched to
    /// it automatically. Database migrations may also normalize saved ids.
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
            ("claude-sonnet-4-6", AgentModel::ClaudeSonnet55),
            ("claude-sonnet-5", AgentModel::ClaudeSonnet55),
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
        .chain([AgentKind::Harness])
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
            .or_else(|| ServedModel::parse(value).map(Self::OpenAiCompatible))
            .ok_or_else(|| format!("unknown model: {value}"))
    }
}

impl AgentSelectionMetadata for AgentModel {
    /// Returns the model id, or the bare served name of an
    /// [`AgentModel::OpenAiCompatible`] model, such as `qwen3:latest`.
    fn name(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible(served) => served.name().unwrap_or(served.id()),
            _ => (*self).provider_model_str(),
        }
    }

    fn description(&self) -> &'static str {
        self.descriptor().description
    }
}

impl AgentKind {
    /// All CLI-backed agent kinds, in display order.
    ///
    /// [`AgentKind::Harness`] runs in process, so it is never discovered from
    /// `PATH` and stays out of this list.
    pub const ALL: &[AgentKind] = &[
        AgentKind::Gemini,
        AgentKind::Antigravity,
        AgentKind::Claude,
        AgentKind::Codex,
    ];

    /// Returns the provider CLI executable name, or `None` for the in-process
    /// harness.
    pub fn executable_name(self) -> Option<&'static str> {
        match self {
            Self::Antigravity => Some("agy"),
            Self::Gemini => Some("gemini"),
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Harness => None,
        }
    }

    /// Returns this provider's models in their catalog display order.
    ///
    /// Antigravity serves the Gemini models, and the harness serves the
    /// Codex models available through `OpenAI`'s Responses API beside its
    /// own.
    pub fn models(self) -> &'static [AgentModel] {
        match self {
            Self::Antigravity => Self::Gemini.declared_models(),
            Self::Harness => HARNESS_MODELS,
            Self::Claude | Self::Codex | Self::Gemini => self.declared_models(),
        }
    }

    /// Returns the default model for this agent kind.
    pub fn default_model(self) -> AgentModel {
        match self {
            Self::Antigravity | Self::Gemini => AgentModel::Gemini31Pro,
            Self::Claude => AgentModel::ClaudeFable51,
            Self::Codex => AgentModel::Gpt61Sol,
            Self::Harness => AgentModel::MuseSpark13,
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

    /// Returns whether this provider can run the given model. Only
    /// [`AgentKind::Harness`] runs served [`AgentModel::OpenAiCompatible`]
    /// models.
    pub fn supports_model(self, model: AgentModel) -> bool {
        match model {
            AgentModel::OpenAiCompatible(_) => self == Self::Harness,
            _ => self.models().contains(&model),
        }
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
            Self::Harness => "harness",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Antigravity => "Google Antigravity CLI agent.",
            Self::Gemini => "Google Gemini CLI agent.",
            Self::Claude => "Anthropic Claude Code agent.",
            Self::Codex => "OpenAI Codex CLI agent.",
            Self::Harness => "Native Agentty harness agent.",
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
            "harness" => Ok(Self::Harness),
            other => Err(format!("unknown agent kind: {other}")),
        }
    }
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;

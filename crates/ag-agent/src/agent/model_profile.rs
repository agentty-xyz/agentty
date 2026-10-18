//! Declared context profiles for the catalog models whose context Agentty
//! manages: Codex app-server compaction and native harness registrations.

use ag_harness::model::ModelProfile;
use ag_session::AgentModel;

/// Codex Astra, Sol, Luna, and Terra: 1,050,000-token window with 128,000
/// tokens held back for output and provider overhead before compaction. The
/// reservation is an input-budget policy, not a maximum output limit.
const CODEX_LARGE: ModelProfile = ModelProfile::new(1_050_000).with_reserved_tokens(128_000);
/// Codex Spark: 128,000-token window with 8,000 tokens held back for output
/// and provider overhead before compaction.
const CODEX_SPARK: ModelProfile = ModelProfile::new(128_000).with_reserved_tokens(8_000);
/// Kimi K2.6: documented and enforced 262,144-token window with a
/// 32,768-token default `max_tokens`.
const KIMI_K2_6: ModelProfile = ModelProfile::new(262_144).with_estimate_margin(32_768);
/// Kimi K3: documented 1M (1,048,576-token) window with a 131,072-token
/// default `max_completion_tokens`.
const KIMI_K3: ModelProfile = ModelProfile::new(1_048_576).with_estimate_margin(131_072);
/// Muse Spark 1.3: documented 1,048,576-token window and an undocumented
/// output limit, reserved at the 131,072 tokens the API accepts.
const MUSE_SPARK_1_3: ModelProfile = ModelProfile::new(1_048_576).with_estimate_margin(131_072);
/// Qwen3.8-Max: documented 1,000,000-token window, 983,616-token thinking
/// input limit, and 131,072-token output limit.
const QWEN3_8_MAX: ModelProfile = ModelProfile::new(1_000_000).with_estimate_margin(131_072);
/// Qwen-Plus: documented 1,000,000-token window, 995,904-token thinking
/// input limit, and 32,768-token output limit.
const QWEN_PLUS: ModelProfile = ModelProfile::new(1_000_000).with_estimate_margin(32_768);

/// Returns the declared context profile of `model`, or `None` when its
/// provider CLI manages context itself.
pub(crate) const fn declared(model: AgentModel) -> Option<ModelProfile> {
    match model {
        AgentModel::Gpt6Astra
        | AgentModel::Gpt61Sol
        | AgentModel::Gpt6Luna
        | AgentModel::Gpt56Terra => Some(CODEX_LARGE),
        AgentModel::Gpt53CodexSpark => Some(CODEX_SPARK),
        AgentModel::MuseSpark13 => Some(MUSE_SPARK_1_3),
        AgentModel::KimiK3 => Some(KIMI_K3),
        AgentModel::KimiK26 => Some(KIMI_K2_6),
        AgentModel::Qwen38Max => Some(QWEN3_8_MAX),
        AgentModel::QwenPlus => Some(QWEN_PLUS),
        AgentModel::Gemini4Argon
        | AgentModel::Gemini31Pro
        | AgentModel::Gemini38Flash
        | AgentModel::Gemini35FlashLite
        | AgentModel::ClaudeFable51
        | AgentModel::ClaudeOpus55
        | AgentModel::ClaudeSonnet55
        | AgentModel::ClaudeHaiku4520251001 => None,
    }
}

#[cfg(test)]
#[path = "model_profile_test.rs"]
mod tests;

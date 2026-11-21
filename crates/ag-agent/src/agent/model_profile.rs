//! Declared context profiles for the catalog models whose context Agentty
//! manages: Codex app-server compaction and native harness registrations.
//!
//! The `ag-harness` provider catalog declares the Kimi, Muse, and Claude
//! profiles, the first two fitted as below; Agentty declares the Qwen3.8-Max
//! and Qwen-Plus ones.
//!
//! Harness models declare estimators fitted on 2026-10-18 to live
//! provider-reported input tokens: the `ag-harness` benchmark `estimate`
//! suite's synthetic probes plus recorded Agentty harness requests, each
//! with the 7.4 KB protocol schema and the `read`, `write`, and `bash`
//! definitions
//! (`crates/ag-harness/tests/benchmark/results/2026-10-18-estimate-probes.
//! jsonl`). Errors below are provider tokens over estimated tokens, minus one,
//! on those recorded requests, replayed without tool-call reasoning to the
//! models that never receive it. Content denser than repository text, such
//! as digit-heavy generated code, still runs 30–46% over every fitted
//! estimate, which runtime calibration absorbs.
//!
//! Each harness profile's estimate margin tolerates the largest under-count
//! its estimator showed, rounded up, plus five points: on those recorded
//! requests, or on one live request fitted near the input budget from this
//! repository's Rust sources read whole (`ag-harness` live test
//! `test_estimate_margin_near_window`), which ran +12.6% over the estimate on
//! Muse Spark 1.3, +11.9% on Kimi K3, +9.0% on Qwen-Plus, +7.7% on
//! Qwen3.8-Max, and +7.6% on Kimi K2.6. Kimi K2.6 shares Kimi K3's tokenizer
//! and margin. Calibration learns denser content only from the next usage
//! report, and a full budget leaves 113,000–140,000 tokens beside its
//! prediction on the 1M windows and 33,000 on Kimi K2.6: one maximal 50 KB
//! `read` of digits, which Qwen tokenizes one per digit, misses about 39,000
//! Qwen tokens and 12,000 Kimi tokens. Content denser still overflows the
//! window, and the harness re-sends the request within 75% of the rejected
//! weight, which fits content up to 53–57% over the estimate.

use ag_harness::model::{HeuristicContextEstimator, ModelProfile};
use ag_session::AgentModel;

/// Codex Astra, Sol, Luna, and Terra: 1,050,000-token window with 128,000
/// tokens held back for output and provider overhead before compaction. The
/// reservation is an input-budget policy, not a maximum output limit.
const CODEX_LARGE: ModelProfile = ModelProfile::new(1_050_000).with_reserved_tokens(128_000);
/// Codex Spark: 128,000-token window with 8,000 tokens held back for output
/// and provider overhead before compaction.
const CODEX_SPARK: ModelProfile = ModelProfile::new(128_000).with_reserved_tokens(8_000);
/// Qwen3.8-Max: documented 1,000,000-token window, 983,616-token thinking
/// input limit, and 131,072-token output limit.
const QWEN3_8_MAX: ModelProfile = ModelProfile::new(1_000_000)
    .with_estimate_margin(131_072, 15)
    .with_estimator(QWEN3_8_MAX_ESTIMATOR);
/// Qwen-Plus: documented 1,000,000-token window, 995,904-token thinking
/// input limit, and 32,768-token output limit.
const QWEN_PLUS: ModelProfile = ModelProfile::new(1_000_000)
    .with_estimate_margin(32_768, 16)
    .with_estimator(QWEN_PLUS_ESTIMATOR);

/// Qwen3.8-Max (JSON Object mode): 24 synthetic probes and 39 recorded
/// requests; content at 4.2 bytes per token, tool definitions at 2.7, and a
/// 250-token tool-calling preamble. Recorded requests: −8.3% to +9.6%
/// (default estimate: +22% to +69%).
const QWEN3_8_MAX_ESTIMATOR: HeuristicContextEstimator = HeuristicContextEstimator::DEFAULT
    .with_messages(239, 33)
    .with_tool_definitions(373, 0)
    .with_request_tokens(250)
    .with_output_schema_in_prompt();
/// Qwen-Plus (JSON Object mode): 25 synthetic probes and 39 recorded
/// requests; content at 4.2 bytes per token and tool definitions at 2.7.
/// Recorded requests: −7.9% to +10.9% (default estimate: +19% to +63%).
const QWEN_PLUS_ESTIMATOR: HeuristicContextEstimator = HeuristicContextEstimator::DEFAULT
    .with_messages(236, 24)
    .with_tool_definitions(372, 0)
    .with_request_tokens(70)
    .with_output_schema_in_prompt();

/// Returns the declared context profile of `model`, or `None` when its
/// provider CLI manages context itself, the `ag-harness` provider catalog
/// declares it, as for the Kimi, Muse, Qwen flash, ChatGPT-plan, and Claude
/// models,
/// or, for the OpenAI-compatible model, the environment declares it.
pub(crate) const fn declared(model: AgentModel) -> Option<ModelProfile> {
    match model {
        AgentModel::Gpt6Astra
        | AgentModel::Gpt61Sol
        | AgentModel::Gpt6Luna
        | AgentModel::Gpt56Terra => Some(CODEX_LARGE),
        AgentModel::Gpt53CodexSpark => Some(CODEX_SPARK),
        AgentModel::Qwen38Max => Some(QWEN3_8_MAX),
        AgentModel::QwenPlus => Some(QWEN_PLUS),
        AgentModel::MuseSpark13
        | AgentModel::MuseSpark13Contributor
        | AgentModel::KimiK3
        | AgentModel::KimiK26
        | AgentModel::OpenAiCompatible(_)
        | AgentModel::Qwen38Flash
        | AgentModel::Qwen37Flash
        | AgentModel::ChatGptGpt61Sol
        | AgentModel::ChatGptGpt61Terra
        | AgentModel::ChatGptGpt6Astra
        | AgentModel::ChatGptGpt6Sol
        | AgentModel::ChatGptGpt6Luna
        | AgentModel::ChatGptGpt56Sol
        | AgentModel::ChatGptGpt56Terra
        | AgentModel::ChatGptGpt56Luna
        | AgentModel::Gemini4Argon
        | AgentModel::Gemini4Hydrogen
        | AgentModel::GeminiProOrchestrator
        | AgentModel::Gemini38Flash
        | AgentModel::GptOss120b
        | AgentModel::ClaudeFable52
        | AgentModel::ClaudeOpus55
        | AgentModel::ClaudeSonnet55
        | AgentModel::ClaudeHaiku55 => None,
    }
}

#[cfg(test)]
#[path = "model_profile_test.rs"]
mod tests;

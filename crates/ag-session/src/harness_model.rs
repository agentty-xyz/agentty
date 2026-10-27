//! Harness model choices offered to users and the default for new sessions.

use crate::AgentModel;

/// Smallest context window, in tokens, a local server may run a model with
/// for harness turns. The system prompt, tool definitions, output schema,
/// and response reservation leave too little room for a conversation below
/// it, and Ollama fills a smaller window by silently dropping the start of
/// the conversation.
pub const MINIMUM_RUNNING_CONTEXT_WINDOW: u64 = 16_384;

/// Who pays for a harness model's tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelBilling {
    /// A local or self-hosted server runs the model at no per-token cost.
    Local,
    /// A hosted provider bills every request to the configured API key.
    Paid,
}

/// Whether a harness model can run a turn now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessModelStatus {
    /// The provider is configured and, for a served model, offers tool calls.
    Ready,
    /// The named environment variable must be set before the provider can
    /// serve requests.
    MissingConfiguration(&'static str),
    /// The server reports that the model cannot call tools, which every
    /// harness turn needs.
    NoToolSupport,
}

/// One harness model a user can choose, with its cost, readiness, and
/// context window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessModelOption {
    /// Who pays for the model's tokens.
    pub billing: ModelBilling,
    /// Context window in tokens the model supports, when the server reports
    /// it.
    pub context_window: Option<u64>,
    /// Context window in tokens the server currently runs the model with,
    /// when it reports one. It can be smaller than
    /// [`Self::context_window`], such as Ollama's 4,096-token default.
    pub loaded_context_window: Option<u64>,
    /// Model sessions run.
    pub model: AgentModel,
    /// Provider or server display name, such as `Ollama` or `Kimi`.
    pub provider: String,
    /// Whether the model can run a turn now.
    pub status: HarnessModelStatus,
}

impl HarnessModelOption {
    /// Returns whether the model can run a turn now.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == HarnessModelStatus::Ready
    }
}

/// Returns the model new harness sessions start on.
///
/// The user's last explicit choice, `remembered`, wins while it can run, and
/// a remembered local model wins even while its server is unreachable, so a
/// stopped server never moves new sessions onto a paid provider. Without one,
/// the first ready local model is preferred over the first ready paid model.
#[must_use]
pub fn default_harness_model(
    options: &[HarnessModelOption],
    remembered: Option<AgentModel>,
) -> Option<AgentModel> {
    let remembered_option =
        remembered.and_then(|model| options.iter().find(|option| option.model == model));
    match (remembered, remembered_option) {
        (Some(model), Some(option)) if option.is_ready() => return Some(model),
        (Some(model @ AgentModel::OpenAiCompatible(_)), None) => return Some(model),
        _ => {}
    }

    [ModelBilling::Local, ModelBilling::Paid]
        .into_iter()
        .find_map(|billing| {
            options
                .iter()
                .find(|option| option.billing == billing && option.is_ready())
        })
        .map(|option| option.model)
}

#[cfg(test)]
#[path = "harness_model_test.rs"]
mod tests;

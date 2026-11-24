//! Per-model usage accumulation for harness runs.

use std::collections::BTreeMap;
use std::future::poll_fn;
use std::pin::Pin;

use ag_contracts::{ContextFill, ModelUsage, PlanLogin, TurnUsage};
use ag_harness::catalog::{ModelId, ModelSpec};
use ag_harness::{Cost, EventStream, RunEventKind, Usage};
use futures_core::Stream as _;

use super::config::ModelSelection;
use crate::agent::usage::{cost_estimate, price};

/// Folds each completed model request of one run into per-model usage.
///
/// The harness prices requests only for models registered with prices, and
/// Agentty registers its models without them, so an unpriced request is
/// priced at the harness provider catalog's list price of the model that
/// answered it. A `ChatGPT` plan model has no price, as its plan bills a flat
/// fee, so its requests carry the API equivalent: the list price of the
/// same-named `OpenAI` API model, or no cost without one. A request's context
/// fill is its reported input against that model's window. The requests' usage
/// sums to the run outcome's usage.
pub(crate) struct UsageMeter {
    /// Registration key of the run's model, which answers requests whose
    /// event names no catalog model.
    run_key: String,
    /// Context window the run's model is fitted to.
    run_window: Option<u64>,
    /// Usage of the requests folded so far.
    usage: TurnUsage,
}

impl UsageMeter {
    /// Creates a meter for a run on `selection`, whose requests are fitted
    /// to the window its profile resolves in `environment`, or to
    /// `running_context_window` when a local server reports one.
    pub(crate) fn new(
        selection: &ModelSelection,
        environment: &BTreeMap<String, String>,
        running_context_window: Option<u64>,
    ) -> Self {
        Self {
            run_key: selection.key(),
            run_window: selection.context_window(environment, running_context_window),
            usage: TurnUsage::default(),
        }
    }

    /// Folds `event` when it completes a model request and returns the
    /// run's cumulative usage after it.
    pub(crate) fn observe(&mut self, event: &RunEventKind) -> Option<&TurnUsage> {
        let RunEventKind::ModelRequestCompleted {
            cost, model, usage, ..
        } = event
        else {
            return None;
        };
        let request = self.request_usage(*cost, model.as_ref(), usage);
        self.usage.record(&request);

        Some(&self.usage)
    }

    /// Folds every model request of `stream` until the run ends and returns
    /// the run's usage.
    pub(crate) async fn drain(mut self, mut stream: EventStream) -> TurnUsage {
        while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
            self.observe(&event.kind);
        }

        self.finish()
    }

    /// Returns the usage of every request folded.
    pub(crate) fn finish(self) -> TurnUsage {
        self.usage
    }

    /// Converts one completed request answered by `model`.
    fn request_usage(
        &self,
        cost: Option<Cost>,
        model: Option<&ModelId>,
        usage: &Usage,
    ) -> ModelUsage {
        let key = model.map_or(self.run_key.as_str(), ModelId::as_str);
        let selection = ModelSelection::from_key(key);
        let spec = selection.as_ref().and_then(ModelSelection::catalog_spec);
        let api_twin = selection.as_ref().and_then(ModelSelection::api_twin_spec);
        let window = if key == self.run_key {
            self.run_window
        } else {
            spec.as_ref().and_then(ModelSpec::context_window)
        };
        let model = ModelSelection::model_id_of(key);
        // A request whose provider omitted its counts leaves every detail
        // and its charge unknown; one that omitted only its output still
        // reports what the model saw.
        if usage.unreported_requests > 0 {
            return ModelUsage {
                context: (usage.input > 0).then(|| ContextFill::of_request(usage.input, window)),
                input_tokens: usage.input,
                output_tokens: usage.output,
                ..ModelUsage::unreported(model, 1)
            };
        }
        let mut request = ModelUsage {
            cache_read_tokens: Some(usage.cache_read),
            cache_write_tokens: usage.cache_write,
            context: Some(ContextFill::of_request(usage.input, window)),
            input_tokens: usage.input,
            output_tokens: usage.output,
            reasoning_tokens: Some(usage.reasoning),
            requests: 1,
            ..ModelUsage::new(model)
        };
        let pricing = spec.and_then(|spec| spec.pricing());
        request.cost = cost
            .map(|cost| cost_estimate(cost, &request, pricing))
            .or_else(|| pricing.map(|pricing| price(&request, pricing)))
            .or_else(|| {
                api_twin
                    .and_then(|twin| twin.pricing())
                    .map(|pricing| price(&request, pricing).billed_to(PlanLogin::ChatGpt))
            });

        request
    }
}

#[cfg(test)]
#[path = "usage_test.rs"]
mod tests;

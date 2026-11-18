//! Model that writes compaction checkpoints and in-turn round summaries in
//! place of the session's model.

use std::cmp;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::calibration::ContextCalibration;
use crate::catalog::{ModelHandle, ModelId, Pricing};
use crate::context::{ContextBudget, ContextEstimator};
use crate::model::{Model, ModelRequest, ReasoningEffort};

/// Context budget and estimator the host set explicitly, which size every
/// provider request, including a summary model's, in place of a declared
/// profile.
#[derive(Clone, Default)]
pub(crate) struct SizingOverrides {
    pub(crate) context_budget: Option<ContextBudget>,
    pub(crate) context_estimator: Option<Arc<dyn ContextEstimator>>,
}

/// A cataloged model that summarizes, with its request budget, estimator,
/// reasoning depth, prices, maximum output, and concurrency cap, and a
/// calibration of its own, so its usage never calibrates the session
/// model's estimates. Clones share the calibration.
#[derive(Clone)]
pub(crate) struct SummaryModel {
    /// Permits of the model's own concurrency cap, shared by every harness
    /// built from its catalog; `None` without a cap.
    pub(crate) concurrency: Option<Arc<Semaphore>>,
    pub(crate) context_budget: ContextBudget,
    pub(crate) context_calibration: ContextCalibration,
    pub(crate) context_estimator: Arc<dyn ContextEstimator>,
    pub(crate) id: ModelId,
    /// Cataloged maximum output, which bounds a request's budget estimate.
    pub(crate) max_output: Option<u64>,
    pub(crate) model: Arc<dyn Model>,
    /// Prices of the model, which give each of its requests its cost.
    pub(crate) pricing: Option<Pricing>,
    /// Reasoning depth of every summary request: low when the model's
    /// specification declares reasoning, and otherwise none.
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

impl SummaryModel {
    /// Returns the summary model `handle` resolved, sized by the host's
    /// explicit `overrides`, else by its declared profile, else by the
    /// session's `context_budget` and `context_estimator`; an explicit
    /// budget larger than the declared one gives way to it.
    pub(crate) fn new(
        handle: &ModelHandle,
        overrides: &SizingOverrides,
        context_budget: ContextBudget,
        context_estimator: &Arc<dyn ContextEstimator>,
    ) -> Self {
        let profile_estimator = handle
            .profile()
            .map(|profile| Arc::new(profile.estimator()) as Arc<dyn ContextEstimator>);
        let context_estimator = overrides
            .context_estimator
            .clone()
            .or(profile_estimator)
            .unwrap_or_else(|| Arc::clone(context_estimator));
        // An explicit budget may lower the model's declared window but
        // never raise it past what the model accepts.
        let context_budget = match (overrides.context_budget, handle.context_budget()) {
            (Some(explicit), Some(declared)) => {
                cmp::min_by_key(explicit, declared, |budget| budget.max_request_weight())
            }
            (explicit, declared) => explicit.or(declared).unwrap_or(context_budget),
        };
        let spec = handle.spec();
        let reasoning_effort = spec
            .capabilities()
            .reasoning()
            .then_some(ReasoningEffort::Low);

        Self {
            concurrency: handle.concurrency(),
            context_budget,
            context_calibration: ContextCalibration::default(),
            context_estimator,
            id: handle.id().clone(),
            max_output: spec.max_output(),
            model: handle.model(),
            pricing: spec.pricing(),
            reasoning_effort,
        }
    }

    /// Returns `request` at this model's reasoning depth, replacing the
    /// depth the request asked of the session's model.
    pub(crate) fn request(&self, request: &ModelRequest) -> ModelRequest {
        request
            .clone()
            .with_optional_reasoning_effort(self.reasoning_effort)
    }
}

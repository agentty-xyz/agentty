use std::num::NonZeroU64;

use ag_harness::model::ContextBudget;

/// Returns a budget no fixture reaches, so projection keeps every loaded turn
/// and admission never rejects mandatory content.
pub(crate) fn unbounded_context_budget() -> ContextBudget {
    ContextBudget::new(NonZeroU64::MAX)
}

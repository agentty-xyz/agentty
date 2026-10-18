use ag_harness::model::ModelProfile;

/// Returns a profile no fixture fills, so projection keeps every loaded turn
/// and admission never rejects mandatory content.
pub(crate) fn unbounded_model_profile() -> ModelProfile {
    ModelProfile::new(u64::MAX)
}

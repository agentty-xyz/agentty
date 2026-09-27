/// One campaign task projected into scheduling facts by its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CampaignCandidate<Key> {
    /// Stable task identity returned when selected.
    pub key: Key,
    /// Whether the task is approved and ready to claim.
    pub planned: bool,
    /// Whether the task already consumes campaign capacity.
    pub occupies_slot: bool,
    /// Whether fan-in considers the task complete.
    pub settled: bool,
}

/// Pure selection result over a campaign snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignDecision<Key> {
    /// Planned tasks admitted in input order.
    pub selected: Vec<Key>,
    /// Whether every task reached a fan-in state.
    pub all_settled: bool,
}

/// Selects planned tasks in input order within remaining campaign capacity.
/// An empty campaign cannot roll up.
pub fn select_campaign_tasks<Key: Copy>(
    limit: usize,
    candidates: &[CampaignCandidate<Key>],
) -> CampaignDecision<Key> {
    let occupied = candidates
        .iter()
        .filter(|candidate| candidate.occupies_slot)
        .count();
    let available = limit.saturating_sub(occupied);
    let selected = candidates
        .iter()
        .filter(|candidate| candidate.planned)
        .take(available)
        .map(|candidate| candidate.key)
        .collect();

    CampaignDecision {
        selected,
        all_settled: !candidates.is_empty() && candidates.iter().all(|candidate| candidate.settled),
    }
}

#[cfg(test)]
#[path = "campaign_test.rs"]
mod tests;

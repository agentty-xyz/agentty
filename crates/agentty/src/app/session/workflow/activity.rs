//! Bounded per-turn activity reduction for durable display-only footers.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ag_contracts::{ActivityEvent, ActivityKind, ActivityStatus};

/// Reduces snapshots without counting lifecycle updates as additional calls.
#[derive(Default)]
pub(super) struct TurnActivity {
    calls: BTreeMap<(String, String), ActivityEvent>,
}

impl TurnActivity {
    /// Drains utility activity while its submission runs, retaining only the
    /// bounded per-call state needed for a footer.
    pub(super) async fn collect(
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<ActivityEvent>,
    ) -> Self {
        let mut activity = Self::default();
        while let Some(event) = receiver.recv().await {
            activity.observe(event);
        }

        activity
    }

    pub(super) fn observe(&mut self, event: ActivityEvent) {
        let key = (event.attempt_id.clone(), event.id.clone());
        if self.calls.contains_key(&key) || self.calls.len() < 2048 {
            self.calls.insert(key, event);
        }
    }

    /// Summarizes actual invocations; missing terminal events remain explicit.
    pub(super) fn summary(&self) -> String {
        let mut groups = BTreeMap::<(bool, &str), (usize, usize, usize)>::new();
        for event in self.calls.values() {
            let counts = groups
                .entry((event.kind == ActivityKind::Skill, event.name.as_str()))
                .or_default();
            counts.0 += 1;
            counts.1 += usize::from(event.status == ActivityStatus::Failed);
            counts.2 += usize::from(matches!(
                event.status,
                ActivityStatus::Running | ActivityStatus::Interrupted
            ));
        }
        let mut summary = String::new();
        for skill in [false, true] {
            let mut separator = if skill { "Skills: " } else { "Tools: " };
            for ((is_skill, name), (count, failed, interrupted)) in &groups {
                if *is_skill != skill {
                    continue;
                }
                let _ = write!(summary, "{separator}{name} ×{count}");
                if *failed > 0 {
                    let _ = write!(summary, " ({failed} failed)");
                }
                if *interrupted > 0 {
                    let _ = write!(summary, " ({interrupted} interrupted)");
                }
                separator = ", ";
            }
            if separator == ", " {
                summary.push('\n');
            }
        }
        summary
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;

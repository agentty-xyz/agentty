//! Run measurements derived from the turn report and its request records:
//! repeated reads and prompt-cache reuse.

use std::collections::BTreeSet;

use ag_harness::turn::ToolActivity;
use serde_json::{Map, Value, json};

/// Share of the append-only bound a request must reuse from the cache for
/// its prefix to count as held. Below it, the provider served the request
/// from an older or shorter prefix than the previous request it extends.
const HELD_PREFIX_PERCENT: u64 = 85;

/// Counts reads that repeat an earlier read target in the same turn: a file
/// read at the same path and starting line, an inspection with the same
/// canonical arguments, or a repeat the harness answered without running.
/// An inspection from a report recorded before arguments were reported
/// falls back to its action and summary.
pub(super) fn duplicate_reads(tool_calls: &[ToolActivity]) -> usize {
    let mut seen = BTreeSet::new();
    let mut duplicates = 0;
    for activity in tool_calls {
        let target = match activity {
            ToolActivity::Read {
                path, start_line, ..
            } => format!("file:{path}:{start_line}"),
            ToolActivity::ReadInspection {
                action,
                arguments,
                summary,
                ..
            } if arguments.is_empty() => format!("{action:?}:{summary}"),
            ToolActivity::ReadInspection { arguments, .. } => format!("inspection:{arguments}"),
            ToolActivity::ReadRepeated { .. } => {
                duplicates += 1;
                continue;
            }
            _ => continue,
        };
        if !seen.insert(target) {
            duplicates += 1;
        }
    }

    duplicates
}

/// Counts reads answered with the call that fetches them because earlier
/// results of their response already filled the round's share.
pub(super) fn deferred_reads(tool_calls: &[ToolActivity]) -> usize {
    tool_calls
        .iter()
        .filter(|activity| matches!(activity, ToolActivity::ReadDeferred { .. }))
        .count()
}

/// Adds prompt-cache reuse to each turn request record, measured against
/// the previous turn request, and returns the run's cache totals.
///
/// A turn request extends the previous one, so an append-only cache could
/// reuse up to the smaller of the two inputs (`cache_expected_tokens`). The
/// prefix is held when the provider reused at least 85% of that bound
/// (`prefix_held`). `reduction_event` marks a request whose reductions
/// differ from the previous turn request's, which rewrote earlier bytes or
/// newly cut the latest round. A lost prefix at a reduction event is
/// charged to the harness and any other to the provider. Context summary
/// requests are left out of the pairs but count toward the cached share.
pub(super) fn record_cache_reuse(requests: &mut [Value]) -> Map<String, Value> {
    let mut totals = CacheTotals::default();
    let mut previous: Option<(Option<u64>, Value)> = None;
    for request in requests.iter_mut() {
        let input = request["input_tokens"].as_u64();
        let hit = request["cache_hit_tokens"].as_u64();
        totals.add_usage(input, hit);
        if request["purpose"] != "turn" {
            continue;
        }
        let reductions = request["reductions"].clone();
        let (mut expected, mut held, mut reduced) = (None, None, None);
        if let Some((previous_input, previous_reductions)) = &previous {
            let event = *previous_reductions != reductions;
            totals.reduction_events += usize::from(event);
            expected = previous_input
                .zip(input)
                .map(|(before, now)| before.min(now));
            held = expected
                .zip(hit)
                .map(|(expected, hit)| totals.add_pair(expected, hit, event));
            reduced = Some(event);
        }
        if let Some(record) = request.as_object_mut() {
            record.insert("cache_expected_tokens".into(), json!(expected));
            record.insert("prefix_held".into(), json!(held));
            record.insert("reduction_event".into(), json!(reduced));
        }
        previous = Some((input, reductions));
    }

    totals.into_fields()
}

/// Run-level prompt-cache totals over every reported request.
#[derive(Default)]
struct CacheTotals {
    cache_hit_tokens: Option<u64>,
    cached_input_tokens: u64,
    held_prefixes: usize,
    lost_harness_prefixes: usize,
    lost_harness_tokens: u64,
    lost_provider_prefixes: usize,
    lost_provider_tokens: u64,
    reduction_events: usize,
}

impl CacheTotals {
    fn add_usage(&mut self, input: Option<u64>, hit: Option<u64>) {
        if let Some(hit) = hit {
            self.cache_hit_tokens = Some(self.cache_hit_tokens.unwrap_or(0) + hit);
            self.cached_input_tokens += input.unwrap_or(0);
        }
    }

    /// Counts one pair of turn requests and returns whether its prefix held.
    fn add_pair(&mut self, expected: u64, hit: u64, reduced: bool) -> bool {
        let held = hit * 100 >= expected * HELD_PREFIX_PERCENT;
        let lost = expected.saturating_sub(hit);
        match (held, reduced) {
            (true, _) => self.held_prefixes += 1,
            (false, true) => {
                self.lost_harness_prefixes += 1;
                self.lost_harness_tokens += lost;
            }
            (false, false) => {
                self.lost_provider_prefixes += 1;
                self.lost_provider_tokens += lost;
            }
        }

        held
    }

    fn into_fields(self) -> Map<String, Value> {
        let cached_share = self
            .cache_hit_tokens
            .filter(|_| self.cached_input_tokens > 0)
            .map(|hit| {
                let permyriad =
                    (hit * 10_000 + self.cached_input_tokens / 2) / self.cached_input_tokens;

                f64::from(u32::try_from(permyriad).unwrap_or(u32::MAX)) / 10_000.0
            });
        let mut fields = Map::new();
        fields.insert("cache_hit_tokens".into(), json!(self.cache_hit_tokens));
        fields.insert("cached_share".into(), json!(cached_share));
        fields.insert("reduction_events".into(), json!(self.reduction_events));
        fields.insert("held_prefixes".into(), json!(self.held_prefixes));
        fields.insert(
            "lost_prefixes".into(),
            json!({
                "harness": self.lost_harness_prefixes,
                "provider": self.lost_provider_prefixes,
            }),
        );
        fields.insert(
            "lost_cache_tokens".into(),
            json!({
                "harness": self.lost_harness_tokens,
                "provider": self.lost_provider_tokens,
            }),
        );

        fields
    }
}

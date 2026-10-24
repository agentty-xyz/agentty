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
/// differ from the previous turn request's. When both requests carry the
/// digests of the messages they `sent` (consumed here), `prefix_messages`
/// counts the leading messages they share and `prefix_rewritten` marks a
/// request that did not keep every message of the previous one, so the
/// harness rewrote earlier bytes. A lost prefix is charged to the harness
/// when the request rewrote it, or, without digests, at a reduction event,
/// and to the provider otherwise. A pair whose requests belong to different
/// `turn`s of a session is a turn boundary. Context summary requests are
/// left out of the pairs but count toward the cached share.
pub(super) fn record_cache_reuse(requests: &mut [Value]) -> Map<String, Value> {
    let mut totals = CacheTotals::default();
    let mut previous: Option<PreviousRequest> = None;
    for request in requests.iter_mut() {
        let input = request["input_tokens"].as_u64();
        let hit = request["cache_hit_tokens"].as_u64();
        let sent = request
            .as_object_mut()
            .and_then(|record| record.remove("sent"))
            .and_then(|sent| serde_json::from_value::<Vec<u64>>(sent).ok());
        totals.add_usage(input, hit);
        if request["purpose"] != "turn" {
            continue;
        }
        let reductions = request["reductions"].clone();
        let turn = request["turn"].clone();
        let mut pair = Pair::default();
        if let Some(previous) = &previous {
            let event = previous.reductions != reductions;
            totals.reduction_events += usize::from(event);
            pair.kept = previous
                .sent
                .as_ref()
                .zip(sent.as_ref())
                .map(|(before, now)| {
                    let kept = before
                        .iter()
                        .zip(now)
                        .take_while(|(earlier, later)| earlier == later)
                        .count();

                    (kept, kept < before.len())
                });
            let rewritten = pair.kept.map(|(_, rewritten)| rewritten);
            totals.rewritten_prefixes += usize::from(rewritten == Some(true));
            pair.expected = previous
                .input
                .zip(input)
                .map(|(before, now)| before.min(now));
            pair.held = pair
                .expected
                .zip(hit)
                .map(|(expected, hit)| totals.add_pair(expected, hit, rewritten.unwrap_or(event)));
            pair.reduced = Some(event);
            if previous.turn != turn {
                totals.add_boundary(pair.held, rewritten);
            }
        }
        if let Some(record) = request.as_object_mut() {
            record.insert("cache_expected_tokens".into(), json!(pair.expected));
            record.insert("prefix_held".into(), json!(pair.held));
            record.insert("reduction_event".into(), json!(pair.reduced));
            record.insert(
                "prefix_messages".into(),
                json!(pair.kept.map(|(kept, _)| kept)),
            );
            record.insert(
                "prefix_rewritten".into(),
                json!(pair.kept.map(|(_, rewritten)| rewritten)),
            );
        }
        previous = Some(PreviousRequest {
            input,
            reductions,
            sent,
            turn,
        });
    }

    totals.into_fields()
}

/// The previous turn request a turn request is compared with.
struct PreviousRequest {
    input: Option<u64>,
    reductions: Value,
    sent: Option<Vec<u64>>,
    turn: Value,
}

/// What one turn request kept of the previous one.
#[derive(Default)]
struct Pair {
    expected: Option<u64>,
    held: Option<bool>,
    /// Leading messages shared with the previous request, and whether that
    /// left any of its messages out.
    kept: Option<(usize, bool)>,
    reduced: Option<bool>,
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
    rewritten_prefixes: usize,
    turn_boundaries: usize,
    turn_boundaries_held: usize,
    turn_boundaries_rewritten: usize,
}

impl CacheTotals {
    /// Counts one pair whose requests belong to different turns.
    fn add_boundary(&mut self, held: Option<bool>, rewritten: Option<bool>) {
        self.turn_boundaries += 1;
        self.turn_boundaries_held += usize::from(held == Some(true));
        self.turn_boundaries_rewritten += usize::from(rewritten == Some(true));
    }

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
        fields.insert("rewritten_prefixes".into(), json!(self.rewritten_prefixes));
        fields.insert(
            "turn_boundaries".into(),
            json!({
                "pairs": self.turn_boundaries,
                "held": self.turn_boundaries_held,
                "rewritten": self.turn_boundaries_rewritten,
            }),
        );
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

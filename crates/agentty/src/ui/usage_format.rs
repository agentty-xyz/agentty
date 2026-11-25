//! Usage display formatting: money and token figures, honesty states, and
//! the per-session usage panel's cells, summary, and notes.
//!
//! Each figure is a [`UsageFigure`] that records how exact it is, so every
//! surface renders the same states its own way. The panel writes a lower
//! bound with `≥`, a figure no request reported as `?`, a cost without a
//! price as `unknown`, and a detail the backend did not record as `—`. Each
//! state that appears gets one note naming its cause.

use std::collections::HashMap;

use ag_contracts::{CostBasis, TurnUsage, UsagePurpose};
use ag_tui_text::text_util;

use crate::domain::session::SessionId;
use crate::domain::session_usage::{ContextNow, LiveUsageKey, SessionUsage, UsageRecord};

/// Prefix of a figure that is a lower bound.
const LOWER_BOUND_MARK: &str = "≥";

/// Panel spelling of a figure no request reported.
const UNREPORTED_TEXT: &str = "?";

/// Panel spelling of a detail the backend did not record.
const NOT_RECORDED_TEXT: &str = "—";

/// Panel spelling of a cost without a price.
const UNPRICED_TEXT: &str = "unknown";

/// Money text of a lower-bound charge below half a cent: the true charge is
/// at least what was counted, and one mark per figure is enough.
const LOWER_BOUND_SUB_CENT_TEXT: &str = "$0.00";

/// Label of the bold totals row.
const SESSION_TOTAL_LABEL: &str = "Session total";

/// Cells of the context gauge.
pub(crate) const CONTEXT_GAUGE_CELLS: u64 = 5;

const MICRO_USD_PER_CENT: u64 = 10_000;
const MICRO_USD_PER_DOLLAR: u64 = 1_000_000;
const CENTS_PER_THOUSAND_DOLLARS: u64 = 100_000;

/// One usage figure and how exact it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UsageFigure {
    /// A recorded figure, formatted without its mark.
    Known {
        /// Whether some usage is left out, so the true value is at least
        /// this.
        is_lower_bound: bool,
        /// Formatted figure.
        text: String,
    },
    /// The backend did not record this figure.
    NotRecorded,
    /// No request reported this figure.
    Unreported,
    /// The usage has no price.
    Unpriced,
}

impl UsageFigure {
    /// Returns how the usage panel writes this figure.
    pub(crate) fn panel_text(&self) -> String {
        match self {
            Self::Known {
                is_lower_bound,
                text,
            } => {
                if *is_lower_bound {
                    return format!("{LOWER_BOUND_MARK}{text}");
                }

                text.clone()
            }
            Self::NotRecorded => NOT_RECORDED_TEXT.to_string(),
            Self::Unreported => UNREPORTED_TEXT.to_string(),
            Self::Unpriced => UNPRICED_TEXT.to_string(),
        }
    }

    /// Returns whether this figure is a known lower bound.
    fn is_lower_bound(&self) -> bool {
        matches!(
            self,
            Self::Known {
                is_lower_bound: true,
                ..
            }
        )
    }

    /// Returns a known token count.
    fn tokens(count: u64, is_lower_bound: bool) -> Self {
        Self::Known {
            is_lower_bound,
            text: text_util::format_token_count(count),
        }
    }

    /// Returns a known charge; a lower bound below half a cent reads
    /// `$0.00` rather than carrying a second mark.
    fn cost(micro_usd: u64, is_lower_bound: bool) -> Self {
        let text = if is_lower_bound && micro_usd < MICRO_USD_PER_CENT / 2 {
            LOWER_BOUND_SUB_CENT_TEXT.to_string()
        } else {
            format_money(micro_usd)
        };

        Self::Known {
            is_lower_bound,
            text,
        }
    }
}

/// One table column of the usage panel, after the model column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsageColumn {
    /// Provider requests.
    Requests,
    /// Input tokens, cache included.
    Input,
    /// Input tokens served from the prompt cache.
    Cached,
    /// Input tokens written to the prompt cache.
    CacheWrite,
    /// Output tokens, reasoning included.
    Output,
    /// Reasoning tokens.
    Reasoning,
    /// Largest request input against the model's window.
    ContextPeak,
    /// Estimated charge.
    Cost,
}

impl UsageColumn {
    /// Every column in display order, which is also variant order.
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Requests,
        Self::Input,
        Self::Cached,
        Self::CacheWrite,
        Self::Output,
        Self::Reasoning,
        Self::ContextPeak,
        Self::Cost,
    ];
    /// Number of columns; [`Self::Cost`] is the last variant.
    pub(crate) const COUNT: usize = Self::Cost as usize + 1;
    /// Columns dropped first when the panel is too narrow, in order. The
    /// cost column is never dropped.
    pub(crate) const DROP_ORDER: [Self; Self::COUNT - 1] = [
        Self::CacheWrite,
        Self::Reasoning,
        Self::ContextPeak,
        Self::Cached,
        Self::Requests,
        Self::Output,
        Self::Input,
    ];

    /// Returns the column header.
    pub(crate) fn header(self) -> &'static str {
        match self {
            Self::Requests => "Req",
            Self::Input => "Input",
            Self::Cached => "Cached",
            Self::CacheWrite => "Cache wr",
            Self::Output => "Output",
            Self::Reasoning => "Reason",
            Self::ContextPeak => "Ctx peak",
            Self::Cost => "Est. cost",
        }
    }

    /// Returns the column's position in [`Self::ALL`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Returns the columns that fit in `available_width` beside a model column
/// of at least `model_min_width`, separated by `gap_width`, dropping columns
/// by [`UsageColumn::DROP_ORDER`].
pub(crate) fn visible_usage_columns(
    column_widths: &[usize; UsageColumn::COUNT],
    model_min_width: usize,
    gap_width: usize,
    available_width: usize,
) -> Vec<UsageColumn> {
    let mut columns = UsageColumn::ALL.to_vec();
    let required_width = |columns: &[UsageColumn]| {
        columns.iter().fold(model_min_width, |width, column| {
            width + gap_width + column_widths[column.index()]
        })
    };
    for dropped in UsageColumn::DROP_ORDER {
        if required_width(&columns) <= available_width {
            break;
        }
        columns.retain(|column| *column != dropped);
    }

    columns
}

/// One row of the usage table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageTableRow {
    /// Cells in [`UsageColumn::ALL`] order.
    pub(crate) cells: [UsageFigure; UsageColumn::COUNT],
    /// Model identifier, or the totals label.
    pub(crate) model: String,
    /// Short purpose label shown dimmed after the model, absent for session
    /// turns and the totals row.
    pub(crate) purpose: Option<&'static str>,
}

impl UsageTableRow {
    /// Returns the cell of `column`.
    pub(crate) fn cell(&self, column: UsageColumn) -> &UsageFigure {
        &self.cells[column.index()]
    }
}

/// Context line of the usage summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContextSummary {
    /// Filled gauge cells out of [`CONTEXT_GAUGE_CELLS`], when the window is
    /// known.
    pub(crate) gauge_filled: Option<u64>,
    /// Latest fill, such as `384.2k / 1.0M now (38%)`.
    pub(crate) now: Option<String>,
    /// Latest fill as a whole percentage of the window, when known.
    pub(crate) now_percent: Option<u64>,
    /// Largest fill, such as `612.4k (61%)`.
    pub(crate) peak: Option<String>,
}

/// Headline figures above the usage table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageSummary {
    /// Share of input served from cache, when any model reports it.
    pub(crate) cache: Option<String>,
    /// Context fill, when any request reported its input.
    pub(crate) context: Option<ContextSummary>,
    /// Estimated session cost.
    pub(crate) cost: UsageFigure,
    /// Short explanation of the cost figure.
    pub(crate) cost_explanation: &'static str,
    /// Provider requests.
    pub(crate) requests: UsageFigure,
    /// Input and output tokens.
    pub(crate) tokens: UsageFigure,
    /// Turns started.
    pub(crate) turns: String,
}

/// One note line explaining a mark shown in the panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsageNote {
    /// Whether the note states that costs are estimates rather than
    /// explaining an inexact figure.
    pub(crate) is_estimate: bool,
    /// Mark the note explains.
    pub(crate) mark: &'static str,
    /// Cause of the mark.
    pub(crate) text: String,
}

/// Everything the usage panel shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsagePanelContent {
    /// One note per mark that appears.
    pub(crate) notes: Vec<UsageNote>,
    /// One row per model and purpose.
    pub(crate) rows: Vec<UsageTableRow>,
    /// Headline figures.
    pub(crate) summary: UsageSummary,
    /// Bold totals row.
    pub(crate) total: UsageTableRow,
}

/// Builds the panel content of `session_id`: its recorded `usage` with the
/// running turns in `live` that belong to it folded in.
pub(crate) fn session_panel_content(
    usage: &SessionUsage,
    session_id: &SessionId,
    live: &HashMap<LiveUsageKey, TurnUsage>,
) -> UsagePanelContent {
    let session_live = live
        .iter()
        .filter(|(key, _)| key.session_id == *session_id)
        .map(|(key, turn_usage)| (&key.run, turn_usage));

    usage_panel_content(&usage.with_live(session_live))
}

/// Builds the usage panel's rows, totals, summary, and notes for `usage`.
pub(crate) fn usage_panel_content(usage: &SessionUsage) -> UsagePanelContent {
    let rows = usage.records.iter().map(record_row).collect::<Vec<_>>();
    let total = total_row(&usage.records);
    let summary = usage_summary(usage, &total);
    let notes = usage_notes(&usage.records, &rows, &total, &summary);

    UsagePanelContent {
        notes,
        rows,
        summary,
        total,
    }
}

/// Returns the dimmed label shown after the model for `purpose`.
fn purpose_label(purpose: UsagePurpose) -> Option<&'static str> {
    match purpose {
        UsagePurpose::Turn => None,
        UsagePurpose::Title => Some("title"),
        UsagePurpose::CommitAssist => Some("commit"),
        UsagePurpose::Review => Some("review"),
        UsagePurpose::Rebase => Some("rebase"),
        UsagePurpose::Other => Some("other"),
    }
}

/// Builds the table row of one record.
fn record_row(record: &UsageRecord) -> UsageTableRow {
    let is_unreported = record.is_fully_unreported();
    let has_unreported = record.unreported_request_count > 0;
    let token_cell = |tokens: u64, is_lower_bound: bool| {
        if is_unreported {
            return UsageFigure::Unreported;
        }

        UsageFigure::tokens(tokens, is_lower_bound)
    };
    let detail_cell = |tokens: Option<u64>| {
        if is_unreported {
            return UsageFigure::Unreported;
        }

        tokens.map_or(UsageFigure::NotRecorded, |tokens| {
            UsageFigure::tokens(tokens, has_unreported)
        })
    };
    let requests = if record.detailed_invocation_count == 0 {
        UsageFigure::NotRecorded
    } else {
        UsageFigure::Known {
            is_lower_bound: record.undetailed_invocation_count() > 0,
            text: record.request_count.to_string(),
        }
    };
    let context_peak = if is_unreported {
        UsageFigure::Unreported
    } else {
        record
            .context_peak_tokens
            .map_or(UsageFigure::NotRecorded, |peak| {
                context_figure(peak, record.context_window_tokens)
            })
    };

    UsageTableRow {
        cells: [
            requests,
            token_cell(
                record.input_tokens,
                has_unreported || record.undetailed_invocation_count() > 0,
            ),
            detail_cell(record.cache_read_tokens),
            detail_cell(record.cache_write_tokens),
            token_cell(record.output_tokens, has_unreported),
            detail_cell(record.reasoning_tokens),
            context_peak,
            record_cost_cell(record),
        ],
        model: record.model.clone(),
        purpose: purpose_label(record.purpose),
    }
}

/// Returns the cost cell of one record.
fn record_cost_cell(record: &UsageRecord) -> UsageFigure {
    match record.cost_micro_usd {
        Some(micro_usd) => UsageFigure::cost(micro_usd, record_cost_is_lower_bound(record)),
        None if record.detailed_invocation_count == 0 => UsageFigure::NotRecorded,
        None => UsageFigure::Unpriced,
    }
}

/// Returns whether a record's known cost leaves out some of its usage.
fn record_cost_is_lower_bound(record: &UsageRecord) -> bool {
    record.unpriced_invocation_count > 0
        || record.unreported_request_count > 0
        || record.undetailed_invocation_count() > 0
}

/// Builds the bold totals row over every record.
fn total_row(records: &[UsageRecord]) -> UsageTableRow {
    let all_unreported =
        !records.is_empty() && records.iter().all(UsageRecord::is_fully_unreported);
    let any_unreported = records
        .iter()
        .any(|record| record.unreported_request_count > 0);
    let any_undetailed = records
        .iter()
        .any(|record| record.undetailed_invocation_count() > 0);
    let token_total = |tokens: fn(&UsageRecord) -> u64, is_lower_bound: bool| {
        if all_unreported {
            return UsageFigure::Unreported;
        }
        let total = records
            .iter()
            .fold(0_u64, |total, record| total.saturating_add(tokens(record)));

        UsageFigure::tokens(total, is_lower_bound)
    };
    let requests = if records
        .iter()
        .all(|record| record.detailed_invocation_count == 0)
    {
        UsageFigure::NotRecorded
    } else {
        let total = records.iter().fold(0_u64, |total, record| {
            total.saturating_add(record.request_count)
        });

        UsageFigure::Known {
            is_lower_bound: any_undetailed,
            text: total.to_string(),
        }
    };

    UsageTableRow {
        cells: [
            requests,
            token_total(
                |record| record.input_tokens,
                any_unreported || any_undetailed,
            ),
            detail_total(records, |record| record.cache_read_tokens),
            detail_total(records, |record| record.cache_write_tokens),
            token_total(|record| record.output_tokens, any_unreported),
            detail_total(records, |record| record.reasoning_tokens),
            context_peak_total(records),
            total_cost_cell(records),
        ],
        model: SESSION_TOTAL_LABEL.to_string(),
        purpose: None,
    }
}

/// Sums a detail column, marking the total a lower bound when any record
/// left it unknown or had unreported requests.
fn detail_total(records: &[UsageRecord], detail: fn(&UsageRecord) -> Option<u64>) -> UsageFigure {
    let known = records.iter().filter_map(detail).collect::<Vec<_>>();
    if known.is_empty() {
        if !records.is_empty() && records.iter().all(UsageRecord::is_fully_unreported) {
            return UsageFigure::Unreported;
        }

        return UsageFigure::NotRecorded;
    }
    let total = known
        .iter()
        .fold(0_u64, |total, tokens| total.saturating_add(*tokens));
    let is_lower_bound = known.len() < records.len()
        || records
            .iter()
            .any(|record| record.unreported_request_count > 0);

    UsageFigure::tokens(total, is_lower_bound)
}

/// Returns the largest context fill over every record: a percentage when a
/// window is known, else the absolute peak.
fn context_peak_total(records: &[UsageRecord]) -> UsageFigure {
    peak_context(records).map_or_else(
        || {
            if !records.is_empty() && records.iter().all(UsageRecord::is_fully_unreported) {
                return UsageFigure::Unreported;
            }

            UsageFigure::NotRecorded
        },
        |(peak, window)| context_figure(peak, window),
    )
}

/// Returns the largest context peak and its window, preferring peaks with a
/// known window, compared by their share of it.
fn peak_context(records: &[UsageRecord]) -> Option<(u64, Option<u64>)> {
    let windowed = records
        .iter()
        .filter_map(|record| {
            record
                .context_peak_tokens
                .zip(record.context_window_tokens.filter(|window| *window > 0))
        })
        .max_by_key(|(peak, window)| u128::from(*peak) * 1_000_000 / u128::from(*window));
    if let Some((peak, window)) = windowed {
        return Some((peak, Some(window)));
    }

    records
        .iter()
        .filter_map(|record| record.context_peak_tokens)
        .max()
        .map(|peak| (peak, None))
}

/// Returns the session cost cell: the sum of known costs, a lower bound when
/// any usage was left out, and `unknown` when nothing was priced.
fn total_cost_cell(records: &[UsageRecord]) -> UsageFigure {
    let known = records
        .iter()
        .filter_map(|record| record.cost_micro_usd)
        .collect::<Vec<_>>();
    if known.is_empty() {
        if records
            .iter()
            .any(|record| record.detailed_invocation_count > 0)
        {
            return UsageFigure::Unpriced;
        }

        return UsageFigure::NotRecorded;
    }
    let total = known
        .iter()
        .fold(0_u64, |total, micro_usd| total.saturating_add(*micro_usd));
    let is_lower_bound = records
        .iter()
        .any(|record| record.cost_micro_usd.is_none() || record_cost_is_lower_bound(record));

    UsageFigure::cost(total, is_lower_bound)
}

/// Builds the headline figures from the totals row and session context.
fn usage_summary(usage: &SessionUsage, total: &UsageTableRow) -> UsageSummary {
    let cost = total.cell(UsageColumn::Cost).clone();
    let cost_explanation = cost_explanation(&cost, &usage.records);
    let input = total.cell(UsageColumn::Input);
    let tokens = if *input == UsageFigure::Unreported {
        UsageFigure::Unreported
    } else {
        let total_tokens = usage.records.iter().fold(0_u64, |total, record| {
            total
                .saturating_add(record.input_tokens)
                .saturating_add(record.output_tokens)
        });
        let is_lower_bound =
            input.is_lower_bound() || total.cell(UsageColumn::Output).is_lower_bound();

        UsageFigure::tokens(total_tokens, is_lower_bound)
    };

    UsageSummary {
        cache: cache_share(&usage.records),
        context: context_summary(usage),
        cost,
        cost_explanation,
        requests: total.cell(UsageColumn::Requests).clone(),
        tokens,
        turns: usage.turn_count.to_string(),
    }
}

/// Explains the session cost figure in a short phrase.
fn cost_explanation(cost: &UsageFigure, records: &[UsageRecord]) -> &'static str {
    match cost {
        UsageFigure::Known {
            is_lower_bound: true,
            ..
        } => "lower bound",
        UsageFigure::Known { .. } => match combined_cost_basis(records) {
            Some(CostBasis::ListPrice) => "from list prices",
            Some(CostBasis::HostPrice) => "from configured prices",
            Some(CostBasis::AgentReported) => "as the agent reported",
            Some(CostBasis::Mixed) | None => "from several price sources",
        },
        UsageFigure::Unpriced => "no price for these models",
        UsageFigure::NotRecorded | UsageFigure::Unreported => "not recorded",
    }
}

/// Returns the one basis every priced record shares, or
/// [`CostBasis::Mixed`] when they differ.
fn combined_cost_basis(records: &[UsageRecord]) -> Option<CostBasis> {
    records
        .iter()
        .filter_map(|record| record.cost_basis)
        .reduce(|basis, next| {
            if basis == next {
                basis
            } else {
                CostBasis::Mixed
            }
        })
}

/// Returns the share of input served from cache over the records that
/// report cache reads, when any of them had input.
fn cache_share(records: &[UsageRecord]) -> Option<String> {
    let (cached, input) = records
        .iter()
        .filter_map(|record| {
            record
                .cache_read_tokens
                .map(|cached| (cached, record.input_tokens))
        })
        .fold(
            (0_u64, 0_u64),
            |(cached, input), (record_cached, record_input)| {
                (
                    cached.saturating_add(record_cached),
                    input.saturating_add(record_input),
                )
            },
        );
    if input == 0 {
        return None;
    }
    let is_partial = records
        .iter()
        .any(|record| record.cache_read_tokens.is_none() && record.input_tokens > 0);
    let scope = if is_partial {
        " (models that report it)"
    } else {
        ""
    };

    Some(format!(
        "{} of input served from cache{scope}",
        format_percent(cached.min(input), input)
    ))
}

/// Builds the context line from the latest fill and the largest peak.
fn context_summary(usage: &SessionUsage) -> Option<ContextSummary> {
    let peak = peak_context(&usage.records).map(|(peak, window)| fill_figure(peak, window, ""));
    let (gauge_filled, now, now_percent) = match usage.context {
        Some(ContextNow {
            tokens,
            window_tokens,
        }) => {
            let window = window_tokens.filter(|window| *window > 0);

            (
                window.map(|window| gauge_filled(tokens, window)),
                Some(fill_figure(tokens, window_tokens, " now")),
                window.map(|window| {
                    u64::try_from(rounded_percent(tokens, window)).unwrap_or(u64::MAX)
                }),
            )
        }
        None => (None, None, None),
    };
    if now.is_none() && peak.is_none() {
        return None;
    }

    Some(ContextSummary {
        gauge_filled,
        now,
        now_percent,
        peak,
    })
}

/// Formats a fill as `tokens / window<suffix> (pct)`, or `tokens<suffix>`
/// when the window is unknown.
fn fill_figure(tokens: u64, window: Option<u64>, suffix: &str) -> String {
    let tokens_label = text_util::format_token_count(tokens);
    match window.filter(|window| *window > 0) {
        Some(window) => format!(
            "{tokens_label} / {}{suffix} ({})",
            text_util::format_token_count(window),
            format_percent(tokens, window)
        ),
        None => format!("{tokens_label}{suffix}"),
    }
}

/// Returns the gauge cells `tokens` fills of `window`.
fn gauge_filled(tokens: u64, window: u64) -> u64 {
    let filled = (u128::from(tokens) * u128::from(CONTEXT_GAUGE_CELLS) + u128::from(window) / 2)
        / u128::from(window);

    u64::try_from(filled)
        .unwrap_or(CONTEXT_GAUGE_CELLS)
        .min(CONTEXT_GAUGE_CELLS)
}

/// Builds one note per figure state that appears in the rows, totals, or
/// summary.
fn usage_notes(
    records: &[UsageRecord],
    rows: &[UsageTableRow],
    total: &UsageTableRow,
    summary: &UsageSummary,
) -> Vec<UsageNote> {
    let figures = rows
        .iter()
        .chain(std::iter::once(total))
        .flat_map(|row| row.cells.iter())
        .chain([&summary.cost, &summary.requests, &summary.tokens])
        .collect::<Vec<_>>();
    let has_figure = |state: &UsageFigure| figures.contains(&state);
    let mut notes = Vec::new();
    let mut push_note = |mark: &'static str, label: &str, causes: Vec<String>| {
        if !causes.is_empty() {
            notes.push(UsageNote {
                is_estimate: false,
                mark,
                text: format!("{label}: {}.", causes.join("; ")),
            });
        }
    };
    if figures.iter().any(|figure| figure.is_lower_bound()) {
        push_note(LOWER_BOUND_MARK, "Lower bound", lower_bound_causes(records));
    }
    if has_figure(&UsageFigure::Unreported) {
        push_note(UNREPORTED_TEXT, "Unreported", unreported_causes(records));
    }
    if has_figure(&UsageFigure::Unpriced) {
        push_note(UNPRICED_TEXT, "Unpriced", unpriced_causes(records));
    }
    if has_figure(&UsageFigure::NotRecorded) {
        push_note(
            NOT_RECORDED_TEXT,
            "Not recorded",
            not_recorded_causes(records),
        );
    }
    if let Some(basis) = combined_cost_basis(records) {
        notes.push(UsageNote {
            is_estimate: true,
            mark: "$",
            text: format!(
                "Costs are estimated {}; the provider bill is authoritative.",
                basis_source(basis)
            ),
        });
    }

    notes
}

/// Names why figures are lower bounds.
fn lower_bound_causes(records: &[UsageRecord]) -> Vec<String> {
    let mut causes = Vec::new();
    let unreported = records
        .iter()
        .filter(|record| record.unreported_request_count > 0)
        .collect::<Vec<_>>();
    if !unreported.is_empty() {
        let request_count = unreported.iter().fold(0_u64, |total, record| {
            total.saturating_add(record.unreported_request_count)
        });
        causes.push(format!(
            "{} ({}) reported no token counts",
            counted(request_count, "request", "requests"),
            model_list(&unreported)
        ));
    }
    let has_priced = records.iter().any(|record| record.cost_micro_usd.is_some());
    let unpriced = records
        .iter()
        .filter(|record| record.unpriced_invocation_count > 0)
        .collect::<Vec<_>>();
    if has_priced && !unpriced.is_empty() {
        causes.push(format!(
            "the cost leaves out unpriced runs ({})",
            model_list(&unpriced)
        ));
    }
    let undetailed = records
        .iter()
        .filter(|record| record.undetailed_invocation_count() > 0)
        .collect::<Vec<_>>();
    if !undetailed.is_empty() {
        let run_count = undetailed.iter().fold(0_u64, |total, record| {
            total.saturating_add(record.undetailed_invocation_count())
        });
        let verb = if run_count == 1 { "was" } else { "were" };
        causes.push(format!(
            "{} ({}) {verb} recorded before detailed usage, so input may leave out cache reads",
            counted(run_count, "run", "runs"),
            model_list(&undetailed)
        ));
    }
    let has_partial_detail = [
        |record: &UsageRecord| record.cache_read_tokens,
        |record: &UsageRecord| record.cache_write_tokens,
        |record: &UsageRecord| record.reasoning_tokens,
    ]
    .into_iter()
    .any(|detail| {
        records.iter().any(|record| detail(record).is_some())
            && records.iter().any(|record| detail(record).is_none())
    });
    if has_partial_detail {
        causes.push("some models do not report every column".to_string());
    }

    causes
}

/// Names the models whose figures no request reported.
fn unreported_causes(records: &[UsageRecord]) -> Vec<String> {
    let unreported = records
        .iter()
        .filter(|record| record.is_fully_unreported())
        .collect::<Vec<_>>();

    vec![format!(
        "{} reported no token counts",
        model_list(&unreported)
    )]
}

/// Names the models whose usage has no price, and says the session total
/// leaves them out when other usage is priced.
fn unpriced_causes(records: &[UsageRecord]) -> Vec<String> {
    let unpriced = records
        .iter()
        .filter(|record| record.cost_micro_usd.is_none() && record.detailed_invocation_count > 0)
        .collect::<Vec<_>>();
    let has_priced = records.iter().any(|record| record.cost_micro_usd.is_some());
    let scope = if has_priced {
        ", which the total leaves out"
    } else {
        ""
    };

    vec![format!("no price for {}{scope}", model_list(&unpriced))]
}

/// Names why details read `—`.
fn not_recorded_causes(records: &[UsageRecord]) -> Vec<String> {
    let mut causes = Vec::new();
    let has_unreported_detail = records.iter().any(|record| {
        record.detailed_invocation_count > 0
            && !record.is_fully_unreported()
            && (record.cache_read_tokens.is_none()
                || record.cache_write_tokens.is_none()
                || record.reasoning_tokens.is_none()
                || record.context_peak_tokens.is_none())
    });
    if has_unreported_detail {
        causes.push("the agent backend does not report this figure".to_string());
    }
    if records
        .iter()
        .any(|record| record.undetailed_invocation_count() > 0)
    {
        causes.push("runs recorded before detailed usage".to_string());
    }

    causes
}

/// Names where estimated prices came from.
fn basis_source(basis: CostBasis) -> &'static str {
    match basis {
        CostBasis::ListPrice => "from built-in list prices",
        CostBasis::HostPrice => "from configured prices",
        CostBasis::AgentReported => "by the agent itself",
        CostBasis::Mixed => "from several price sources",
    }
}

/// Lists the models of `records`, which are ordered by model, once each.
fn model_list(records: &[&UsageRecord]) -> String {
    let mut models = records
        .iter()
        .map(|record| record.model.as_str())
        .collect::<Vec<_>>();
    models.dedup();

    models.join(", ")
}

/// Formats `count` with its singular or plural noun.
fn counted(count: u64, singular: &str, plural: &str) -> String {
    if count == 1 {
        return format!("1 {singular}");
    }

    format!("{count} {plural}")
}

/// Returns a context figure: a share of the window when known, else the
/// absolute token count.
fn context_figure(peak: u64, window: Option<u64>) -> UsageFigure {
    let text = match window.filter(|window| *window > 0) {
        Some(window) => format_percent(peak, window),
        None => text_util::format_token_count(peak),
    };

    UsageFigure::Known {
        is_lower_bound: false,
        text,
    }
}

/// Formats an estimated charge in micro-USD: two decimals, `<$0.01` for a
/// small nonzero charge, and three significant digits from $1,000.
fn format_money(micro_usd: u64) -> String {
    if micro_usd > 0 && micro_usd < MICRO_USD_PER_CENT / 2 {
        return "<$0.01".to_string();
    }
    let cents = micro_usd.saturating_add(MICRO_USD_PER_CENT / 2) / MICRO_USD_PER_CENT;
    if cents < CENTS_PER_THOUSAND_DOLLARS {
        return format!("${}.{:02}", cents / 100, cents % 100);
    }

    let dollars = micro_usd / MICRO_USD_PER_DOLLAR;
    let digit_count = dollars.max(1).ilog10() + 1;
    let scale = 10_u64.pow(digit_count.saturating_sub(3));
    let scale_micro_usd = scale.saturating_mul(MICRO_USD_PER_DOLLAR);
    let rounded_dollars = micro_usd.saturating_add(scale_micro_usd / 2) / scale_micro_usd * scale;

    format!("${}", group_thousands(rounded_dollars))
}

/// Formats a share of `whole` as a rounded whole percentage.
fn format_percent(part: u64, whole: u64) -> String {
    format!("{}%", rounded_percent(part, whole))
}

/// Returns `part` as a rounded whole percentage of `whole`.
fn rounded_percent(part: u64, whole: u64) -> u128 {
    if whole == 0 {
        return 0;
    }

    (u128::from(part) * 100 + u128::from(whole) / 2) / u128::from(whole)
}

/// Formats whole dollars with thousands separators.
fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    grouped
}

#[cfg(test)]
#[path = "usage_format_test.rs"]
mod tests;

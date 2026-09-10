use ag_forge::{ReviewComment, ReviewCommentSnapshot, ReviewCommentThread};

use super::app_mode::ReviewCommentSelection;

/// One row in the grouped review-comment selector projection.
pub(crate) enum GroupedReviewCommentRow<'a> {
    /// Selectable standalone comment or inline thread.
    Entry(ReviewCommentEntry<'a>),
    /// Non-selectable heading rendered before one populated group.
    GroupLabel(&'static str),
}

/// One selectable review-comment entry and its detail source.
#[derive(Clone, Copy)]
pub(crate) enum ReviewCommentEntry<'a> {
    /// Review-request-wide discussion comment without an inline thread ID.
    General(&'a ReviewComment),
    /// Forge review thread attached to a file or line range.
    Thread(&'a ReviewCommentThread),
}

/// Returns the complete selector projection in unresolved, outdated,
/// resolved, then standalone order, including labels for each populated
/// group.
pub(crate) fn grouped_review_comment_rows(
    snapshot: &ReviewCommentSnapshot,
) -> Vec<GroupedReviewCommentRow<'_>> {
    let mut rows = Vec::with_capacity(
        snapshot
            .threads
            .len()
            .saturating_add(snapshot.pr_level_comments.len())
            .saturating_add(4),
    );
    append_group_rows(
        &mut rows,
        "Unresolved",
        snapshot
            .threads
            .iter()
            .filter(|thread| !thread.is_resolved && thread.is_outdated != Some(true))
            .map(ReviewCommentEntry::Thread),
    );
    append_group_rows(
        &mut rows,
        "Outdated",
        snapshot
            .threads
            .iter()
            .filter(|thread| !thread.is_resolved && thread.is_outdated == Some(true))
            .map(ReviewCommentEntry::Thread),
    );
    append_group_rows(
        &mut rows,
        "Resolved",
        snapshot
            .threads
            .iter()
            .filter(|thread| thread.is_resolved)
            .map(ReviewCommentEntry::Thread),
    );
    append_group_rows(
        &mut rows,
        "Standalone",
        snapshot
            .pr_level_comments
            .iter()
            .map(ReviewCommentEntry::General),
    );

    rows
}

/// Returns only selectable entries from one materialized grouped projection.
pub(crate) fn selectable_entries<'rows, 'snapshot>(
    rows: &'rows [GroupedReviewCommentRow<'snapshot>],
) -> impl Iterator<Item = ReviewCommentEntry<'snapshot>> + 'rows
where
    'snapshot: 'rows,
{
    rows.iter().filter_map(|row| match row {
        GroupedReviewCommentRow::Entry(entry) => Some(*entry),
        GroupedReviewCommentRow::GroupLabel(_) => None,
    })
}

/// Returns the selected standalone comment or inline thread from one
/// materialized grouped projection.
pub(crate) fn selected_entry<'snapshot>(
    rows: &[GroupedReviewCommentRow<'snapshot>],
    selected_comment_index: usize,
) -> Option<ReviewCommentEntry<'snapshot>> {
    selectable_entries(rows).nth(selected_comment_index)
}

/// Returns the forge-native identifier for the selected grouped thread row.
pub(crate) fn selected_thread_id(
    snapshot: &ReviewCommentSnapshot,
    selected_comment_index: usize,
) -> Option<&str> {
    let rows = grouped_review_comment_rows(snapshot);

    selected_entry(&rows, selected_comment_index).and_then(|entry| match entry {
        ReviewCommentEntry::General(_) => None,
        ReviewCommentEntry::Thread(thread) => Some(thread.id.as_str()),
    })
}

/// Returns the selected thread identifier only when the thread is actionable.
pub(crate) fn selected_actionable_thread_id(
    snapshot: &ReviewCommentSnapshot,
    selected_comment_index: usize,
) -> Option<&str> {
    let rows = grouped_review_comment_rows(snapshot);

    selected_entry(&rows, selected_comment_index).and_then(|entry| match entry {
        ReviewCommentEntry::Thread(thread) if thread.is_actionable() => Some(thread.id.as_str()),
        ReviewCommentEntry::General(_) | ReviewCommentEntry::Thread(_) => None,
    })
}

/// Returns whether one forge thread is selected for agent evaluation.
pub(crate) fn is_selected(selections: &[ReviewCommentSelection], thread_id: &str) -> bool {
    selections
        .iter()
        .any(|selection| selection.thread_id == thread_id)
}

/// Toggles one thread's inclusion in the next agent evaluation batch.
pub(crate) fn toggle_selection(selections: &mut Vec<ReviewCommentSelection>, thread_id: &str) {
    if let Some(selection_index) = selections
        .iter()
        .position(|selection| selection.thread_id == thread_id)
    {
        selections.remove(selection_index);

        return;
    }

    selections.push(ReviewCommentSelection {
        thread_id: thread_id.to_string(),
    });
}

/// Drops selections for threads that are no longer actionable after refresh.
pub(crate) fn retain_actionable_selections(
    selections: &mut Vec<ReviewCommentSelection>,
    snapshot: &ReviewCommentSnapshot,
) {
    selections.retain(|selection| {
        snapshot
            .threads
            .iter()
            .any(|thread| thread.id == selection.thread_id && thread.is_actionable())
    });
}

/// Retargets a positional selection to the same forge thread in an updated
/// snapshot, falling back to the nearest valid row if the thread disappeared.
pub(crate) fn retarget_selected_index(
    previous_snapshot: Option<&ReviewCommentSnapshot>,
    previous_selected_index: usize,
    updated_snapshot: &ReviewCommentSnapshot,
) -> usize {
    let selected_thread_id = previous_snapshot
        .and_then(|snapshot| selected_thread_id(snapshot, previous_selected_index));
    let updated_rows = grouped_review_comment_rows(updated_snapshot);
    if let Some(updated_index) = selected_thread_id.and_then(|selected_thread_id| {
        selectable_entries(&updated_rows).position(
            |entry| matches!(entry, ReviewCommentEntry::Thread(thread) if thread.id == selected_thread_id),
        )
    }) {
        return updated_index;
    }

    let updated_item_count = updated_snapshot
        .threads
        .len()
        .saturating_add(updated_snapshot.pr_level_comments.len());

    previous_selected_index.min(updated_item_count.saturating_sub(1))
}

/// Adds one heading and its entries only when the group is populated.
fn append_group_rows<'a>(
    rows: &mut Vec<GroupedReviewCommentRow<'a>>,
    label: &'static str,
    entries: impl Iterator<Item = ReviewCommentEntry<'a>>,
) {
    let mut group_has_entries = false;
    for entry in entries {
        if !group_has_entries {
            rows.push(GroupedReviewCommentRow::GroupLabel(label));
            group_has_entries = true;
        }
        rows.push(GroupedReviewCommentRow::Entry(entry));
    }
}

#[cfg(test)]
#[path = "review_comment_test.rs"]
mod tests;

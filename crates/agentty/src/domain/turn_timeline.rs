//! Live timeline of one running turn: the model's narration and reasoning
//! and its tool calls in arrival order, grouped into display-neutral steps.
//!
//! [`TurnTimeline`] reduces a turn's streamed output and activity, stamped
//! with an injected clock. Each entry keeps only what its row needs, parsed
//! once when the entry changes, and joins a step as it arrives: a narration
//! message with the reasoning and tool rows that follow it, or rows alone
//! for backends that do not narrate. [`TurnTimeline::snapshot`] rebuilds
//! only the visible steps that changed since the previous snapshot.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ag_contracts::{
    ActivityEvent, ActivityFailure, ActivityKind, ActivityStatus, OutputEvent, OutputKind,
};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

/// Most tool rows one turn admits; later calls are not shown.
const TOOL_ROW_LIMIT: usize = 2048;
/// Most entries the timeline keeps before it evicts its oldest steps.
const ENTRY_LIMIT: usize = 4096;
/// Entries one eviction frees at least, so evictions stay rare.
const ENTRY_EVICTION_BATCH: usize = 1024;
/// Most characters one narration message keeps.
const NARRATION_CHAR_LIMIT: usize = 16 * 1024;
/// Most characters one reasoning item keeps.
const REASONING_CHAR_LIMIT: usize = 4 * 1024;
/// Most recent steps a snapshot holds; earlier ones are only counted.
const STEP_LIMIT: usize = 400;
/// Shortest finished call whose duration a row shows, in milliseconds.
const SLOW_CALL_MILLIS: u64 = 5_000;
/// Most failed-output lines a row keeps.
const FAILURE_TAIL_LINES: usize = 3;
/// Source of distinct timeline identities.
static NEXT_TIMELINE_ID: AtomicU64 = AtomicU64::new(0);

/// Reduced output and activity of one running turn, in arrival order.
#[derive(Debug)]
pub(crate) struct TurnTimeline {
    entries: Vec<Entry>,
    /// Evicted steps that held narration or reasoning, which the turn
    /// summary counts.
    evicted_narrated_steps: usize,
    /// Oldest steps evicted to bound memory, counted as hidden.
    evicted_steps: usize,
    /// Distinct identity of this timeline, which snapshots carry.
    id: u64,
    index: HashMap<EntryKey, usize>,
    /// Entries of reasoning still streaming, in arrival order.
    open_reasoning: BTreeMap<usize, Option<CurrentAction>>,
    /// Latest progress text the turn reported outside its output, such as
    /// `Waiting for <model>`, with the time it arrived.
    progress: Option<(String, u64)>,
    revision: u64,
    /// Entries of calls being prepared or still running, in arrival order.
    running_tools: BTreeMap<usize, CurrentAction>,
    steps: Vec<StepSlot>,
    /// Tool rows the turn admitted, bounded by [`TOOL_ROW_LIMIT`].
    tools_admitted: usize,
    /// Unix milliseconds of the first observed event.
    turn_started_at: Option<u64>,
}

impl Default for TurnTimeline {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            evicted_narrated_steps: 0,
            evicted_steps: 0,
            id: NEXT_TIMELINE_ID.fetch_add(1, Ordering::Relaxed),
            index: HashMap::new(),
            open_reasoning: BTreeMap::new(),
            progress: None,
            revision: 0,
            running_tools: BTreeMap::new(),
            steps: Vec::new(),
            tools_admitted: 0,
            turn_started_at: None,
        }
    }
}

impl TurnTimeline {
    /// Applies one streamed output change observed at `now` Unix
    /// milliseconds, returning whether the timeline changed.
    pub(crate) fn observe_output(&mut self, event: OutputEvent, now: u64) -> bool {
        self.start_turn(now);
        match event {
            OutputEvent::Started { id, kind } => self.start_item(id, kind, now),
            OutputEvent::Delta { id, text } => self.append_delta(id, &text),
            OutputEvent::Completed { id, text } => self.complete(id, &text, now),
            OutputEvent::Answer { replaces, text } => {
                self.answer(replaces, &text, now);

                true
            }
            OutputEvent::Discard { ids } => self.discard(&ids),
        }
    }

    /// Applies one activity snapshot observed at `now` Unix milliseconds,
    /// returning whether the timeline changed.
    ///
    /// A native call's activity shares the id of the streamed tool-call item
    /// that announced it, and replaces that item's row in place.
    pub(crate) fn observe_activity(&mut self, event: ActivityEvent, now: u64) -> bool {
        self.start_turn(now);
        let key = EntryKey::Activity(event.attempt_id.clone(), event.id.clone());
        let known = self.index.get(&key).copied();
        let announced = || {
            let index = *self.index.get(&EntryKey::Output(event.id.parse().ok()?))?;

            Some(index)
        };
        let target = known.or_else(announced);
        if let Some(index) = target
            && let EntryKind::Tool(tool) = &mut self.entries[index].kind
            && (known.is_some() || tool.activity.is_none())
        {
            if known.is_none() {
                tool.started_at = now;
            }
            tool.update(event, now);
            let running = tool.is_running().then(|| tool.current_action());
            self.index.insert(key, index);
            self.set_running(index, running);
            self.touch(index);

            return true;
        }
        if self.tools_admitted >= TOOL_ROW_LIMIT {
            return false;
        }
        let mut tool = ToolEntry::preparing(now);
        tool.update(event, now);
        self.push(key, EntryKind::Tool(tool), now);

        true
    }

    /// Records progress the turn reported outside its output. A repeat,
    /// such as the wait for a model's next response, restarts its timer.
    pub(crate) fn observe_progress(&mut self, text: String, now: u64) -> bool {
        self.start_turn(now);
        self.progress = Some((text, now));
        self.revision = self.revision.wrapping_add(1);

        true
    }

    /// Settles calls that never reported a result once the turn's events
    /// end, returning whether any changed.
    pub(crate) fn interrupt_running(&mut self, now: u64) -> bool {
        let running = std::mem::take(&mut self.running_tools);
        let mut changed = false;
        for index in running.into_keys() {
            if let EntryKind::Tool(tool) = &mut self.entries[index].kind
                && tool.interrupt(now)
            {
                changed = true;
                self.touch(index);
            }
        }

        changed
    }

    /// Returns whether the timeline holds anything to show.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the steps the turn narrated or reasoned in, evicted ones
    /// included, leaving out a step that only holds the answer, or `None`
    /// when it did neither, as backends without narration do.
    pub(crate) fn narrated_steps(&self) -> Option<usize> {
        let steps = self.evicted_narrated_steps
            + self
                .steps
                .iter()
                .filter(|step| self.is_narrated(step))
                .count();

        (steps > 0).then_some(steps)
    }

    /// Returns whether `step` holds narration other than the answer, or
    /// reasoning.
    fn is_narrated(&self, step: &StepSlot) -> bool {
        step.entries
            .iter()
            .any(|index| match &self.entries[*index].kind {
                EntryKind::Narration(narration) => !narration.is_answer,
                EntryKind::Reasoning(_) => true,
                EntryKind::Tool(_) => false,
            })
    }

    /// Returns the visible steps, rebuilding only those that changed since
    /// the previous snapshot.
    pub(crate) fn snapshot(&mut self) -> TimelineSnapshot {
        let turn_started_at = self.turn_started_at.unwrap_or_default();
        let visible_from = self.steps.len().saturating_sub(STEP_LIMIT);
        let entries = &self.entries;
        let steps = self.steps[visible_from..]
            .iter_mut()
            .map(|slot| {
                if let Some(step) = &slot.built {
                    return Arc::clone(step);
                }
                let step = Arc::new(slot.build(entries, turn_started_at));
                slot.built = Some(Arc::clone(&step));

                step
            })
            .collect();

        TimelineSnapshot {
            current: self.current_action(),
            hidden_steps: self.evicted_steps + visible_from,
            id: self.id,
            revision: self.revision,
            step_count: self.evicted_steps + self.steps.len(),
            steps,
        }
    }

    /// Picks what the status line names: the open reasoning's title, else
    /// the latest running call, else the latest progress text.
    pub(crate) fn current_action(&self) -> Option<CurrentAction> {
        let reasoning = self.open_reasoning.values().rev().find_map(Clone::clone);
        let running = || self.running_tools.values().next_back().cloned();
        let progress = || {
            self.progress
                .as_ref()
                .map(|(text, since)| CurrentAction::new(text, None, *since))
        };

        reasoning.or_else(running).or_else(progress)
    }

    /// Records the first event's time as the turn start.
    fn start_turn(&mut self, now: u64) {
        self.turn_started_at.get_or_insert(now);
    }

    /// Opens output item `id`. A tool call past the row limit is ignored,
    /// and so is its later activity.
    fn start_item(&mut self, id: u64, kind: OutputKind, now: u64) -> bool {
        let entry_kind = match kind {
            OutputKind::Message => EntryKind::Narration(NarrationEntry::default()),
            OutputKind::Reasoning => EntryKind::Reasoning(ReasoningEntry::default()),
            OutputKind::ToolCall if self.tools_admitted >= TOOL_ROW_LIMIT => return false,
            OutputKind::ToolCall => EntryKind::Tool(ToolEntry::preparing(now)),
        };
        self.push(EntryKey::Output(id), entry_kind, now);

        true
    }

    /// Appends `kind` under `key` to the latest step it fits, or a new one,
    /// and evicts the oldest steps once the timeline holds too many
    /// entries.
    fn push(&mut self, key: EntryKey, kind: EntryKind, now: u64) {
        if matches!(kind, EntryKind::Tool(_)) {
            self.tools_admitted += 1;
        }
        let index = self.entries.len();
        self.index.insert(key, index);
        self.entries.push(Entry {
            at: now,
            kind,
            revision: 0,
            step: 0,
        });
        self.place(index);
        self.touch(index);
        if self.entries.len() > ENTRY_LIMIT {
            self.evict_oldest_steps();
        }
    }

    /// Adds entry `index` to the latest step it fits, or starts a step,
    /// and tracks it while it streams or runs.
    fn place(&mut self, index: usize) {
        let entry = &self.entries[index];
        let fits = self.steps.last().is_some_and(|step| match &entry.kind {
            EntryKind::Narration(_) => !step.has_narration && !step.has_tools,
            EntryKind::Reasoning(_) => !step.has_thought && !step.has_tools,
            EntryKind::Tool(_) => true,
        });
        if !fits {
            self.steps.push(StepSlot::default());
        }
        let step_index = self.steps.len() - 1;
        let step = &mut self.steps[step_index];
        step.entries.push(index);
        match &entry.kind {
            EntryKind::Narration(_) => step.has_narration = true,
            EntryKind::Reasoning(reasoning) => {
                step.has_thought = true;
                let open = reasoning.finished_at.is_none();
                let title = reasoning.title_action(entry.at);
                self.set_open_reasoning(index, open, title);
            }
            EntryKind::Tool(tool) => {
                step.has_tools = true;
                let running = tool.is_running().then(|| tool.current_action());
                self.set_running(index, running);
            }
        }
        self.entries[index].step = step_index;
    }

    /// Tracks the reasoning at `index` while it is `open`, with the action
    /// its title names, if any.
    fn set_open_reasoning(&mut self, index: usize, open: bool, title: Option<CurrentAction>) {
        if open {
            self.open_reasoning.insert(index, title);
        } else {
            self.open_reasoning.remove(&index);
        }
    }

    /// Marks entry `index` changed, so its step is rebuilt.
    fn touch(&mut self, index: usize) {
        self.revision = self.revision.wrapping_add(1);
        let entry = &mut self.entries[index];
        entry.revision = self.revision;
        self.steps[entry.step].built = None;
    }

    /// Tracks the call at `index` with its action while it still runs.
    fn set_running(&mut self, index: usize, action: Option<CurrentAction>) {
        match action {
            Some(action) => self.running_tools.insert(index, action),
            None => self.running_tools.remove(&index),
        };
    }

    /// Appends a fragment to message or reasoning item `id`, returning
    /// whether any text was added.
    fn append_delta(&mut self, id: u64, text: &str) -> bool {
        let Some(index) = self.output_index(id) else {
            return false;
        };
        let entry = &mut self.entries[index];
        let mut title = None;
        let appended = match &mut entry.kind {
            EntryKind::Narration(narration) if narration.open => {
                narration.text.push(text, NARRATION_CHAR_LIMIT)
            }
            EntryKind::Reasoning(reasoning) if reasoning.finished_at.is_none() => {
                let appended = reasoning.text.push(text, REASONING_CHAR_LIMIT);
                reasoning.title = reasoning_title(&reasoning.text.text);
                title = appended.then(|| reasoning.title_action(entry.at));

                appended
            }
            _ => false,
        };
        if let Some(title) = title {
            self.open_reasoning.insert(index, title);
        }
        if appended {
            self.touch(index);
        }

        appended
    }

    /// Replaces item `id`'s fragments with its completed text. A tool call
    /// keeps its preparing row until its activity replaces it.
    fn complete(&mut self, id: u64, text: &str, now: u64) -> bool {
        let Some(index) = self.output_index(id) else {
            return false;
        };
        match &mut self.entries[index].kind {
            EntryKind::Narration(narration) => {
                narration.open = false;
                narration.text = BoundedText::new(text, NARRATION_CHAR_LIMIT);
            }
            EntryKind::Reasoning(reasoning) => {
                reasoning.finished_at = Some(now);
                reasoning.text = BoundedText::new(text, REASONING_CHAR_LIMIT);
                reasoning.title = reasoning_title(&reasoning.text.text);
                self.open_reasoning.remove(&index);
            }
            EntryKind::Tool(_) => return false,
        }
        self.touch(index);

        true
    }

    /// Shows the validated answer in place of the message it streamed as,
    /// or as a new message when it did not stream.
    fn answer(&mut self, replaces: Option<u64>, text: &str, now: u64) {
        let answer = NarrationEntry {
            is_answer: true,
            open: false,
            text: BoundedText::new(text, NARRATION_CHAR_LIMIT),
        };
        if let Some(index) = replaces.and_then(|id| self.output_index(id))
            && let EntryKind::Narration(narration) = &mut self.entries[index].kind
        {
            *narration = answer;
            self.touch(index);

            return;
        }
        self.push(
            EntryKey::Answer(now, self.revision),
            EntryKind::Narration(answer),
            now,
        );
    }

    /// Drops the items of a discarded response together with the tool rows
    /// that share their ids.
    fn discard(&mut self, ids: &[u64]) -> bool {
        let mut void = vec![false; self.entries.len()];
        for (key, &index) in &self.index {
            void[index] = match key {
                EntryKey::Output(id) => ids.contains(id),
                EntryKey::Activity(_, id) => id.parse().is_ok_and(|id| ids.contains(&id)),
                EntryKey::Answer(..) => false,
            };
        }
        if !void.contains(&true) {
            return false;
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if void[index] && matches!(entry.kind, EntryKind::Tool(_)) {
                self.tools_admitted = self.tools_admitted.saturating_sub(1);
            }
        }
        self.retain_entries(|index| !void[index]);

        true
    }

    /// Evicts the oldest steps until the timeline is a batch below its
    /// entry limit, counting them as hidden.
    fn evict_oldest_steps(&mut self) {
        let mut evicted_entries = 0;
        let mut evicted_steps = 0;
        let mut evicted_narrated_steps = 0;
        for step in &self.steps {
            if self.entries.len() - evicted_entries <= ENTRY_LIMIT - ENTRY_EVICTION_BATCH {
                break;
            }
            evicted_entries += step.entries.len();
            evicted_steps += 1;
            evicted_narrated_steps += usize::from(self.is_narrated(step));
        }
        self.evicted_steps += evicted_steps;
        self.evicted_narrated_steps += evicted_narrated_steps;
        let first_kept = self
            .steps
            .get(evicted_steps)
            .map_or(self.entries.len(), |step| step.entries[0]);
        self.retain_entries(|index| index >= first_kept);
    }

    /// Keeps the entries `keep` accepts by index and groups them again.
    fn retain_entries(&mut self, keep: impl Fn(usize) -> bool) {
        let mut remapped = vec![None; self.entries.len()];
        let mut kept = 0;
        for (index, slot) in remapped.iter_mut().enumerate() {
            if keep(index) {
                *slot = Some(kept);
                kept += 1;
            }
        }
        let mut index = 0;
        self.entries.retain(|_| {
            let keep = remapped[index].is_some();
            index += 1;

            keep
        });
        self.index.retain(|_, index| match remapped[*index] {
            Some(new_index) => {
                *index = new_index;

                true
            }
            None => false,
        });
        self.steps.clear();
        self.open_reasoning.clear();
        self.running_tools.clear();
        for index in 0..self.entries.len() {
            self.place(index);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Returns the entry index of output item `id`.
    fn output_index(&self, id: u64) -> Option<usize> {
        self.index.get(&EntryKey::Output(id)).copied()
    }
}

/// Render-ready grouping of a turn's timeline. Two snapshots are equal when
/// they come from the same timeline at the same revision, so comparing and
/// hashing one costs nothing.
#[derive(Clone, Debug)]
pub(crate) struct TimelineSnapshot {
    /// What the turn is doing now, for the status line.
    pub(crate) current: Option<CurrentAction>,
    /// Earliest steps left out of [`Self::steps`].
    pub(crate) hidden_steps: usize,
    /// Steps the turn took so far, including hidden ones.
    pub(crate) step_count: usize,
    /// Most recent steps in order.
    pub(crate) steps: Vec<Arc<TimelineStep>>,
    /// Identity of the timeline the snapshot came from.
    id: u64,
    /// Changes whenever the timeline changes.
    revision: u64,
}

impl TimelineSnapshot {
    /// Returns the identity of the timeline the snapshot came from, which
    /// differs between turns while revisions restart at zero.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl PartialEq for TimelineSnapshot {
    fn eq(&self, other: &Self) -> bool {
        (self.id, self.revision) == (other.id, other.revision)
    }
}

impl Eq for TimelineSnapshot {}

impl Hash for TimelineSnapshot {
    fn hash<State: Hasher>(&self, state: &mut State) {
        (self.id, self.revision).hash(state);
    }
}

/// What the turn is doing now, such as `Running cargo test`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CurrentAction {
    /// Columns of `text` that name the action, such as `Running`; the rest
    /// names its object.
    pub(crate) label_width: usize,
    /// Unix milliseconds when the action started.
    pub(crate) since: u64,
    /// The action on one line, which the status line and the session's
    /// progress text both show.
    pub(crate) text: String,
}

impl CurrentAction {
    /// Joins `label` and its optional `detail` into one line.
    fn new(label: &str, detail: Option<&str>, since: u64) -> Self {
        let text = match detail {
            Some(detail) => format!("{label} {detail}"),
            None => label.to_string(),
        };

        Self {
            label_width: label.width(),
            since,
            text,
        }
    }
}

/// One step: a narration message with the reasoning and calls that follow
/// it, or calls alone when the backend does not narrate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TimelineStep {
    /// Seconds from the turn's start to the step's first event.
    pub(crate) at_seconds: u64,
    /// Changes whenever the narration or thought changes.
    pub(crate) header_revision: u64,
    pub(crate) narration: Option<Narration>,
    pub(crate) rows: Vec<TimelineRow>,
    pub(crate) thought: Option<Thought>,
}

/// One message the model wrote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Narration {
    /// Whether the message is the turn's validated answer, rendered as
    /// Markdown.
    pub(crate) is_answer: bool,
    /// Whether fragments may still arrive.
    pub(crate) open: bool,
    pub(crate) text: Arc<str>,
}

/// One reasoning item, shown as a single dim line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Thought {
    /// Seconds the reasoning took, or `None` while it runs.
    pub(crate) seconds: Option<u64>,
    /// Reasoning text without its title, on one line.
    pub(crate) summary: String,
}

impl Thought {
    /// Summarizes reasoning that started at `started_at`.
    fn new(started_at: u64, reasoning: &ReasoningEntry) -> Self {
        let text = &reasoning.text.text;
        let body = text
            .lines()
            .filter(|line| reasoning.title.is_none() || !is_title_line(line))
            .collect::<Vec<_>>()
            .join(" ")
            .replace("**", "");
        let summary = body.split_whitespace().collect::<Vec<_>>().join(" ");

        Self {
            seconds: reasoning
                .finished_at
                .map(|finished_at| finished_at.saturating_sub(started_at) / 1000),
            summary: if summary.is_empty() {
                reasoning.title.clone().unwrap_or_default()
            } else {
                summary
            },
        }
    }
}

/// One row under a step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TimelineRow {
    /// Consecutive successful reads, searches, and listings.
    Explored(ExploreRow),
    /// One call.
    Tool(ToolRow),
}

impl TimelineRow {
    /// Returns the revision that changes whenever the row's content does.
    pub(crate) fn revision(&self) -> u64 {
        match self {
            Self::Explored(row) => row.revision,
            Self::Tool(row) => row.revision,
        }
    }

    /// Returns the Unix milliseconds the row's call started running, while
    /// it runs.
    pub(crate) fn running_since(&self) -> Option<u64> {
        match self {
            Self::Explored(row) => row.running_since,
            Self::Tool(row) => row.running_since,
        }
    }
}

/// Consecutive successful reads, searches, and listings folded into one
/// row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExploreRow {
    /// Calls the row folds.
    pub(crate) calls: usize,
    /// What the calls touched, grouped by kind in first-seen order.
    pub(crate) parts: Vec<ExplorePart>,
    /// Changes whenever any folded call changes.
    pub(crate) revision: u64,
    /// Unix milliseconds the earliest still-running folded call started,
    /// while any of them runs.
    pub(crate) running_since: Option<u64>,
}

/// What one kind of exploration touched, such as the files read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExplorePart {
    /// Distinct targets in first-seen order: file names, quoted queries, or
    /// listed paths.
    pub(crate) items: Vec<String>,
    /// `read`, `search`, or `list`.
    pub(crate) label: &'static str,
}

/// One call's row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolRow {
    /// Seconds a slow finished call took.
    pub(crate) duration_seconds: Option<u64>,
    /// Line range of the read of the same file that preceded an edit, such
    /// as `:10-42`, or empty for a whole-file read.
    pub(crate) read_first: Option<String>,
    pub(crate) result: RowResult,
    /// Changes whenever the row's content does.
    pub(crate) revision: u64,
    /// Unix milliseconds the call started, while it runs.
    pub(crate) running_since: Option<u64>,
    pub(crate) state: RowState,
    /// Last lines of a failed call's output.
    pub(crate) tail: Vec<String>,
    /// What the call acted on, on one line.
    pub(crate) target: String,
    /// Whether the target is a literal command rather than a path or name.
    pub(crate) target_is_command: bool,
    /// The call's verb, such as `Ran` or `Edit`.
    pub(crate) verb: &'static str,
}

/// Lifecycle of one row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RowState {
    Failed,
    Interrupted,
    Running,
    Succeeded,
}

/// The outcome a row reports on its right.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RowResult {
    /// Lines a file change added and removed.
    DiffStat { added: u64, removed: u64 },
    /// A command's own exit status.
    Exit(i64),
    /// A failure other than a command's exit, such as `rejected`, with the
    /// first line of why when the provider reported it.
    Failed {
        label: &'static str,
        reason: Option<String>,
    },
    /// The call ended without a result.
    Interrupted,
    /// Nothing to report.
    None,
    /// A command ended by a signal.
    Signal(i64),
}

/// Identity of one entry: a streamed output item, an activity, or an
/// answer that did not stream, keyed by its arrival.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum EntryKey {
    Activity(String, String),
    Answer(u64, u64),
    Output(u64),
}

/// One timeline entry with its arrival time and step.
#[derive(Debug)]
struct Entry {
    /// Unix milliseconds of arrival.
    at: u64,
    kind: EntryKind,
    /// Timeline revision of the entry's latest change.
    revision: u64,
    /// Index of the step the entry belongs to.
    step: usize,
}

#[derive(Debug)]
enum EntryKind {
    Narration(NarrationEntry),
    Reasoning(ReasoningEntry),
    Tool(ToolEntry),
}

impl EntryKind {
    /// Returns the call this entry holds, if it is one.
    fn tool(&self) -> Option<&ToolEntry> {
        match self {
            Self::Tool(tool) => Some(tool),
            Self::Narration(_) | Self::Reasoning(_) => None,
        }
    }
}

/// Text cut to a character limit, with its character count kept beside it.
#[derive(Debug, Default)]
struct BoundedText {
    chars: usize,
    text: String,
}

impl BoundedText {
    /// Returns `text` cut to `limit` characters.
    fn new(text: &str, limit: usize) -> Self {
        let mut bounded = Self::default();
        bounded.push(text, limit);

        bounded
    }

    /// Appends `text` while fewer than `limit` characters are kept,
    /// returning whether anything was appended.
    fn push(&mut self, text: &str, limit: usize) -> bool {
        let before = self.chars;
        for character in text.chars().take(limit.saturating_sub(self.chars)) {
            self.text.push(character);
            self.chars += 1;
        }

        self.chars != before
    }
}

/// One message the model wrote, or the validated answer.
#[derive(Debug)]
struct NarrationEntry {
    is_answer: bool,
    open: bool,
    text: BoundedText,
}

impl Default for NarrationEntry {
    fn default() -> Self {
        Self {
            is_answer: false,
            open: true,
            text: BoundedText::default(),
        }
    }
}

/// One reasoning item with its title, kept current as it streams.
#[derive(Debug, Default)]
struct ReasoningEntry {
    finished_at: Option<u64>,
    text: BoundedText,
    title: Option<String>,
}

impl ReasoningEntry {
    /// Returns the action the title names, for reasoning that started at
    /// `since`.
    fn title_action(&self, since: u64) -> Option<CurrentAction> {
        self.title
            .as_ref()
            .map(|title| CurrentAction::new(title, None, since))
    }
}

/// One call, announced by a streamed tool-call item or its first activity.
#[derive(Debug)]
struct ToolEntry {
    /// What the row needs from the latest activity, or `None` while the
    /// model writes the arguments.
    activity: Option<Box<ToolActivity>>,
    /// Unix milliseconds of the first terminal activity.
    finished_at: Option<u64>,
    /// Unix milliseconds when the call started running.
    started_at: u64,
}

/// The parts of one activity snapshot a row and its folding need, derived
/// once when the activity arrives.
#[derive(Debug)]
struct ToolActivity {
    /// Present-tense verb the status line names the running call with.
    action: &'static str,
    /// What an exploration touched, for its folded row.
    explore: Option<(&'static str, String)>,
    /// Reported path, compared when a read precedes an edit.
    path: Option<String>,
    /// Line range of a successful read, such as `:10-42`, or empty for the
    /// whole file.
    read_range: Option<String>,
    row: ToolRow,
}

impl ToolEntry {
    /// Returns a call whose arguments the model is still writing.
    fn preparing(now: u64) -> Self {
        Self {
            activity: None,
            finished_at: None,
            started_at: now,
        }
    }

    /// Derives the row from `event`, recording when it first finished.
    fn update(&mut self, event: ActivityEvent, now: u64) {
        if event.status != ActivityStatus::Running {
            self.finished_at.get_or_insert(now);
        }
        self.activity = Some(Box::new(ToolActivity::new(
            event,
            self.started_at,
            self.finished_at,
        )));
    }

    /// Settles a running call as interrupted, returning whether it ran.
    fn interrupt(&mut self, now: u64) -> bool {
        let Some(activity) = self
            .activity
            .as_mut()
            .filter(|activity| activity.row.state == RowState::Running)
        else {
            return false;
        };
        self.finished_at = Some(now);
        activity.row.state = RowState::Interrupted;
        activity.row.result = RowResult::Interrupted;
        activity.row.running_since = None;
        activity.explore = None;

        true
    }

    /// Returns whether the call is being prepared or still runs.
    fn is_running(&self) -> bool {
        self.activity
            .as_ref()
            .is_none_or(|activity| activity.row.state == RowState::Running)
    }

    /// Describes the running call for the status line.
    fn current_action(&self) -> CurrentAction {
        let Some(activity) = &self.activity else {
            return CurrentAction::new("Preparing", Some("a tool call"), self.started_at);
        };
        let target = &activity.row.target;

        CurrentAction::new(
            activity.action,
            (!target.is_empty()).then_some(target.as_str()),
            self.started_at,
        )
    }

    /// Returns the call's row at `revision`.
    fn row(&self, revision: u64) -> ToolRow {
        let Some(activity) = &self.activity else {
            return ToolRow {
                duration_seconds: None,
                read_first: None,
                result: RowResult::None,
                revision,
                running_since: Some(self.started_at),
                state: RowState::Running,
                tail: Vec::new(),
                target: "a tool call".to_string(),
                target_is_command: false,
                verb: "Preparing",
            };
        };
        let mut row = activity.row.clone();
        row.revision = revision;

        row
    }
}

impl ToolActivity {
    /// Derives what a row needs from `event` for a call that started at
    /// `started_at` and finished at `finished_at`.
    fn new(event: ActivityEvent, started_at: u64, finished_at: Option<u64>) -> Self {
        let state = match event.outcome() {
            ActivityStatus::Completed => RowState::Succeeded,
            ActivityStatus::Failed => RowState::Failed,
            ActivityStatus::Interrupted => RowState::Interrupted,
            ActivityStatus::Running => RowState::Running,
        };
        let target = Self::target(&event);
        let exploring =
            event.kind.is_exploration() && matches!(state, RowState::Running | RowState::Succeeded);
        let explore = exploring.then(|| match event.kind {
            ActivityKind::Read => ("read", file_name(&target).to_string()),
            ActivityKind::Search => ("search", format!("\"{target}\"")),
            _ => ("list", target.clone()),
        });
        let read_range = (event.kind == ActivityKind::Read
            && event.status == ActivityStatus::Completed)
            .then(|| {
                event
                    .facts
                    .lines
                    .map_or_else(String::new, |lines| match lines.last {
                        Some(last) => format!(":{}-{last}", lines.first),
                        None => format!(":{}-", lines.first),
                    })
            });
        let row = ToolRow {
            duration_seconds: Self::slow_duration(&event, started_at, finished_at),
            read_first: None,
            result: Self::result(&event, state),
            revision: 0,
            running_since: (state == RowState::Running).then_some(started_at),
            state,
            tail: if state == RowState::Failed {
                Self::failure_tail(&event)
            } else {
                Vec::new()
            },
            target,
            target_is_command: event.kind == ActivityKind::Command,
            verb: Self::verb(event.kind, state),
        };

        Self {
            action: Self::action(event.kind),
            explore,
            path: event.facts.target,
            read_range,
            row,
        }
    }

    /// Returns the present-tense verb for a running call of `kind`.
    fn action(kind: ActivityKind) -> &'static str {
        match kind {
            ActivityKind::Command => "Running",
            ActivityKind::FileChange => "Editing",
            ActivityKind::Read => "Reading",
            ActivityKind::Search | ActivityKind::WebSearch => "Searching",
            ActivityKind::List => "Listing",
            ActivityKind::Mcp | ActivityKind::Skill | ActivityKind::Tool => "Calling",
        }
    }

    /// Returns the seconds a slow finished call took: the duration the
    /// provider reported, else the time between its first and last event.
    fn slow_duration(
        event: &ActivityEvent,
        started_at: u64,
        finished_at: Option<u64>,
    ) -> Option<u64> {
        let finished_at = finished_at?;
        let millis = event.facts.duration.map_or_else(
            || finished_at.saturating_sub(started_at),
            |duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        );

        (millis >= SLOW_CALL_MILLIS).then_some(millis / 1000)
    }

    /// Returns the outcome a row reports for `event` in `state`.
    fn result(event: &ActivityEvent, state: RowState) -> RowResult {
        match state {
            RowState::Running => RowResult::None,
            RowState::Interrupted => RowResult::Interrupted,
            RowState::Failed => {
                if let Some(signal) = event.facts.signal {
                    RowResult::Signal(signal)
                } else if let Some(code) = event.effective_exit_code().filter(|code| *code != 0) {
                    RowResult::Exit(code)
                } else {
                    let label = match event.facts.failure {
                        Some(ActivityFailure::Denied) => "denied",
                        Some(ActivityFailure::Rejected) => "rejected",
                        Some(ActivityFailure::TimedOut) => "timed out",
                        Some(ActivityFailure::Unavailable) => "unavailable",
                        Some(ActivityFailure::Error) | None => "failed",
                    };

                    RowResult::Failed {
                        label,
                        reason: event.facts.reason.clone(),
                    }
                }
            }
            RowState::Succeeded => {
                if let Some(stat) = event.facts.diff_stat {
                    RowResult::DiffStat {
                        added: stat.added,
                        removed: stat.removed,
                    }
                } else if event.kind == ActivityKind::Command
                    && let Some(code) = event.effective_exit_code()
                {
                    RowResult::Exit(code)
                } else {
                    RowResult::None
                }
            }
        }
    }

    /// Returns the last lines of a failed call's output.
    fn failure_tail(event: &ActivityEvent) -> Vec<String> {
        let Some(output) = &event.output else {
            return Vec::new();
        };
        let mut lines = output
            .lines()
            .rev()
            .map(single_line)
            .filter(|line| !line.is_empty())
            .take(FAILURE_TAIL_LINES)
            .collect::<Vec<_>>();
        lines.reverse();

        lines
    }

    /// Returns what `event` acted on: a command literally, else the
    /// reported target or a field of its input, prefixed by the tool for
    /// MCP and other tools.
    fn target(event: &ActivityEvent) -> String {
        let input = || {
            event
                .input
                .as_deref()
                .and_then(|input| input_preview(input, event.kind))
                .filter(|input| !input.is_empty())
        };
        let reported = || event.facts.target.as_deref().map(single_line);
        match event.kind {
            ActivityKind::Command => input().or_else(reported).unwrap_or_default(),
            ActivityKind::Skill => single_line(&event.name),
            ActivityKind::Mcp | ActivityKind::Tool => {
                let name = single_line(&event.name);
                match reported().or_else(input) {
                    Some(detail) => format!("{name} · {detail}"),
                    None => name,
                }
            }
            ActivityKind::FileChange
            | ActivityKind::List
            | ActivityKind::Read
            | ActivityKind::Search
            | ActivityKind::WebSearch => reported().or_else(input).unwrap_or_default(),
        }
    }

    /// Returns the verb a row shows for `kind` in `state`.
    fn verb(kind: ActivityKind, state: RowState) -> &'static str {
        let running = state == RowState::Running;
        match kind {
            ActivityKind::Command if running => "Running",
            ActivityKind::Command => "Ran",
            ActivityKind::FileChange => "Edit",
            ActivityKind::Read => "Read",
            ActivityKind::Search => "Search",
            ActivityKind::List => "List",
            ActivityKind::Mcp => "MCP",
            ActivityKind::Skill => "Skill",
            ActivityKind::Tool if running => "Calling",
            ActivityKind::Tool => "Called",
            ActivityKind::WebSearch if running => "Searching",
            ActivityKind::WebSearch => "Searched",
        }
    }
}

/// Entries of one step, with the step built from them until one changes.
#[derive(Debug, Default)]
struct StepSlot {
    built: Option<Arc<TimelineStep>>,
    entries: Vec<usize>,
    has_narration: bool,
    has_thought: bool,
    has_tools: bool,
}

impl StepSlot {
    /// Builds the step from `entries`, folding reads into the edits that
    /// follow them and consecutive successful explorations into one row.
    fn build(&self, entries: &[Entry], turn_started_at: u64) -> TimelineStep {
        let mut step = TimelineStep {
            at_seconds: entries[self.entries[0]].at.saturating_sub(turn_started_at) / 1000,
            header_revision: 0,
            narration: None,
            rows: Vec::new(),
            thought: None,
        };
        for entry in self.entries.iter().map(|&index| &entries[index]) {
            match &entry.kind {
                EntryKind::Narration(narration) => {
                    step.header_revision = step.header_revision.max(entry.revision);
                    step.narration = Some(Narration {
                        is_answer: narration.is_answer,
                        open: narration.open,
                        text: Arc::from(narration.text.text.as_str()),
                    });
                }
                EntryKind::Reasoning(reasoning) => {
                    step.header_revision = step.header_revision.max(entry.revision);
                    step.thought = Some(Thought::new(entry.at, reasoning));
                }
                EntryKind::Tool(_) => {}
            }
        }
        let mut explore: Option<ExploreRow> = None;
        let mut tools = self
            .entries
            .iter()
            .filter_map(|&index| {
                let entry = &entries[index];

                entry.kind.tool().map(|tool| (entry.revision, tool))
            })
            .peekable();
        while let Some((revision, tool)) = tools.next() {
            let Some((activity, (label, item))) = tool
                .activity
                .as_ref()
                .and_then(|activity| Some((activity, activity.explore.clone()?)))
            else {
                step.rows.extend(explore.take().map(TimelineRow::Explored));
                step.rows.push(TimelineRow::Tool(tool.row(revision)));
                continue;
            };
            if let Some(read_first) =
                Self::read_before_edit(activity, tools.peek().map(|(_, edit)| *edit))
                && let Some((edit_revision, edit)) = tools.next()
            {
                step.rows.extend(explore.take().map(TimelineRow::Explored));
                let mut row = edit.row(edit_revision.max(revision));
                row.read_first = Some(read_first);
                step.rows.push(TimelineRow::Tool(row));
                continue;
            }
            let group = explore.get_or_insert_with(|| ExploreRow {
                calls: 0,
                parts: Vec::new(),
                revision: 0,
                running_since: None,
            });
            group.calls += 1;
            group.revision = group.revision.max(revision);
            group.running_since = match (group.running_since, activity.row.running_since) {
                (Some(earlier), Some(later)) => Some(earlier.min(later)),
                (folded, current) => folded.or(current),
            };
            Self::add_explore_item(group, label, item);
        }
        step.rows.extend(explore.map(TimelineRow::Explored));

        step
    }

    /// Returns the read range when successful read `read` of a file is
    /// directly followed by an edit of the same file.
    fn read_before_edit(read: &ToolActivity, next: Option<&ToolEntry>) -> Option<String> {
        let range = read.read_range.clone()?;
        let edit = next?.activity.as_ref()?;
        let read_path = read.path.as_deref()?;
        let edit_path = edit.path.as_deref()?;

        (edit.row.verb == "Edit" && same_path(read_path, edit_path)).then_some(range)
    }

    /// Adds what one exploration touched to `group`.
    fn add_explore_item(group: &mut ExploreRow, label: &'static str, item: String) {
        if item.is_empty() || item == "\"\"" {
            return;
        }
        match group.parts.iter_mut().find(|part| part.label == label) {
            Some(part) if part.items.contains(&item) => {}
            Some(part) => part.items.push(item),
            None => group.parts.push(ExplorePart {
                items: vec![item],
                label,
            }),
        }
    }
}

/// Returns what call `event` acted on, as its timeline row names it: a
/// command literally, else the reported target or a field of its input,
/// prefixed by the tool for MCP and other tools.
pub(crate) fn call_target(event: &ActivityEvent) -> String {
    ToolActivity::target(event)
}

/// Returns the title of reasoning text: its first line written as
/// `**Title**`.
fn reasoning_title(text: &str) -> Option<String> {
    let line = text.lines().find(|line| is_title_line(line))?;
    let title = line.trim().trim_matches('*').trim();

    (!title.is_empty()).then(|| title.to_string())
}

/// Returns whether `line` is a bold `**Title**` line.
fn is_title_line(line: &str) -> bool {
    let line = line.trim();

    line.len() > 4 && line.starts_with("**") && line.ends_with("**")
}

/// Keeps command text literal while extracting useful fields from tool
/// arguments without exposing edit payloads or raw JSON.
fn input_preview(input: &str, kind: ActivityKind) -> Option<String> {
    if kind == ActivityKind::Command {
        return Some(single_line(input));
    }
    let preview = match serde_json::from_str::<Value>(input) {
        Ok(Value::Object(arguments)) => [
            "command",
            "CommandLine",
            "file_path",
            "path",
            "TargetFile",
            "target_file",
            "query",
            "pattern",
        ]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))?
        .to_string(),
        Ok(Value::String(text)) => text,
        Ok(Value::Array(_)) => return None,
        Err(_) if input.trim_start().starts_with('{') => return None,
        _ => input.to_string(),
    };

    Some(single_line(&preview))
}

/// Keeps multiline text on one row without control characters.
pub(crate) fn single_line(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|character| !character.is_control())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns whether two reported paths name the same file, allowing one to
/// be relative to the other.
pub(crate) fn same_path(left: &str, right: &str) -> bool {
    let left = left.trim_start_matches("./");
    let right = right.trim_start_matches("./");

    left == right || left.ends_with(&format!("/{right}")) || right.ends_with(&format!("/{left}"))
}

/// Returns the last component of `path`.
pub(crate) fn file_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

#[cfg(test)]
#[path = "turn_timeline_test.rs"]
mod tests;

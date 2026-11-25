//! Summary of one finished turn: what it edited, which calls failed and how
//! the turn recovered, and how much it explored, persisted as the turn's
//! activity summary.
//!
//! [`TurnSummary`] is built without a model call from the calls a turn
//! made, and from its timeline's steps when the backend narrates. Its
//! persisted text is plain and tab-separated. Its first line is the format
//! marker [`TurnSummary::FORMAT_MARKER`], which renderers key on and leave
//! out, and its last line starts with [`TurnSummary::DIVIDER_PREFIX`]:
//!
//! ```text
//! turn-summary v1
//! 14 steps · 51 tools · 20m 23s
//! Edited\t✓\tsrc/lib.rs\t+79 −14\t7 edits
//! Failed\t✓\tcargo test 2>&1 | tail -20\texit 101\t→ rerun exit 0
//! \t✓\tedit to lib.rs rejected (hunk context mismatch)\t\t→ retried after re-reading
//! Explored\t25 reads · 7 searches · ran 5 commands
//! Used\tgithub/search ×2 · review ×1
//! ─ Worked 20m 23s · 14 steps · 51 tools · 2 failed (1 rejected edit, 1 nonzero exit), all recovered
//! ```
//!
//! A row of `heading`, `glyph`, `target`, `result`, and `note` columns
//! leaves `heading` empty below the first row of its section; a recovered
//! failure's note starts with [`TurnSummary::RECOVERY_PREFIX`]. A change to
//! the rows or columns bumps the marker's version. Earlier summaries listed
//! tool, MCP, and skill counts on `Tools:`, `MCP:`, and `Skills:` lines
//! instead.
//!
//! An edit's row is keyed on the path its activity reports. Codex reports
//! only the first path of a patch that changes several files, with the
//! line counts of the whole patch, so such a patch is listed under its
//! first file.

use std::fmt::Write as _;

use ag_contracts::{ActivityEvent, ActivityFailure, ActivityKind, ActivityStatus};

use crate::domain::turn_timeline::{call_target, file_name, same_path, single_line};

/// Pipeline stages a command's output is trimmed with, which do not change
/// what the command ran.
const OUTPUT_FILTERS: [&str; 2] = ["head", "tail"];

/// What one finished turn did, built from its calls in order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TurnSummary {
    /// Commands the turn ran, failed ones included.
    commands: usize,
    /// Files changed by successful edits, in first-edit order.
    edited: Vec<EditedFile>,
    /// Seconds the turn took, when its start is known.
    elapsed_seconds: Option<u64>,
    /// Reads, searches, listings, and web searches.
    explored: ExploreCounts,
    /// Failed calls grouped by what failed and why, in first-failure order.
    failed: Vec<FailureGroup>,
    /// Calls that never reported a result.
    interrupted: usize,
    /// Narrated steps, or `None` when the backend does not narrate.
    steps: Option<usize>,
    /// Calls the turn made.
    tools: usize,
    /// MCP, skill, and other tool names with their call counts, in
    /// first-call order.
    used: Vec<(String, usize)>,
}

impl TurnSummary {
    /// Start of the last line of a persisted summary.
    pub(crate) const DIVIDER_PREFIX: &str = "─ Worked";
    /// Heading of the files the turn edited.
    pub(crate) const EDITED_HEADING: &str = "Edited";
    /// Heading of the explorations and commands the turn ran.
    pub(crate) const EXPLORED_HEADING: &str = "Explored";
    /// Heading of the calls that failed.
    pub(crate) const FAILED_HEADING: &str = "Failed";
    /// Glyph of a failure the turn did not recover from.
    pub(crate) const FAILURE_GLYPH: &str = "✗";
    /// First line of a persisted summary, naming its format and version.
    pub(crate) const FORMAT_MARKER: &str = "turn-summary v1";
    /// Heading of the commands a turn that explored nothing ran.
    pub(crate) const RAN_HEADING: &str = "Ran";
    /// Start of the note of a recovered failure.
    pub(crate) const RECOVERY_PREFIX: &str = "→ ";
    /// Glyph of a successful edit or a recovered failure.
    pub(crate) const SUCCESS_GLYPH: &str = "✓";
    /// Heading of the MCP, skill, and other tool calls.
    pub(crate) const USED_HEADING: &str = "Used";

    /// Summarizes `calls` in the order they started.
    ///
    /// `steps` counts the turn's narrated steps, or is `None` for a backend
    /// that does not narrate; `elapsed_seconds` is `None` when the turn's
    /// start is unknown.
    pub(crate) fn from_calls(
        calls: &[&ActivityEvent],
        steps: Option<usize>,
        elapsed_seconds: Option<u64>,
    ) -> Self {
        let mut summary = Self {
            elapsed_seconds,
            steps,
            tools: calls.len(),
            ..Self::default()
        };
        let mut failures = Vec::new();
        for (index, event) in calls.iter().enumerate() {
            summary.count_kind(event);
            match event.outcome() {
                ActivityStatus::Completed if event.kind == ActivityKind::FileChange => {
                    summary.add_edit(event);
                }
                ActivityStatus::Completed => {}
                ActivityStatus::Failed => failures.push(index),
                ActivityStatus::Interrupted | ActivityStatus::Running => summary.interrupted += 1,
            }
        }
        summary.failed = FailureGroup::group(calls, &failures);

        summary
    }

    /// Returns the persisted text, or an empty string for a turn that made
    /// no calls. `usage`, such as token counts, ends the last line.
    pub(crate) fn render(&self, usage: Option<&str>) -> String {
        if self.tools == 0 {
            return String::new();
        }
        let mut text = format!("{}\n{}\n", Self::FORMAT_MARKER, self.header());
        for (index, edit) in self.edited.iter().enumerate() {
            let heading = if index == 0 { Self::EDITED_HEADING } else { "" };
            let noun = plural(edit.edits, "edit", "edits");
            let stat = edit.stat.map_or_else(String::new, |(added, removed)| {
                format!("+{added} −{removed}")
            });
            let _ = writeln!(
                text,
                "{heading}\t{}\t{}\t{stat}\t{noun}",
                Self::SUCCESS_GLYPH,
                edit.path
            );
        }
        for (index, group) in self.failed.iter().enumerate() {
            let heading = if index == 0 { Self::FAILED_HEADING } else { "" };
            let glyph = if group.recovery.is_some() {
                Self::SUCCESS_GLYPH
            } else {
                Self::FAILURE_GLYPH
            };
            let note = group.recovery.as_deref().unwrap_or_default();
            let _ = writeln!(
                text,
                "{heading}\t{glyph}\t{}\t{}\t{note}",
                group.target, group.result
            );
        }
        if let Some(line) = self.explored_line() {
            text.push_str(&line);
            text.push('\n');
        }
        if !self.used.is_empty() {
            let used = self
                .used
                .iter()
                .map(|(name, count)| format!("{name} ×{count}"))
                .collect::<Vec<_>>()
                .join(" · ");
            let _ = writeln!(text, "{}\t{used}", Self::USED_HEADING);
        }
        text.push_str(&self.divider(usage));

        text
    }

    /// Counts `event` under its kind.
    fn count_kind(&mut self, event: &ActivityEvent) {
        let counter = match event.kind {
            ActivityKind::Command => &mut self.commands,
            ActivityKind::Read => &mut self.explored.reads,
            ActivityKind::Search => &mut self.explored.searches,
            ActivityKind::List => &mut self.explored.listings,
            ActivityKind::WebSearch => &mut self.explored.web_searches,
            ActivityKind::FileChange => return,
            ActivityKind::Mcp | ActivityKind::Skill | ActivityKind::Tool => {
                let name = single_line(&event.name);
                match self.used.iter_mut().find(|(used, _)| *used == name) {
                    Some((_, count)) => *count += 1,
                    None => self.used.push((name, 1)),
                }

                return;
            }
        };
        *counter += 1;
    }

    /// Adds successful edit `event` to its file's totals.
    fn add_edit(&mut self, event: &ActivityEvent) {
        let path = call_target(event);
        let known = self
            .edited
            .iter()
            .position(|edit| same_path(&edit.path, &path));
        let index = known.unwrap_or_else(|| {
            self.edited.push(EditedFile {
                edits: 0,
                path,
                stat: None,
            });

            self.edited.len() - 1
        });
        let edit = &mut self.edited[index];
        edit.edits += 1;
        if let Some(stat) = event.facts.diff_stat {
            let (added, removed) = edit.stat.unwrap_or_default();
            edit.stat = Some((added + stat.added, removed + stat.removed));
        }
    }

    /// Returns the first line: steps, calls, and time.
    fn header(&self) -> String {
        let mut segments = Vec::new();
        segments.extend(self.steps_segment());
        segments.push(self.tools_segment());
        segments.extend(self.elapsed_seconds.map(format_elapsed));

        segments.join(" · ")
    }

    /// Returns the `Explored` line, or a `Ran` line when the turn only ran
    /// commands, or `None` when it did neither.
    fn explored_line(&self) -> Option<String> {
        let counts = &self.explored;
        let mut segments = [
            (counts.reads, "read", "reads"),
            (counts.searches, "search", "searches"),
            (counts.listings, "listing", "listings"),
            (counts.web_searches, "web search", "web searches"),
        ]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, one, many)| plural(count, one, many))
        .collect::<Vec<_>>();
        let heading = if segments.is_empty() {
            Self::RAN_HEADING
        } else {
            Self::EXPLORED_HEADING
        };
        if self.commands > 0 {
            let commands = plural(self.commands, "command", "commands");
            segments.push(if segments.is_empty() {
                commands
            } else {
                format!("ran {commands}")
            });
        }
        if segments.is_empty() {
            return None;
        }

        Some(format!("{heading}\t{}", segments.join(" · ")))
    }

    /// Returns the last line: time, steps, calls, failures, interrupted
    /// calls, and `usage`.
    fn divider(&self, usage: Option<&str>) -> String {
        let mut segments = Vec::new();
        segments.extend(self.elapsed_seconds.map(format_elapsed));
        segments.extend(self.steps_segment());
        segments.push(self.tools_segment());
        segments.extend(self.failures_segment());
        if self.interrupted > 0 {
            segments.push(format!("{} interrupted", self.interrupted));
        }
        segments.extend(usage.map(str::to_string));

        format!("{} {}", Self::DIVIDER_PREFIX, segments.join(" · "))
    }

    /// Returns `N steps`, omitted for a backend without narration or a turn
    /// without steps.
    fn steps_segment(&self) -> Option<String> {
        self.steps
            .filter(|steps| *steps > 0)
            .map(|steps| plural(steps, "step", "steps"))
    }

    /// Returns `N tools`.
    fn tools_segment(&self) -> String {
        plural(self.tools, "tool", "tools")
    }

    /// Returns `N failed (classes), all recovered`, or `None` when nothing
    /// failed.
    fn failures_segment(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let mut classes: Vec<(FailureClass, bool, usize)> = Vec::new();
        for group in &self.failed {
            let is_edit = group.kind == ActivityKind::FileChange;
            let noun = group.class.noun(is_edit, 1);
            match classes
                .iter_mut()
                .find(|(class, edit, _)| class.noun(*edit, 1) == noun)
            {
                Some((_, _, count)) => *count += group.count,
                None => classes.push((group.class, is_edit, group.count)),
            }
        }
        classes.sort_by_key(|(_, _, count)| std::cmp::Reverse(*count));
        let failed: usize = self.failed.iter().map(|group| group.count).sum();
        let recovered: usize = self
            .failed
            .iter()
            .filter(|group| group.recovery.is_some())
            .map(|group| group.count)
            .sum();
        let classes = classes
            .into_iter()
            .map(|(class, is_edit, count)| class.noun(is_edit, count))
            .collect::<Vec<_>>()
            .join(", ");
        let recovery = if recovered == failed {
            ", all recovered".to_string()
        } else if recovered > 0 {
            format!(", {recovered} recovered")
        } else {
            String::new()
        };

        Some(format!("{failed} failed ({classes}){recovery}"))
    }
}

/// Successful edits of one file.
#[derive(Clone, Debug, Eq, PartialEq)]
struct EditedFile {
    edits: usize,
    path: String,
    /// Lines added and removed, when any edit reported them.
    stat: Option<(u64, u64)>,
}

/// Exploration calls by kind.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ExploreCounts {
    listings: usize,
    reads: usize,
    searches: usize,
    web_searches: usize,
}

/// Failed calls of one kind on one target that failed the same way.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FailureGroup {
    class: FailureClass,
    /// Failed calls in the group.
    count: usize,
    kind: ActivityKind,
    /// How the turn recovered, such as `→ rerun exit 0`, or `None` when no
    /// later call of the same kind on the same target succeeded.
    recovery: Option<String>,
    /// How the calls failed, such as `exit 101`, or empty when the target
    /// says it.
    result: String,
    /// What failed, such as a command or `edit to lib.rs rejected`.
    target: String,
}

impl FailureGroup {
    /// Groups the failed calls at `failures` in `calls` and finds how the
    /// turn recovered from each group.
    fn group(calls: &[&ActivityEvent], failures: &[usize]) -> Vec<Self> {
        let mut groups: Vec<(FailureClass, String, Vec<usize>)> = Vec::new();
        for &index in failures {
            let event = calls[index];
            let class = FailureClass::of(event);
            let target = call_target(event);
            match groups
                .iter_mut()
                .find(|(group_class, group_target, members)| {
                    *group_class == class
                        && calls[members[0]].kind == event.kind
                        && same_target(event.kind, group_target, &target)
                }) {
                Some((_, _, members)) => members.push(index),
                None => groups.push((class, target, vec![index])),
            }
        }

        groups
            .into_iter()
            .map(|(class, target, members)| Self::build(calls, class, &target, &members))
            .collect()
    }

    /// Describes one group of failed calls.
    fn build(
        calls: &[&ActivityEvent],
        class: FailureClass,
        target: &str,
        members: &[usize],
    ) -> Self {
        let first = calls[members[0]];
        let last_index = members[members.len() - 1];
        let count = members.len();
        let reason = first
            .facts
            .reason
            .as_deref()
            .map(single_line)
            .filter(|reason| !reason.is_empty());
        let (target_text, result) = if first.kind == ActivityKind::FileChange {
            let edits = if count == 1 {
                "edit".to_string()
            } else {
                format!("{count} edits")
            };
            let mut text = format!("{edits} to {} {}", file_name(target), class.label());
            if let Some(reason) = &reason {
                let _ = write!(text, " ({reason})");
            }

            (text, String::new())
        } else {
            let mut result = match class {
                FailureClass::Exit(code) => format!("exit {code}"),
                FailureClass::Signal(signal) => format!("signal {signal}"),
                _ => class.label().to_string(),
            };
            if let Some(reason) = reason.filter(|_| class.is_reported()) {
                let _ = write!(result, " · {reason}");
            }
            if count > 1 {
                let _ = write!(result, " ×{count}");
            }

            (target.to_string(), result)
        };

        Self {
            class,
            count,
            kind: first.kind,
            recovery: Self::recovery(calls, last_index),
            result,
            target: target_text,
        }
    }

    /// Returns how the turn recovered from the failed call at
    /// `failed_index`: a later successful call of the same kind on the same
    /// target, after re-reading the file for an edit.
    fn recovery(calls: &[&ActivityEvent], failed_index: usize) -> Option<String> {
        let failed = calls[failed_index];
        let target = call_target(failed);
        let (offset, success) =
            calls[failed_index + 1..]
                .iter()
                .enumerate()
                .find(|(_, event)| {
                    event.kind == failed.kind
                        && event.outcome() == ActivityStatus::Completed
                        && same_target(failed.kind, &target, &call_target(event))
                })?;
        let recovery = match failed.kind {
            ActivityKind::Command => match success.effective_exit_code() {
                Some(code) => format!("rerun exit {code}"),
                None => "rerun succeeded".to_string(),
            },
            ActivityKind::FileChange => {
                let between = &calls[failed_index + 1..failed_index + 1 + offset];
                let reread = between.iter().any(|event| {
                    event.kind == ActivityKind::Read
                        && event.outcome() == ActivityStatus::Completed
                        && same_path(&call_target(event), &target)
                });
                if reread {
                    "retried after re-reading".to_string()
                } else {
                    "retried".to_string()
                }
            }
            _ => "retried".to_string(),
        };

        Some(format!("{}{recovery}", TurnSummary::RECOVERY_PREFIX))
    }
}

/// How a call failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureClass {
    Denied,
    Error,
    /// A command's own nonzero exit status.
    Exit(i64),
    Rejected,
    /// A command ended by a signal.
    Signal(i64),
    TimedOut,
    Unavailable,
}

impl FailureClass {
    /// Classifies failed call `event`.
    fn of(event: &ActivityEvent) -> Self {
        if let Some(signal) = event.facts.signal {
            return Self::Signal(signal);
        }
        if let Some(code) = event.effective_exit_code().filter(|code| *code != 0) {
            return Self::Exit(code);
        }

        match event.facts.failure {
            Some(ActivityFailure::Denied) => Self::Denied,
            Some(ActivityFailure::Rejected) => Self::Rejected,
            Some(ActivityFailure::TimedOut) => Self::TimedOut,
            Some(ActivityFailure::Unavailable) => Self::Unavailable,
            Some(ActivityFailure::Error) | None => Self::Error,
        }
    }

    /// Returns the label a row shows, such as `rejected`.
    fn label(self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Error => "failed",
            Self::Exit(_) => "exited nonzero",
            Self::Rejected => "rejected",
            Self::Signal(_) => "killed",
            Self::TimedOut => "timed out",
            Self::Unavailable => "unavailable",
        }
    }

    /// Returns whether the provider's reason explains the failure, which an
    /// exit status or a signal already does.
    fn is_reported(self) -> bool {
        !matches!(self, Self::Exit(_) | Self::Signal(_))
    }

    /// Returns `count` failures of this class, such as `2 rejected edits`.
    fn noun(self, is_edit: bool, count: usize) -> String {
        let (one, many) = match self {
            Self::Denied if is_edit => ("denied edit", "denied edits"),
            Self::Denied => ("denied call", "denied calls"),
            Self::Error if is_edit => ("failed edit", "failed edits"),
            Self::Error => ("error", "errors"),
            Self::Exit(_) => ("nonzero exit", "nonzero exits"),
            Self::Rejected if is_edit => ("rejected edit", "rejected edits"),
            Self::Rejected => ("rejected call", "rejected calls"),
            Self::Signal(_) => ("signal", "signals"),
            Self::TimedOut => ("timeout", "timeouts"),
            Self::Unavailable => ("unavailable tool", "unavailable tools"),
        };

        plural(count, one, many)
    }
}

/// Returns whether two targets of `kind` name the same thing: the same
/// file for edits and reads, the same command once trailing output filters
/// are stripped, else the same text.
fn same_target(kind: ActivityKind, left: &str, right: &str) -> bool {
    match kind {
        ActivityKind::Command => command_core(left) == command_core(right),
        ActivityKind::FileChange | ActivityKind::List | ActivityKind::Read => {
            same_path(left, right)
        }
        _ => left == right,
    }
}

/// Strips trailing `2>&1`, `| tail …`, and `| head …` from `command`. A
/// trailing stage counts as an output filter only when it is `head` or
/// `tail` with nothing but options and counts after it, so a stage such as
/// `| tail -5 && cargo clippy`, `| head -1; y`, or one inside quotes is
/// kept.
fn command_core(command: &str) -> &str {
    let mut core = command.trim();
    loop {
        if let Some(stripped) = core.strip_suffix("2>&1") {
            core = stripped.trim_end();
            continue;
        }
        let Some(pipe) = core.rfind('|') else {
            return core;
        };
        if !is_output_filter(&core[pipe + 1..]) || core[..pipe].ends_with('|') {
            return core;
        }
        core = core[..pipe].trim_end();
    }
}

/// Returns whether pipeline `stage` is `head` or `tail` followed only by
/// options and counts, such as `tail -n 20` or `head -5`.
fn is_output_filter(stage: &str) -> bool {
    let mut words = stage.split_whitespace();
    let is_filter = words
        .next()
        .is_some_and(|word| OUTPUT_FILTERS.contains(&word));

    is_filter
        && words.all(|word| {
            word.chars()
                .all(|character| character.is_ascii_alphanumeric() || "-+".contains(character))
        })
}

/// Formats `seconds` as the live status line does, such as `20m 23s`.
fn format_elapsed(seconds: u64) -> String {
    ag_tui_text::text_util::format_duration_compact(i64::try_from(seconds).unwrap_or(i64::MAX))
}

/// Returns `count` with the noun matching it.
fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

#[cfg(test)]
#[path = "turn_summary_test.rs"]
mod tests;

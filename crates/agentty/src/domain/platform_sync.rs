//! What a session tells a platform's Sync, and nothing more: the title and
//! brief of its first message, its repository, its label, and the summary
//! it finishes with.
//!
//! The platform's Sync is the account's shared record of the work its
//! sessions do. It is unrelated to syncing a project with its default
//! branch.

/// Longest title, in characters.
pub(crate) const MAX_TITLE_CHARS: usize = 200;
/// Longest brief, in characters.
pub(crate) const MAX_BRIEF_CHARS: usize = 4000;
/// Longest session label, in characters.
pub(crate) const MAX_SESSION_CHARS: usize = 100;
/// Longest summary, in characters.
pub(crate) const MAX_SUMMARY_CHARS: usize = 2000;
/// Shortest and longest repository, in characters.
const REPOSITORY_CHARS: std::ops::RangeInclusive<usize> = 3..=200;
/// Characters of the session identifier a label shows.
const SESSION_ID_CHARS: usize = 6;
/// Summary sent for a session that has no title.
pub(crate) const FALLBACK_SUMMARY: &str = "Finished in Agentty.";

/// Choice a session offers while its task on the platform's Sync holds it
/// back from starting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformTaskChoice {
    /// Another session holds the work: `t` takes the task over and `n`
    /// starts separate work.
    Conflict,
    /// Agentty quit before the start was answered: `s` asks again.
    Interrupted,
}

/// Returns the first line of `message` that is not blank, trimmed and cut
/// to [`MAX_TITLE_CHARS`], or `None` for a message without text.
pub(crate) fn work_title(message: &str) -> Option<String> {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let title = cut(line, MAX_TITLE_CHARS);

    Some(title.trim_end().to_string())
}

/// Returns `message` without outer whitespace, cut to [`MAX_BRIEF_CHARS`].
pub(crate) fn work_brief(message: &str) -> String {
    cut(message.trim(), MAX_BRIEF_CHARS)
}

/// Returns the label the account's other sessions see for the session
/// `session_id` of the instance `instance_name`: the name and the start of
/// the identifier, at most [`MAX_SESSION_CHARS`] characters.
pub(crate) fn session_label(instance_name: &str, session_id: &str) -> String {
    let id = cut(session_id, SESSION_ID_CHARS);
    let name_chars = MAX_SESSION_CHARS - SESSION_ID_CHARS - 1;
    let name = cut(instance_name.trim(), name_chars);
    let name = name.trim_end();
    if name.is_empty() {
        return id;
    }

    format!("{name} {id}")
}

/// Returns the repository a Sync request names for the `origin` remote
/// `remote_url`: its `host/path` without scheme, credentials, or `.git`,
/// when that fits what the platform accepts; `None` means no request.
pub(crate) fn sync_repository(remote_url: &str) -> Option<String> {
    ag_forge::remote_repository_path(remote_url).filter(|path| is_sync_repository(path))
}

/// Returns whether `repository` fits what the platform accepts: 3 to 200
/// ASCII letters, digits, `.`, `_`, `~`, `:`, `-`, and `/`, as a host, a
/// `/`, and a path, with no empty segment.
pub(crate) fn is_sync_repository(repository: &str) -> bool {
    REPOSITORY_CHARS.contains(&repository.len())
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._~:/-".contains(&byte))
        && repository.split_once('/').is_some_and(|(host, path)| {
            !host.is_empty() && path.split('/').all(|segment| !segment.is_empty())
        })
}

/// Returns the summary a finished session sends: its title without outer
/// whitespace, cut to [`MAX_SUMMARY_CHARS`], or [`FALLBACK_SUMMARY`] when
/// it has none.
pub(crate) fn finish_summary(title: Option<&str>) -> String {
    let summary = cut(title.unwrap_or_default().trim(), MAX_SUMMARY_CHARS);
    let summary = summary.trim_end();
    if summary.is_empty() {
        return FALLBACK_SUMMARY.to_string();
    }

    summary.to_string()
}

/// Returns the line of a notice that names the milestone `number` titled
/// `title`, such as `Milestone M3: Ship milestones`.
pub(crate) fn milestone_line(number: u64, title: &str) -> String {
    format!("Milestone M{number}: {title}")
}

/// Returns when a session was last active, `at_ms` against `now_ms`, both
/// in milliseconds since the Unix epoch, such as `5 minutes ago`.
pub(crate) fn last_active_text(now_ms: u64, at_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(at_ms) / 60_000;
    let (count, unit) = match minutes {
        0 => return "just now".to_string(),
        1..60 => (minutes, "minute"),
        60..1440 => (minutes / 60, "hour"),
        _ => (minutes / 1440, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };

    format!("{count} {unit}{plural} ago")
}

/// Returns the first `max_chars` characters of `text`.
fn cut(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
#[path = "platform_sync_test.rs"]
mod tests;

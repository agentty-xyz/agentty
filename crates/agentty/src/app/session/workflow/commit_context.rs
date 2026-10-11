//! Deterministic, bounded evidence for commit-message generation.

use super::prompt_context::json_prefix;
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};

/// Keeps the full diff when small, otherwise retains bounded excerpts from
/// every file that fits. Large inventories retain both ends and disclose loss.
pub(super) fn diff(input: &str) -> String {
    if input.len() <= 20_000 {
        return input.to_string();
    }
    let mut starts = input
        .match_indices("diff --git ")
        .filter(|(offset, _)| *offset == 0 || input.as_bytes()[offset - 1] == b'\n')
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    starts.push(input.len());
    let files = starts
        .windows(2)
        .map(|positions| &input[positions[0]..positions[1]])
        .collect::<Vec<_>>();
    if files.is_empty() {
        return excerpt(input, 6_000).to_string();
    }
    let mut retained = files.len().min(64);
    loop {
        let evidence = files
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                *index < retained.div_ceil(2) || *index >= files.len() - retained / 2
            })
            .map(|(_, file)| excerpt(file, 192))
            .collect::<Vec<_>>();
        let result = serde_json::json!({
            "representation": "bounded diff excerpts; original detail omitted",
            "file_count": files.len(),
            "omitted_files": files.len() - evidence.len(),
            "files": evidence,
        })
        .to_string();
        if result.len() <= 20_000 {
            return result;
        }
        retained /= 2;
    }
}

/// Bounds history and file inventory before rendering the fallback prompt.
pub(super) fn fallback(files: &[String], transcript: &SessionTranscript) -> String {
    let history = transcript
        .messages()
        .iter()
        .filter(|message| {
            matches!(
                message.kind,
                SessionMessageKind::UserPrompt | SessionMessageKind::AssistantAnswer
            )
        })
        .collect::<Vec<_>>();
    let chat = history.iter().rev().take(5).rev().map(|message| {
        serde_json::json!({"role": message.kind.as_str(), "content": excerpt(&message.content, 1_000)})
    }).collect::<Vec<_>>();
    let selected = files
        .iter()
        .enumerate()
        .filter(|(index, _)| *index < 32 || *index >= files.len().saturating_sub(32))
        .map(|(_, file)| json_prefix(file, 128))
        .collect::<Vec<_>>();

    serde_json::json!({
        "changed_files": selected,
        "omitted_files": files.len() - selected.len(),
        "chat_history": chat,
        "omitted_messages": history.len() - chat.len(),
    })
    .to_string()
}

/// Keeps ordinary text intact and labels bounded UTF-8 excerpts explicitly.
pub(super) fn excerpt(input: &str, bytes: usize) -> serde_json::Value {
    if serde_json::json!(input).to_string().len() <= bytes * 2 + 128 {
        return serde_json::json!(input);
    }
    let prefix = json_prefix(input, bytes);
    let suffix_start = input.ceil_char_boundary(input.len().saturating_sub(bytes / 2));
    let suffix = json_prefix(&input[suffix_start..], bytes);

    serde_json::json!({"prefix": prefix, "suffix": suffix, "truncated": true, "original_bytes": input.len()})
}

#[cfg(test)]
#[path = "commit_context_test.rs"]
mod tests;

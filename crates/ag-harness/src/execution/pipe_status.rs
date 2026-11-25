//! Exit statuses of every stage of a `bash` command's final pipeline,
//! reported beside the command's own exit status.
//!
//! A command such as `cargo test 2>&1 | tail -20` exits with `tail`'s
//! status. To also report `cargo`'s, a command whose source ends in a
//! pipeline runs after a prefix that installs an `EXIT` trap writing Bash's
//! `PIPESTATUS`, with the shell's final status, to a status file named by a
//! random per-call nonce in the turn's temporary directory, which every
//! executor grants writes to. The harness reads and deletes the file after
//! the command settles; a missing, oversized, or garbled file reports no
//! stages. No descriptor outlives the shell, so background jobs cannot keep
//! the command's output open, and the command's output never carries the
//! status. A [`StreamFilter`] removes any copy of the prefix the command
//! prints (a syntax error quoting its first line, `ps`) as output arrives,
//! before the capture budget applies. `pipefail` stays off, and the
//! command's exit status and output are unchanged. The prefix travels in the
//! command words, so it works under every executor that runs them with Bash.
//! A turn without a temporary directory reports no stages.
//!
//! Only commands ending in a pipeline get the prefix: a trap keeps Bash
//! alive after the command, so it can no longer replace itself with a final
//! simple command, which would change how a signal that kills that command
//! is reported. The probe is display-only and model-influenceable, like the
//! sandbox-denial guess: the command can see its own source, replace or
//! clear the trap, or write the file, and `false | true; exit 0` reports the
//! earlier pipeline. A status that does not match the observed exit reports
//! no stages, and so does a command killed before its trap ran.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rustix::fs::{FileType, Mode, OFlags};
use uuid::Uuid;

/// Start of the status file's name, before the nonce.
const STATUS_FILE_PREFIX: &str = ".agentty-pipe-status-";

/// Longest status file read; a longer one reports no stages.
const MAX_STATUS_BYTES: usize = 4096;

/// The `EXIT` trap that reports one command's final pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PipeStatusProbe {
    /// Random value naming this call's status file.
    nonce: u128,
}

impl PipeStatusProbe {
    /// Returns a probe for `command` when its source ends in a pipeline.
    pub(super) fn for_command(command: &str) -> Option<Self> {
        ends_in_pipeline(command).then(|| Self {
            nonce: Uuid::new_v4().as_u128(),
        })
    }

    /// Returns `command` behind this probe's prefix.
    pub(super) fn wrap(self, command: &str) -> String {
        format!("{}{command}", self.prefix())
    }

    /// Returns where the trap writes the statuses, in the turn's
    /// `temporary_directory`, which the command sees as `TMPDIR`.
    pub(super) fn status_file(self, temporary_directory: &Path) -> PathBuf {
        temporary_directory.join(self.file_name())
    }

    /// Returns the status file's name.
    fn file_name(self) -> String {
        format!("{STATUS_FILE_PREFIX}{:032x}", self.nonce)
    }

    /// Returns the source prepended to the command, on its first line so
    /// error messages keep their line numbers. It records the status file's
    /// path before the command can change `TMPDIR`. `PIPESTATUS` and `$?`
    /// are read by the trap's first command, before anything resets them;
    /// the group discards its own trace output and errors, so `set -x` adds
    /// nothing, `IFS` and `builtin` keep the command's settings and
    /// functions from changing the line, and `>|` writes despite
    /// `noclobber`.
    fn prefix(self) -> String {
        format!(
            "__agentty_pipe_status_file=\"${{TMPDIR-}}/{}\"; trap '{{ \
             __agentty_pipe_status=(\"$?\" \"${{PIPESTATUS[@]}}\"); IFS=\" \"; builtin printf \
             \"%s\\n\" \"${{__agentty_pipe_status[*]}}\" >| \"$__agentty_pipe_status_file\"; }} \
             2>/dev/null' EXIT; ",
            self.file_name()
        )
    }
}

/// Removes every copy of a [`PipeStatusProbe`]'s prefix from one output
/// stream as it arrives. It holds back only the bytes that could start the
/// prefix, so the capture sees the rest of the output unchanged and in
/// order.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct StreamFilter {
    /// Bytes that could still begin the prefix.
    pending: Vec<u8>,
    prefix: Vec<u8>,
}

impl StreamFilter {
    /// Returns the filter that removes `probe`'s prefix.
    pub(super) fn new(probe: PipeStatusProbe) -> Self {
        Self {
            pending: Vec::new(),
            prefix: probe.prefix().into_bytes(),
        }
    }

    /// Filters `bytes`, passing the output to keep to `keep`.
    pub(super) fn push(&mut self, bytes: &[u8], mut keep: impl FnMut(&[u8])) {
        self.pending.extend_from_slice(bytes);
        while let Some(start) = find(&self.pending, &self.prefix) {
            keep(&self.pending[..start]);
            self.pending.drain(..start + self.prefix.len());
        }
        let kept = self.pending.len() - partial_match(&self.pending, &self.prefix);
        keep(&self.pending[..kept]);
        self.pending.drain(..kept);
    }

    /// Passes every held-back byte to `keep` once the stream ended.
    pub(super) fn finish(&mut self, mut keep: impl FnMut(&[u8])) {
        keep(&self.pending);
        self.pending.clear();
    }
}

/// Reads and deletes the status file at `path` once the command settled,
/// returning its statuses as [`statuses`] reads them. The command controls
/// the path, so only a regular file is read, opened without following a
/// symbolic link and without blocking on a FIFO; anything else, a missing
/// or oversized file, or a read error reports no stages. Blocking: run it
/// off the async workers.
pub(super) fn take_statuses(path: &Path, exit_code: Option<i32>) -> Vec<i32> {
    let payload = read_status_file(path);
    // Removal fails only when the path is already gone or the command
    // replaced it with something it cannot delete; neither changes the
    // result.
    let _ = std::fs::remove_file(path);

    payload.map_or_else(Vec::new, |payload| {
        statuses(payload.strip_suffix(b"\n").unwrap_or(&payload), exit_code)
    })
}

/// Returns the first [`MAX_STATUS_BYTES`] of the regular file at `path`, or
/// `None` for a symbolic link, FIFO, or other special file, a missing or
/// longer file, or a failed read.
fn read_status_file(path: &Path) -> Option<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::empty()).ok()?;
    let metadata = rustix::fs::fstat(&file).ok()?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file() {
        return None;
    }
    let mut payload = Vec::new();
    std::fs::File::from(file)
        .take(MAX_STATUS_BYTES as u64 + 1)
        .read_to_end(&mut payload)
        .ok()?;

    (payload.len() <= MAX_STATUS_BYTES).then_some(payload)
}

/// Returns the final pipeline's statuses from a status `payload` when it
/// had more than one stage and matches `exit_code`, the shell's observed
/// exit status: the shell exits with the last stage's status, with
/// `pipefail` with the last failing one (or 0), and after `!` with the
/// negation of either.
fn statuses(payload: &[u8], exit_code: Option<i32>) -> Vec<i32> {
    let statuses = String::from_utf8_lossy(payload)
        .split(' ')
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();
    let Some((&shell, pipeline)) = statuses.split_first() else {
        return Vec::new();
    };
    if exit_code != Some(shell) || pipeline.len() < 2 {
        return Vec::new();
    }
    let last = pipeline.last().copied().unwrap_or_default();
    let last_failure = pipeline
        .iter()
        .rev()
        .copied()
        .find(|status| *status != 0)
        .unwrap_or(0);
    let negated = |status: i32| i32::from(status == 0);
    let consistent = [last, last_failure, negated(last), negated(last_failure)];

    if consistent.contains(&shell) {
        pipeline.to_vec()
    } else {
        Vec::new()
    }
}

/// Returns the index of the first occurrence of `needle` in `bytes`.
fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Returns the length of the longest end of `bytes` that begins `needle`
/// without completing it.
fn partial_match(bytes: &[u8], needle: &[u8]) -> usize {
    (1..needle.len().min(bytes.len() + 1))
        .rev()
        .find(|length| bytes.ends_with(&needle[..*length]))
        .unwrap_or(0)
}

/// Returns whether `command`'s source ends in a pipeline of two or more
/// stages: its last command contains an unquoted `|` or `|&` that no list
/// operator (`;`, `&`, `&&`, `||`, or a newline) follows. This is a lexical
/// guess, not a parse: a pipe inside a here-document or command
/// substitution can make it `true`, which costs only Bash's final-command
/// `exec` optimization, and a pipe inside a compound command's body can
/// make it `false`, which reports no stages.
fn ends_in_pipeline(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut has_pipe = false;
    let mut after_separator = false;
    let mut awaiting_operand = false;
    let mut previous = b' ';
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let next = bytes.get(index + 1).copied();
        let starts_command = !byte.is_ascii_whitespace() && byte != b'#';
        if starts_command && after_separator {
            has_pipe = false;
            after_separator = false;
        }
        if starts_command {
            awaiting_operand = false;
        }
        match byte {
            b'\\' => index += 1,
            b'\'' => index = closing(bytes, index, b'\''),
            b'"' => index = closing(bytes, index, b'"'),
            b'#' if previous.is_ascii_whitespace() || b";&|()<>".contains(&previous) => {
                index = bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| index + offset - 1);
            }
            b'|' if next == Some(b'|') => {
                after_separator = true;
                awaiting_operand = true;
                index += 1;
            }
            b'|' if previous != b'>' => {
                has_pipe = true;
                awaiting_operand = true;
                if next == Some(b'&') {
                    index += 1;
                }
            }
            b'&' if next == Some(b'&') => {
                after_separator = true;
                awaiting_operand = true;
                index += 1;
            }
            b'&' if previous != b'>' && previous != b'<' && next != Some(b'>') => {
                after_separator = true;
            }
            b';' => after_separator = true,
            b'\n' if !awaiting_operand => after_separator = true,
            _ => {}
        }
        previous = bytes.get(index).copied().unwrap_or(byte);
        index += 1;
    }

    has_pipe
}

/// Returns the index of the quote closing the one at `open`, skipping
/// escaped characters inside double quotes, or the last index when it is
/// unterminated.
fn closing(bytes: &[u8], open: usize, quote: u8) -> usize {
    let mut index = open + 1;
    while let Some(&byte) = bytes.get(index) {
        if byte == quote {
            return index;
        }
        if quote == b'"' && byte == b'\\' {
            index += 1;
        }
        index += 1;
    }

    bytes.len().saturating_sub(1)
}

#[cfg(test)]
#[path = "pipe_status_test.rs"]
mod tests;

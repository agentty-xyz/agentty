//! Pipe status probe unit tests: which commands it wraps, how its filter
//! removes its prefix from streamed output, and how it reads its status
//! file.

use std::time::{Duration, Instant};

use super::{PipeStatusProbe, StreamFilter, ends_in_pipeline, statuses, take_statuses};
use crate::execution::contract::{Limits, Output, OutputStream};

fn probe() -> PipeStatusProbe {
    PipeStatusProbe::for_command("a | b").expect("pipeline probe")
}

/// Feeds `chunks` through `filter`, returning the output it kept.
fn filtered(filter: &mut StreamFilter, chunks: &[&[u8]]) -> Vec<u8> {
    let mut kept = Vec::new();
    for chunk in chunks {
        filter.push(chunk, |bytes| kept.extend_from_slice(bytes));
    }
    filter.finish(|bytes| kept.extend_from_slice(bytes));

    kept
}

#[test]
fn only_commands_ending_in_a_pipeline_are_probed() {
    for (command, expected) in [
        ("cargo test 2>&1 | tail -20", true),
        ("a |& b", true),
        ("a |\n  b", true),
        ("a | b\n", true),
        ("a | b # trailing | comment", true),
        ("a | b # note\n", true),
        ("a | b # note | c\nd", false),
        ("a | b &", true),
        ("a; b | c", true),
        ("x=$(a | b)", true),
        ("cargo test", false),
        ("a || b", false),
        ("a | b; c", false),
        ("a | b && c", false),
        ("a | b\nc", false),
        ("a | b & c", false),
        ("echo 'a | b'", false),
        ("echo \"a \\\" | b\"", false),
        ("echo a\\|b", false),
        ("echo hi >| file", false),
        ("a 2>&1", false),
        ("a &> log", false),
        ("a <&3", false),
        ("# a | b", false),
        ("echo x#y | z", true),
        ("echo 'unterminated | b", false),
        ("echo #", false),
    ] {
        // Act
        let probed = ends_in_pipeline(command);

        // Assert
        assert_eq!(probed, expected, "{command:?}");
    }
    assert!(PipeStatusProbe::for_command("cargo test").is_none());
}

#[test]
fn probes_name_a_fresh_status_file_in_the_temporary_directory() {
    // Arrange
    let first = probe();
    let second = probe();
    let directory = std::path::Path::new("/tmp/turn");

    // Act
    let wrapped = first.wrap("a | b");
    let file = first.status_file(directory);

    // Assert
    assert_ne!(file, second.status_file(directory));
    assert_eq!(file.parent(), Some(directory));
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .expect("file name");
    assert!(name.starts_with(".agentty-pipe-status-"), "{name}");
    assert!(
        wrapped.starts_with(&format!(
            "__agentty_pipe_status_file=\"${{TMPDIR-}}/{name}\"; trap '"
        )),
        "{wrapped}"
    );
    assert!(wrapped.ends_with("EXIT; a | b"), "{wrapped}");
    assert!(!wrapped.contains('\n'), "{wrapped}");
}

#[test]
fn filters_remove_every_copy_of_the_prefix_wherever_chunks_split_it() {
    // Arrange
    let probe = probe();
    let prefix = probe.wrap("");
    let stream = format!("bash: -c: line 1: `{prefix}echo a | | b'\n{prefix}");
    let expected = b"bash: -c: line 1: `echo a | | b'\n";

    for split in 0..=stream.len() {
        let mut filter = StreamFilter::new(probe);
        let (first, second) = stream.as_bytes().split_at(split);

        // Act
        let kept = filtered(&mut filter, &[first, second]);

        // Assert
        assert_eq!(kept, expected, "split at {split}");
    }
}

#[test]
fn a_held_back_partial_prefix_is_kept_when_the_stream_ends() {
    // Arrange
    let mut filter = StreamFilter::new(probe());

    // Act
    let kept = filtered(&mut filter, &[b"out\n__agentty_pipe"]);

    // Assert
    assert_eq!(kept, b"out\n__agentty_pipe");
}

#[test]
fn output_omits_the_prefix_before_the_budget_applies() {
    // Arrange: stdout fills the budget exactly once the prefix is removed.
    let probe = probe();
    let limits = Limits::new(Instant::now(), Duration::from_secs(1), 8)
        .expect("limits")
        .with_pipe_status(Some(probe));
    let mut output = Output::new(limits);
    output.capture(OutputStream::Stdout, b"1234");
    output.capture(OutputStream::Stdout, probe.wrap("").as_bytes());
    output.capture(OutputStream::Stdout, b"5678");
    output.capture(OutputStream::Stderr, probe.wrap("").as_bytes());

    // Act
    output.finish();

    // Assert
    assert_eq!(output.stdout().head, b"12345678");
    assert_eq!(output.stderr().head, b"");
    assert!(!output.truncated());
}

#[test]
fn status_files_are_read_once_and_deleted() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let written = directory.path().join("written");
    std::fs::write(&written, "0 101 0\n").expect("status file");
    let oversized = directory.path().join("oversized");
    std::fs::write(&oversized, "0 ".repeat(4096)).expect("oversized file");

    // Act
    let reported = take_statuses(&written, Some(0));
    let again = take_statuses(&written, Some(0));
    let too_long = take_statuses(&oversized, Some(0));

    // Assert
    assert_eq!(reported, [101, 0]);
    assert_eq!(again, Vec::<i32>::new(), "a missing file reports no stages");
    assert_eq!(too_long, Vec::<i32>::new());
    assert!(!written.exists());
    assert!(!oversized.exists());
}

#[test]
fn a_symbolic_link_status_file_is_removed_without_reading_its_target() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let secret = directory.path().join("secret");
    std::fs::write(&secret, "0 1 0\n").expect("linked file");
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&secret, &link).expect("symbolic link");

    // Act
    let linked = take_statuses(&link, Some(0));

    // Assert
    assert_eq!(linked, Vec::<i32>::new());
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(std::fs::read(&secret).expect("target kept"), b"0 1 0\n");
}

#[test]
fn statuses_report_stages_only_when_they_match_the_exit() {
    for (payload, exit_code, expected) in [
        ("0 101 0", Some(0), vec![101, 0]),
        // `pipefail`: the last failing stage.
        ("1 1 0", Some(1), vec![1, 0]),
        // `!`: the negation of the last stage.
        ("1 0 0", Some(1), vec![0, 0]),
        ("0 1 1", Some(0), vec![1, 1]),
        // `false | true; exit 5`: the pipeline is not what the shell
        // exited with.
        ("5 1 0", Some(5), Vec::new()),
        // The shell's own status disagrees with the observed exit.
        ("0 1 0", Some(3), Vec::new()),
        ("0 1 0", None, Vec::new()),
        // A single command is no pipeline.
        ("0 0", Some(0), Vec::new()),
        ("", Some(0), Vec::new()),
        ("0 x 0", Some(0), Vec::new()),
    ] {
        // Act
        let reported = statuses(payload.as_bytes(), exit_code);

        // Assert
        assert_eq!(reported, expected, "{payload:?} {exit_code:?}");
    }
}

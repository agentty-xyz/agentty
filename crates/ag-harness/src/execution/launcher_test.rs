//! Launcher unit tests for instruction and notice channels, child
//! observation, and Seatbelt profile generation.

#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;

use rustix::io::Errno;
use rustix::process::{Pid, WaitOptions, waitpid};

#[cfg(target_os = "macos")]
use super::seatbelt;
use super::{ChildEvent, child_event, instruction, write_notice};
#[cfg(target_os = "macos")]
use crate::execution::contract::ReadScope;
use crate::execution::wire::Notice;
#[cfg(target_os = "macos")]
use crate::execution::wire::{GitWrites, Launch};

#[test]
fn instruction_distinguishes_no_data_acknowledgment_eof_and_invalid_descriptor() {
    // Arrange
    let (input, mut output) = UnixStream::pair().expect("channel");
    input.set_nonblocking(true).expect("nonblocking");

    // Act / Assert
    assert!(!instruction(&input).expect("no instruction"));
    output.write_all(b"x").expect("acknowledge");
    assert!(instruction(&input).expect("instruction"));
    drop(output);
    assert!(instruction(&input).expect("EOF cancels"));
    let directory = tempfile::tempdir().expect("directory");
    let output = std::fs::File::create(directory.path().join("write-only")).expect("write-only");
    assert!(instruction(&output).is_err());
}

#[test]
fn notice_handles_short_writes_and_preserves_channel_failures() {
    // Arrange
    let mut bytes = Vec::new();

    // Act
    write_notice(&Notice::Finished, |remaining| {
        bytes.push(remaining[0]);
        Ok(1)
    })
    .expect("short writes");
    let stalled = write_notice(&Notice::Failed, |_| Ok(0));
    let closed = write_notice(&Notice::Failed, |_| Err(io::ErrorKind::BrokenPipe.into()));

    // Assert
    assert_eq!(bytes, b"\"Finished\"\n");
    assert_eq!(
        stalled.expect_err("zero write").kind(),
        io::ErrorKind::WriteZero
    );
    assert_eq!(
        closed.expect_err("closed").kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn child_observation_keeps_main_exit_descendants_and_wait_failure_distinct() {
    // Arrange
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("child");
    let main_pid = child.id();
    let pid = Pid::from_raw(i32::try_from(main_pid).expect("pid")).expect("pid");
    let status = waitpid(Some(pid), WaitOptions::empty())
        .expect("wait")
        .expect("exit");

    // Act / Assert
    assert_eq!(
        child.wait().expect_err("already reaped").raw_os_error(),
        Some(Errno::CHILD.raw_os_error())
    );
    assert!(matches!(
        child_event(Ok(Some(status)), main_pid, false).expect("main"),
        ChildEvent::MainExit(Notice::MainExit {
            code: Some(0),
            signal: None
        })
    ));
    assert!(matches!(
        child_event(Ok(Some(status)), 0, true).expect("other descendant"),
        ChildEvent::Reaped
    ));
    assert!(matches!(
        child_event(Ok(None), main_pid, true).expect("pending"),
        ChildEvent::Pending
    ));
    assert!(matches!(
        child_event(Err(Errno::CHILD), main_pid, true).expect("finished"),
        ChildEvent::Finished
    ));
    assert!(
        child_event(Err(Errno::CHILD), main_pid, false).is_err(),
        "missing main observation is not success"
    );
    assert!(
        child_event(Err(Errno::INTR), main_pid, true).is_err(),
        "wait failure is not completion"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_profiles_reject_non_utf8_paths_and_keep_host_information_explicit() {
    // Arrange
    let mut configuration = Launch {
        arguments: vec![],
        directory: "/workspace".into(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        read_scope: ReadScope::Granted,
        git_metadata: vec![],
        git_writes: None,
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_denials: vec![],
        system: false,
        unix_sockets: vec![],
        workspace: "/workspace".into(),
        write_nodes: vec![],
        writes: vec![],
    };
    let invalid = OsString::from_vec(vec![0xff]);

    // Act / Assert
    let profile = seatbelt(&configuration).expect("profile");
    assert!(!profile.contains("sysctl-read"));
    assert!(!profile.contains("network"));
    assert!(!profile.contains("(allow file-read*)\n"));
    configuration.unix_sockets.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.unix_sockets.clear();
    configuration.read_denials.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.read_denials.clear();
    configuration.external_reads.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.external_reads.clear();
    configuration.writes.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.writes.clear();
    configuration.git_metadata.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.git_metadata.clear();
    for common in [invalid.into(), "/repo/.git\"".into(), "/repo/.git\n".into()] {
        configuration.git_writes = Some(GitWrites {
            admin: None,
            common,
        });
        assert!(seatbelt(&configuration).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_git_writes_and_network_rules_follow_the_metadata_denials() {
    // Arrange
    let configuration = Launch {
        arguments: vec![],
        directory: "/workspace".into(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        read_scope: ReadScope::FullDisk,
        git_metadata: vec!["/workspace/.git".into()],
        git_writes: Some(GitWrites {
            admin: None,
            common: "/workspace/.git".into(),
        }),
        host_information: true,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: true,
        read_denials: vec!["/home/.ssh".into()],
        system: false,
        unix_sockets: vec!["/agent.sock".into()],
        workspace: "/workspace".into(),
        write_nodes: vec![],
        writes: vec!["/home/.cargo".into()],
    };

    // Act
    let profile = seatbelt(&configuration).expect("profile");

    // Assert
    let denial = profile
        .find("(deny file-write* (regex #\"(^|/)")
        .expect("metadata name denial");
    let reopened = profile
        .find("(allow file-write* (subpath \"/workspace/.git\"))")
        .expect("Git writes");
    let protected = profile
        .find("(deny file-write* (regex #\"^/workspace/[.]git/((config|")
        .expect("protected Git files");
    assert!(denial < reopened && reopened < protected, "{profile}");
    // The protected entries and the directories holding them; no linked
    // worktree administration to protect.
    assert_eq!(profile.matches("(regex #\"^").count(), 2, "{profile}");
    assert!(profile.contains("(regex #\"^/workspace/[.]git(/worktrees(/[^/]+)?)?$\")"));
    assert!(profile.contains("(allow network-outbound (remote ip))"));
    assert!(profile.contains("\"/private/var/run/mDNSResponder\""));
    assert!(profile.contains("(global-name \"com.apple.TrustEvaluationAgent\")"));
    // Full-disk reads, the socket grant, and hidden paths, which come last
    // so they override the read and write grants before them.
    assert!(profile.contains("(allow file-read*)\n"));
    assert!(
        profile.contains(
            "(allow network-outbound (remote unix-socket (path-literal \"/agent.sock\")))"
        )
    );
    assert!(
        profile.ends_with("(deny file-read* file-write* (subpath \"/home/.ssh\"))\n"),
        "{profile}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_patterns_match_paths_literally() {
    // Arrange / Act / Assert
    assert_eq!(
        super::pattern(std::path::Path::new("/a.b+c?(d)[e]{f}|g$h^i\\j*k-l m")).expect("pattern"),
        "/a[.]b[+]c[?][(]d[)]\\[e\\][{]f[}][|]g[$]h\\^i\\\\j[*]k-l m"
    );
}

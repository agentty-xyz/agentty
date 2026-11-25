//! Launcher unit tests for instruction and notice channels, child
//! observation, and Seatbelt profile generation.

#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;

use rustix::io::Errno;
use rustix::process::{Pid, WaitOptions, waitpid};

#[cfg(target_os = "macos")]
use super::seatbelt;
use super::{ChildEvent, child_event, instruction, write_notice};
#[cfg(target_os = "macos")]
use crate::execution::contract::ReadScope;
#[cfg(target_os = "macos")]
use crate::execution::wire::Launch;
use crate::execution::wire::Notice;

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
        git_metadata: vec![],
        hidden_paths: vec![],
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_scope: ReadScope::Granted,
        system: false,
        unix_sockets: vec![],
        workspace: "/workspace".into(),
        write_file_nodes: vec![],
        write_files: vec![],
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
    configuration.hidden_paths.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.hidden_paths.clear();
    configuration.external_reads.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.external_reads.clear();
    configuration.writes.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.writes.clear();
    configuration.write_files.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.write_files.clear();
    for git in [invalid.into(), "/repo/.git\"".into(), "/repo/.git\n".into()] {
        configuration.git_metadata = vec![git];
        assert!(seatbelt(&configuration).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_git_metadata_and_network_rules_follow_the_write_grants() {
    // Arrange
    let configuration = Launch {
        arguments: vec![],
        directory: "/workspace".into(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        git_metadata: vec![
            "/workspace/.git".into(),
            "/repo/.git/worktrees/session".into(),
        ],
        hidden_paths: vec!["/sessions".into()],
        host_information: true,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: true,
        read_scope: ReadScope::FullDisk,
        system: false,
        unix_sockets: vec!["/agent.sock".into()],
        workspace: "/workspace".into(),
        write_file_nodes: vec![],
        write_files: vec![],
        write_nodes: vec![],
        writes: vec!["/sessions/own".into()],
    };

    // Act
    let profile = seatbelt(&configuration).expect("profile");

    // Assert
    // Each Git metadata path is denied, in any letter case and whether or
    // not it exists, after every write grant; nothing reopens it, and no
    // rule denies `.git` names elsewhere, such as in the temporary
    // directory.
    let writable = profile
        .find("(allow file-read* file-write* (subpath \"/sessions/own\"))")
        .expect("writable directory");
    let workspace_git = profile
        .find(
            "(deny file-write* (regex \
             #\"^/[wW][oO][rR][kK][sS][pP][aA][cC][eE]/[.][gG][iI][tT](/|$)\"))",
        )
        .expect("workspace Git entry");
    let administration = profile
        .find("(deny file-write* (regex #\"^/[rR][eE][pP][oO]/[.][gG][iI][tT]/")
        .expect("linked worktree administration");
    assert!(
        writable < workspace_git && workspace_git < administration,
        "{profile}"
    );
    assert_eq!(profile.matches("(regex ").count(), 2, "{profile}");
    assert!(!profile.contains("(allow file-write*"), "{profile}");
    assert!(profile.contains("(allow network-outbound (remote ip))"));
    assert!(profile.contains("\"/private/var/run/mDNSResponder\""));
    assert!(profile.contains("(global-name \"com.apple.TrustEvaluationAgent\")"));
    // The keychain service stays denied, so credentials stay out of reach.
    assert!(!profile.contains("com.apple.SecurityServer"), "{profile}");
    assert!(
        profile.contains(
            "(allow network-outbound (remote unix-socket (path-literal \"/agent.sock\")))"
        )
    );
    // Hidden paths follow the full-disk and metadata reads they override,
    // and the writable directories inside them follow the hidden paths.
    let full_disk = profile
        .find("(allow file-read*)\n")
        .expect("full-disk reads");
    let metadata = profile
        .find("(allow sysctl-read file-read-metadata)")
        .expect("host information");
    let hidden = profile
        .find("(deny file-read* file-write* (subpath \"/sessions\"))")
        .expect("hidden path");
    assert!(
        full_disk < hidden && metadata < hidden && hidden < writable,
        "{profile}"
    );
    // The grant's own path stays in place, after every rule that could
    // reopen it.
    let pinned = profile
        .find("(deny file-write-unlink file-write-create (literal \"/sessions/own\"))")
        .expect("pinned writable directory");
    assert!(writable < pinned && administration < pinned, "{profile}");
}

#[cfg(target_os = "macos")]
#[test]
fn write_grants_must_keep_their_validated_identity() {
    // Arrange
    let directory = tempfile::tempdir().expect("grant");
    let grant = directory.path().canonicalize().expect("canonical");
    let metadata = std::fs::metadata(&grant).expect("metadata");
    let lock = grant.join(".package-cache");
    std::fs::write(&lock, "").expect("lock file");
    let lock_metadata = std::fs::metadata(&lock).expect("lock metadata");
    let mut configuration = Launch {
        arguments: vec![],
        directory: grant.clone(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        git_metadata: vec![],
        hidden_paths: vec![],
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_scope: ReadScope::Granted,
        system: false,
        unix_sockets: vec![],
        workspace: grant.clone(),
        write_file_nodes: vec![(lock_metadata.dev(), lock_metadata.ino())],
        write_files: vec![lock.clone()],
        write_nodes: vec![(metadata.dev(), metadata.ino())],
        writes: vec![grant.clone()],
    };

    // Act / Assert
    assert!(super::verify_write_identities(&configuration).is_ok());
    // Kept under another name, so the replacement cannot reuse its inode.
    std::fs::rename(&lock, grant.join("moved.lock")).expect("move the lock");
    std::fs::write(&lock, "").expect("another lock in its place");
    assert!(super::verify_write_identities(&configuration).is_err());
    configuration.write_files.clear();
    configuration.write_nodes = vec![(metadata.dev(), metadata.ino().wrapping_add(1))];
    assert!(super::verify_write_identities(&configuration).is_err());
    configuration.writes = vec![grant.join("missing")];
    assert!(super::verify_write_identities(&configuration).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_host_caches_keep_their_directories_and_lock_files_in_place() {
    // Arrange
    let configuration = Launch {
        arguments: vec![],
        directory: "/workspace".into(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        git_metadata: vec![],
        hidden_paths: vec![],
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_scope: ReadScope::Granted,
        system: false,
        unix_sockets: vec![],
        workspace: "/workspace".into(),
        write_file_nodes: vec![(1, 2)],
        write_files: vec!["/home/user/.cargo/.package-cache".into()],
        write_nodes: vec![],
        writes: vec!["/home/user/.cargo/registry".into()],
    };

    // Act
    let profile = seatbelt(&configuration).expect("profile");

    // Assert
    // A host cache file is opened for its contents only: no unlink,
    // rename, or creation in its place.
    assert!(
        profile.contains(
            "(allow file-read* file-write-data (literal \"/home/user/.cargo/.package-cache\"))"
        ),
        "{profile}"
    );
    // Host cache directories cannot be removed, renamed, or replaced with a
    // link, as for every write grant.
    let granted = profile
        .find("(allow file-read* file-write* (subpath \"/home/user/.cargo/registry\"))")
        .expect("host cache grant");
    let pinned = profile
        .find("(deny file-write-unlink file-write-create (literal \"/home/user/.cargo/registry\"))")
        .expect("host cache root denial");
    assert!(granted < pinned, "{profile}");
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_patterns_match_paths_literally_in_any_letter_case() {
    // Arrange / Act / Assert
    assert_eq!(
        super::caseless_pattern(std::path::Path::new("/a.B+0?(_)[-]{ }|$^\\*é")).expect("pattern"),
        "/[aA][.][bB][+]0[?][(]_[)]\\[-\\][{] [}][|][$]\\^\\\\[*]é"
    );
}

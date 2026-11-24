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

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::prepare_branch_directories;
#[cfg(target_os = "macos")]
use super::seatbelt;
use super::{ChildEvent, child_event, instruction, write_notice};
#[cfg(target_os = "macos")]
use crate::execution::contract::ReadScope;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::execution::wire::GitWrites;
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
        git_writes: None,
        hidden_paths: vec![],
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_scope: ReadScope::Granted,
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
    configuration.hidden_paths.push(invalid.clone().into());
    assert!(seatbelt(&configuration).is_err());
    configuration.hidden_paths.clear();
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
            branch: "wt/session".into(),
            common,
        });
        assert!(seatbelt(&configuration).is_err());
    }
    configuration.git_writes = Some(GitWrites {
        admin: None,
        branch: "wt/session\"".into(),
        common: "/repo/.git".into(),
    });
    assert!(seatbelt(&configuration).is_err());
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
        git_metadata: vec!["/workspace/.git".into()],
        git_writes: Some(GitWrites {
            admin: None,
            branch: "wt/session.1".into(),
            common: "/workspace/.git".into(),
        }),
        hidden_paths: vec!["/sessions".into()],
        host_information: true,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: true,
        read_scope: ReadScope::FullDisk,
        system: false,
        unix_sockets: vec!["/agent.sock".into()],
        workspace: "/workspace".into(),
        write_nodes: vec![],
        writes: vec!["/sessions/own".into()],
    };

    // Act
    let profile = seatbelt(&configuration).expect("profile");

    // Assert
    let denial = profile
        .find("(deny file-write* (regex #\"(^|/)")
        .expect("metadata name denial");
    // A main worktree reopens the state files at the top of its Git
    // directory, its own reflog and per-worktree references, the object
    // store, and only the granted branch and its reflog, then denies the
    // protected entries, packed references included.
    let reopened = profile
        .find("(allow file-write* (regex #\"^/workspace/[.]git/((index|next-index-[0-9]+|")
        .expect("main worktree state");
    assert!(
        profile.contains(
            "|BISECT_[A-Z_]+)([.]lock)?|logs/HEAD|refs/(bisect|worktree|rewritten)(/.*)?)$\"))"
        ),
        "{profile}"
    );
    assert!(!profile.contains("[^/]+|"), "{profile}");
    let objects = profile
        .find("(allow file-write* (subpath \"/workspace/.git/objects\"))")
        .expect("object store");
    let branch = profile
        .find(
            "(allow file-write* (regex \
             #\"^/workspace/[.]git/(logs/)?refs/heads/wt/session[.]1([.]lock)?$\"))",
        )
        .expect("branch reference");
    let protected = profile
        .find(
            "(deny file-write* (regex \
             #\"^/workspace/[.]git/((config|hooks|modules|packed-refs|reftable|",
        )
        .expect("protected Git files");
    assert!(
        denial < reopened && reopened < objects && objects < branch && branch < protected,
        "{profile}"
    );
    assert!(
        !profile.contains("(allow file-write* (subpath \"/workspace/.git\"))"),
        "{profile}"
    );
    // The main worktree's state, the branch, the protected entries, and the
    // directories holding them; no linked worktree administration.
    assert_eq!(profile.matches("(regex #\"^").count(), 4, "{profile}");
    assert!(
        profile.contains(
            "(regex #\"^/workspace/[.]git(/(worktrees(/[^/]+)?|refs|objects(/info)?))?$\")"
        )
    );
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
    let writable = profile
        .find("(allow file-read* file-write* (subpath \"/sessions/own\"))")
        .expect("writable directory");
    assert!(
        full_disk < hidden && metadata < hidden && hidden < writable,
        "{profile}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_git_writes_in_a_linked_worktree_open_its_administration_and_branch_only() {
    // Arrange
    let configuration = Launch {
        arguments: vec![],
        directory: "/workspace".into(),
        environment: BTreeMap::new(),
        executable: "/bin/bash".into(),
        external_reads: vec![],
        git_metadata: vec![],
        git_writes: Some(GitWrites {
            admin: Some("/repo/.git/worktrees/session".into()),
            branch: "session".into(),
            common: "/repo/.git".into(),
        }),
        hidden_paths: vec![],
        host_information: false,
        launcher: "/launcher".into(),
        linux_bubblewrap: None,
        network: false,
        read_scope: ReadScope::Granted,
        system: false,
        unix_sockets: vec![],
        workspace: "/workspace".into(),
        write_nodes: vec![],
        writes: vec![],
    };

    // Act
    let profile = seatbelt(&configuration).expect("profile");

    // Assert
    assert!(
        profile.contains("(allow file-write* (subpath \"/repo/.git/worktrees/session\"))"),
        "{profile}"
    );
    assert!(
        profile.contains(
            "(allow file-write* (regex #\"^/repo/[.]git/(logs/)?refs/heads/session([.]lock)?$\"))"
        ),
        "{profile}"
    );
    // The common directory's top-level files belong to the main worktree.
    assert!(!profile.contains("|logs/HEAD|"), "{profile}");
    assert!(
        !profile.contains("(allow file-write* (subpath \"/repo/.git\"))"),
        "{profile}"
    );
}

#[test]
fn branch_directories_are_created_without_following_links_or_files() {
    // Arrange
    let directory = tempfile::tempdir().expect("repository");
    let common = directory.path().canonicalize().expect("canonical");
    let outside = common.join("outside");
    std::fs::create_dir_all(common.join("refs/heads")).expect("references");
    std::fs::create_dir(&outside).expect("outside");
    std::os::unix::fs::symlink(&outside, common.join("refs/heads/linked")).expect("link");
    let git = |common: &std::path::Path, branch: &str| GitWrites {
        admin: None,
        branch: branch.into(),
        common: common.to_path_buf(),
    };
    let reftable = common.join("reftable-repository");
    std::fs::create_dir_all(reftable.join("refs")).expect("reftable references");
    std::fs::write(reftable.join("refs/heads"), "").expect("reftable heads file");

    // Act
    let nested = prepare_branch_directories(&git(&common, "wt/a/b")).expect("nested branch");
    let top = prepare_branch_directories(&git(&common, "main")).expect("top-level branch");
    let linked = prepare_branch_directories(&git(&common, "linked/x")).expect("linked directory");
    let files = prepare_branch_directories(&git(&reftable, "wt/x")).expect("reftable repository");
    let missing = prepare_branch_directories(&git(&common.join("missing/repository"), "wt/x"));

    // Assert
    assert_eq!(
        nested,
        [
            common.join("refs/heads/wt/a"),
            common.join("logs/refs/heads/wt/a")
        ]
    );
    assert!(common.join("logs/refs/heads/wt/a").is_dir());
    assert_eq!(
        top,
        [common.join("refs/heads"), common.join("logs/refs/heads")]
    );
    assert_eq!(linked, [common.join("logs/refs/heads/linked")]);
    assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
    assert_eq!(files, [reftable.join("logs/refs/heads/wt")]);
    assert!(missing.is_err());
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

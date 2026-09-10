use super::*;

fn stale_archive(folder: &Path, name: &str) -> PathBuf {
    let path = folder.join(name);
    std::fs::create_dir(&path).expect("archive");
    std::fs::write(path.join(".gitignore"), "*\n").expect("marker");
    std::fs::write(path.join("history.md"), "complete private history").expect("history");

    path
}

/// Leaves a genuinely registered archive with no live process lock.
fn orphaned_archive(folder: &Path) -> PathBuf {
    let mut context = ReplayContext::archive(folder, &"private history".repeat(4096))
        .expect("registered archive");
    let archive = context.archive.take().expect("archive guard").keep();
    context
        .ownership
        .take()
        .expect("ownership guard")
        .keep()
        .expect("persist ownership");
    drop(context.lease.take());

    archive
}

#[tokio::test]
async fn cleanup_preserves_live_archives_symlinks_and_unrelated_data() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let live = ReplayContext::prepare(folder.path().to_owned(), Some("x".repeat(40000)))
        .await
        .expect("live archive");
    let live_path = std::fs::read_dir(folder.path())
        .expect("archives")
        .next()
        .expect("live archive")
        .expect("entry")
        .path();
    let stale = orphaned_archive(folder.path());
    let partial = orphaned_archive(folder.path());
    std::fs::remove_file(partial.join("history.md")).expect("interrupted cleanup");
    let unrelated = stale_archive(folder.path(), "unrelated");
    let extra = orphaned_archive(folder.path());
    std::fs::write(extra.join("user.txt"), "preserve").expect("user data");
    let wrong = orphaned_archive(folder.path());
    std::fs::write(wrong.join(".gitignore"), "user rule").expect("user marker");
    let empty = folder.path().join(".agentty-replay-empty");
    std::fs::create_dir(&empty).expect("empty directory");
    let linked = folder.path().join(".agentty-replay-linked");
    std::os::unix::fs::symlink(&unrelated, &linked).expect("directory link");
    let linked_file = stale_archive(folder.path(), ".agentty-replay-linked-file");
    std::fs::remove_file(linked_file.join("history.md")).expect("remove fixture");
    std::os::unix::fs::symlink(unrelated.join("history.md"), linked_file.join("history.md"))
        .expect("file link");
    std::fs::write(folder.path().join(".agentty-replay-file"), "preserve").expect("file");

    // Act
    super::super::cleanup_session_worktree_artifacts(folder.path()).expect("cleanup");

    // Assert
    assert!(!stale.exists());
    assert!(!partial.exists());
    assert!(live_path.join("history.md").exists());
    assert!(live.reference.is_some());
    for path in [unrelated, extra, wrong, empty, linked, linked_file] {
        assert!(
            path.exists(),
            "unrelated or incomplete entry survives: {path:?}"
        );
    }
    assert!(folder.path().join(".agentty-replay-file").exists());
}

#[test]
fn cleanup_requires_external_ownership_bound_to_the_original_directory() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let registered = orphaned_archive(folder.path());
    let token = std::fs::read(registered.join(".agentty-owner")).expect("ownership token");
    let ownership_path = folder
        .path()
        .parent()
        .expect("managed root")
        .join(std::str::from_utf8(&token).expect("record filename"));
    let lookalike = stale_archive(folder.path(), ".agentty-replay-user-data");
    let copied_token = stale_archive(folder.path(), ".agentty-replay-copied-token");
    std::fs::write(copied_token.join(".agentty-owner"), &token).expect("copied marker");
    let replaced = orphaned_archive(folder.path());
    let replaced_token = std::fs::read(replaced.join(".agentty-owner")).expect("marker");
    let moved = folder.path().join(".agentty-replay-moved");
    std::fs::rename(&replaced, &moved).expect("keep original inode alive");
    stale_archive(
        folder.path(),
        replaced
            .file_name()
            .expect("archive name")
            .to_str()
            .expect("UTF-8 archive name"),
    );
    std::fs::write(replaced.join(".agentty-owner"), replaced_token).expect("stale token");
    let forged = stale_archive(folder.path(), ".agentty-replay-forged");
    let fake_record = format!(".agentty-replay-owner-{}", uuid::Uuid::new_v4());
    let metadata = std::fs::metadata(&forged).expect("directory identity");
    std::fs::write(
        folder.path().join(&fake_record),
        serde_json::to_vec(&ReplayOwnership {
            archive: forged.canonicalize().expect("canonical archive"),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
        .expect("forged record"),
    )
    .expect("repository-controlled record");
    std::fs::write(forged.join(".agentty-owner"), fake_record).expect("forged marker");

    // Act
    cleanup_session_worktree_artifacts(folder.path()).expect("cleanup");

    // Assert
    assert!(!registered.exists());
    assert!(!ownership_path.exists());
    for path in [lookalike, copied_token, replaced, moved, forged] {
        assert!(
            path.join("history.md").exists(),
            "unowned history survives: {path:?}"
        );
    }
}

#[test]
fn cleanup_preserves_invalid_ownership_markers_and_records() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let directory_marker = orphaned_archive(folder.path());
    std::fs::remove_file(directory_marker.join(".agentty-owner")).expect("remove marker");
    std::fs::create_dir(directory_marker.join(".agentty-owner")).expect("directory marker");
    let lease = ReplayContext::open_directory(&directory_marker).expect("archive directory");
    let mut preserved = vec![directory_marker.clone()];
    for marker in [
        b"../record".to_vec(),
        b"not-an-owner".to_vec(),
        vec![b'x'; 128],
        vec![255],
    ] {
        let archive = orphaned_archive(folder.path());
        std::fs::write(archive.join(".agentty-owner"), marker).expect("invalid marker");
        preserved.push(archive);
    }
    for record_kind in ["invalid-json", "directory", "symlink"] {
        let archive = orphaned_archive(folder.path());
        let marker = std::fs::read_to_string(archive.join(".agentty-owner")).expect("token");
        let record = folder.path().parent().expect("managed root").join(marker);
        std::fs::remove_file(&record).expect("remove original record");
        match record_kind {
            "directory" => std::fs::create_dir(&record).expect("record directory"),
            "symlink" => std::os::unix::fs::symlink(archive.join("history.md"), &record)
                .expect("record link"),
            _ => std::fs::write(&record, "not JSON").expect("invalid record"),
        }
        preserved.push(archive);
    }

    // Act
    let non_file_marker = ReplayOwnership::verify(&directory_marker, &lease).expect("inspection");
    cleanup_session_worktree_artifacts(folder.path()).expect("cleanup");

    // Assert
    assert!(non_file_marker.is_none());
    for archive in preserved {
        assert!(archive.join("history.md").exists());
    }
}

#[test]
fn ownership_rejects_paths_without_a_managed_parent() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let context = ReplayContext::archive(folder.path(), &"history".repeat(INLINE_HISTORY_BYTES))
        .expect("registered archive");
    let lease = context.lease.as_ref().expect("archive lease");
    let root = folder.path().ancestors().last().expect("filesystem root");
    let root_child = folder
        .path()
        .ancestors()
        .find(|path| path.parent() == Some(root))
        .expect("directory directly below root");

    // Act
    let registration = ReplayOwnership::register(root, folder.path(), lease);
    let missing_worktree = ReplayOwnership::verify(root, lease);
    let missing_parent = ReplayOwnership::verify(root_child, lease);

    // Assert
    assert_eq!(
        registration
            .expect_err("reject parentless worktree")
            .to_string(),
        "worktree has no parent"
    );
    assert_eq!(
        missing_worktree
            .expect_err("reject parentless archive")
            .to_string(),
        "archive has no worktree"
    );
    assert_eq!(
        missing_parent
            .expect_err("reject worktree at root")
            .to_string(),
        "worktree has no parent"
    );
}

#[test]
fn cleanup_preserves_history_when_ignore_marker_is_missing() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let archive = orphaned_archive(folder.path());
    let history = std::fs::read(archive.join("history.md")).expect("history");
    std::fs::remove_file(archive.join(".gitignore")).expect("remove ignore marker");

    // Act
    let result = cleanup_session_worktree_artifacts(folder.path());

    // Assert
    assert!(result.is_ok());
    assert_eq!(
        std::fs::read(archive.join("history.md")).expect("preserved history"),
        history
    );
    assert!(archive.join(".agentty-owner").exists());
}

#[test]
fn cleanup_recovers_archive_after_exit_without_drop() {
    // Arrange
    const CHILD_FOLDER: &str = "AGENTTY_REPLAY_EXIT_FIXTURE";
    if let Some(folder) = std::env::var_os(CHILD_FOLDER) {
        let context =
            ReplayContext::archive(Path::new(&folder), &"full history after crash".repeat(4096))
                .expect("child archive");
        // Model abrupt termination by retaining the guards and process
        // lock until exit. Returning lets coverage counters flush without
        // running the archive's destructors.
        std::mem::forget(context);

        return;
    }

    let folder = tempfile::tempdir().expect("workspace");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "agent::replay::tests::cleanup_recovers_archive_after_exit_without_drop",
        ])
        .env(CHILD_FOLDER, folder.path());
    // Act
    let result = child.output().expect("child process");
    let orphan = std::fs::read_dir(folder.path())
        .expect("archives")
        .next()
        .expect("orphaned archive")
        .expect("entry")
        .path();
    let history = std::fs::read_to_string(orphan.join("history.md")).expect("orphaned history");
    super::super::cleanup_session_worktree_artifacts(folder.path()).expect("recovery");

    // Assert
    assert!(result.status.success(), "{result:?}");
    assert_eq!(history, "full history after crash".repeat(4096));
    assert!(!orphan.exists());
}

#[test]
fn cleanup_handles_missing_paths_and_reports_invalid_roots() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let missing = folder.path().join("gone");
    let file = folder.path().join("file");
    std::fs::write(&file, "not a directory").expect("fixture");

    // Act
    let missing_root = ReplayContext::cleanup_stale(&missing);
    let vanished_archive = ReplayContext::cleanup_archive(&missing);
    let invalid_root = super::super::cleanup_session_worktree_artifacts(&file);

    // Assert
    assert!(missing_root.is_ok());
    assert!(vanished_archive.is_ok());
    assert!(invalid_root.is_err());
}

#[tokio::test]
async fn short_and_absent_history_need_no_filesystem() {
    // Arrange
    let missing_folder = PathBuf::from("missing-replay-test-folder");

    // Act
    let absent = ReplayContext::prepare(missing_folder.clone(), None)
        .await
        .expect("replay fixture should succeed");
    let short = ReplayContext::prepare(missing_folder, Some("previous work".into()))
        .await
        .expect("replay fixture should succeed");

    // Assert
    assert_eq!(absent.text, None);
    assert_eq!(short.text.as_deref(), Some("previous work"));
    assert!(short.reference.is_none());
}

#[tokio::test]
async fn archive_preserves_unicode_middle_and_cleans_up_on_drop() {
    // Arrange
    let folder = tempfile::tempdir().expect("replay fixture should succeed");
    let transcript = format!(
        "original objective\n{}\naccepted decision\n{}\nchecks and remaining work",
        "界".repeat(INLINE_HISTORY_BYTES),
        "é".repeat(INLINE_HISTORY_BYTES)
    );

    // Act
    let context = ReplayContext::prepare(folder.path().to_owned(), Some(transcript.clone()))
        .await
        .expect("replay fixture should succeed");
    let archive_path = std::fs::read_dir(folder.path())
        .expect("workspace entries")
        .next()
        .expect("archive entry")
        .expect("archive metadata")
        .path();
    let history = std::fs::read_to_string(archive_path.join("history.md"))
        .expect("replay fixture should succeed");
    let text = context
        .text
        .as_ref()
        .expect("replay fixture should succeed");
    let ownership_path = context
        .ownership
        .as_ref()
        .expect("ownership guard")
        .path()
        .to_owned();

    // Assert
    assert_eq!(history, transcript);
    assert!(text.len() < INLINE_HISTORY_BYTES + 1024);
    assert!(text.contains("original objective"));
    assert!(text.contains("checks and remaining work"));
    assert!(!text.contains("accepted decision"));
    assert!(text.contains("`.agentty-replay-"));
    drop(context);
    assert!(!archive_path.exists());
    assert!(!ownership_path.exists());
}

#[tokio::test]
async fn live_archive_is_excluded_from_git() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet", "--template="])
        .arg(folder.path())
        .output()
        .expect("initialize fixture repository");
    assert!(initialized.status.success());
    std::fs::write(folder.path().join("control.txt"), "visible").expect("control file");

    // Act
    let context = ReplayContext::prepare(
        folder.path().to_owned(),
        Some("history".repeat(INLINE_HISTORY_BYTES)),
    )
    .await
    .expect("archive");
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "core.excludesFile=/dev/null",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ])
        .current_dir(folder.path())
        .output()
        .expect("inspect fixture status");

    // Assert
    assert!(context.reference.is_some());
    assert!(status.status.success());
    assert_eq!(
        String::from_utf8(status.stdout).expect("Git status"),
        "?? control.txt\n"
    );
}

#[tokio::test]
async fn marker_write_failure_preserves_history_and_guard_cleanup() {
    // Arrange
    let folder = tempfile::tempdir().expect("workspace");
    let transcript = "private history".repeat(INLINE_HISTORY_BYTES);
    let context = ReplayContext::prepare(folder.path().to_owned(), Some(transcript.clone()))
        .await
        .expect("live archive");
    let archive = context
        .archive
        .as_ref()
        .expect("archive guard")
        .path()
        .to_owned();
    let ownership = context
        .ownership
        .as_ref()
        .expect("ownership guard")
        .path()
        .to_owned();
    let marker = archive.join(".agentty-owner");
    std::fs::remove_file(&marker).expect("remove marker");
    std::fs::create_dir(&marker).expect("block marker write");

    // Act
    let result = ReplayContext::write_archive_files(
        &archive,
        ownership.file_name().expect("record name"),
        "replacement history",
    );
    let history = std::fs::read_to_string(archive.join("history.md")).expect("original history");
    drop(context);

    // Assert
    assert_eq!(
        result.expect_err("marker write must fail").kind(),
        io::ErrorKind::IsADirectory
    );
    assert_eq!(history, transcript);
    assert!(!archive.exists());
    assert!(!ownership.exists());
}

#[tokio::test]
async fn archive_failure_does_not_silently_lose_history() {
    // Arrange
    let folder = tempfile::tempdir().expect("replay fixture should succeed");
    let missing_folder = folder.path().join("missing");

    // Act
    let result =
        ReplayContext::prepare(missing_folder, Some("x".repeat(INLINE_HISTORY_BYTES + 1))).await;

    // Assert
    assert!(result.is_err());
}

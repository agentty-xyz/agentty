//! Public diff capture stays machine-readable under Git presentation settings.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use ag_git::{DiffFile, GitClient, RealGitClient};
use tempfile::{TempDir, tempdir};

#[tokio::test]
async fn remote_branch_commits_support_restricted_fetches_and_unfetched_targets()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let publisher = changed_source_repository()?;
    let remote = tempdir()?;
    let remote_path = remote.path().to_str().expect("remote path");
    run_git(publisher.path(), &["clone", "--bare", ".", remote_path])?;
    let consumer = tempdir()?;
    run_git(
        consumer.path(),
        &[
            "clone",
            "--single-branch",
            "--branch",
            "main",
            "--no-local",
            remote_path,
            "worktree",
        ],
    )?;
    let repository = consumer.path().join("worktree");
    run_git(publisher.path(), &["checkout", "-b", "release"])?;
    commit_capture_source(publisher.path())?;
    let base = run_git(publisher.path(), &["rev-parse", "HEAD"])?;
    run_git(publisher.path(), &["push", remote_path, "release"])?;
    run_git(&repository, &["checkout", "-b", "review"])?;
    fs::write(repository.join("review.txt"), "published review change\n")?;
    commit_capture_source(&repository)?;
    let source = run_git(&repository, &["rev-parse", "HEAD"])?;
    run_git(
        &repository,
        &["push", "--set-upstream", "origin", "HEAD:refs/heads/review"],
    )?;
    fs::write(repository.join("local.txt"), "unpublished local change\n")?;
    commit_capture_source(&repository)?;
    run_git(&repository, &["config", "remote.origin.prune", "true"])?;
    run_git(&repository, &["config", "remote.origin.pruneTags", "true"])?;
    run_git(&repository, &["tag", "local-note"])?;
    let tracking = run_git(
        &repository,
        &["for-each-ref", "--format=%(refname)", "refs/remotes/"],
    )?;
    assert!(!tracking.contains("origin/review"));
    assert!(!tracking.contains("origin/release"));
    let refspec = run_git(&repository, &["config", "--get-all", "remote.origin.fetch"])?;
    assert_eq!(refspec.trim(), "+refs/heads/main:refs/remotes/origin/main");
    let index = fs::read(repository.join(".git/index"))?;
    fs::write(
        repository.join(".git/FETCH_HEAD"),
        "preserve existing fetch state\n",
    )?;
    let client = RealGitClient;

    assert!(
        client
            .ref_hash(repository.clone(), base.trim().to_string())
            .await
            .is_err()
    );

    // Act
    let (base_commit, source_commit) = client
        .fetch_remote_branch_commits(
            repository.clone(),
            "origin".into(),
            "release".into(),
            "review".into(),
        )
        .await?;
    let captured = client
        .committed_diff(
            repository.clone(),
            base_commit.clone(),
            source_commit.clone(),
        )
        .await?;

    // Assert
    assert_eq!(base_commit, base.trim());
    assert_eq!(source_commit, source.trim());
    assert!(captured.contains("+published review change"));
    assert!(!captured.contains("unpublished local change"));
    assert_eq!(
        run_git(
            &repository,
            &["for-each-ref", "--format=%(refname)", "refs/remotes/"]
        )?,
        tracking
    );
    assert_eq!(
        run_git(&repository, &["config", "--get-all", "remote.origin.fetch"])?,
        refspec
    );
    assert_eq!(fs::read(repository.join(".git/index"))?, index);
    assert_eq!(
        run_git(&repository, &["tag", "--list"])?.trim(),
        "local-note"
    );
    assert_eq!(
        fs::read_to_string(repository.join(".git/FETCH_HEAD"))?,
        "preserve existing fetch state\n"
    );

    Ok(())
}

#[tokio::test]
async fn committed_diff_excludes_local_commits_staged_unstaged_and_untracked_edits()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = changed_source_repository()?;
    let repository = directory.path();
    let client = RealGitClient;
    run_git(repository, &["checkout", "-b", "review"])?;
    commit_capture_source(repository)?;
    run_git(
        repository,
        &["update-ref", "refs/remotes/origin/review", "HEAD"],
    )?;
    fs::write(
        repository.join("local-only.txt"),
        "unpublished local commit\n",
    )?;
    commit_capture_source(repository)?;
    fs::write(
        repository.join("src/source file.rs"),
        "staged-only change\n",
    )?;
    run_git(repository, &["add", "src/source file.rs"])?;
    fs::write(
        repository.join("src/source file.rs"),
        "unstaged-only change\n",
    )?;
    fs::write(repository.join("untracked.txt"), "untracked-only change\n")?;
    run_git(
        repository,
        &["config", "diff.external", "missing-diff-driver"],
    )?;
    let index_before = fs::read(repository.join(".git/index"))?;
    let status_before = run_git(
        repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;

    // Act
    let captured = client
        .committed_diff(
            repository.to_path_buf(),
            "main".into(),
            "origin/review".into(),
        )
        .await?;

    // Assert
    assert!(captured.contains("+new();"));
    assert!(captured.contains("-old();"));
    assert!(!captured.contains("local-only"));
    assert!(!captured.contains("staged-only"));
    assert!(!captured.contains("unstaged-only"));
    assert!(!captured.contains("untracked"));
    assert_eq!(fs::read(repository.join(".git/index"))?, index_before);
    assert_eq!(
        run_git(
            repository,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        )?,
        status_before
    );

    // Arrange, Act, Assert
    for (base, source) in [("missing", "origin/review"), ("main", "missing")] {
        assert!(
            client
                .committed_diff(repository.to_path_buf(), base.into(), source.into())
                .await
                .is_err()
        );
    }

    Ok(())
}

fn commit_capture_source(repository: &Path) -> Result<(), Box<dyn Error>> {
    run_git(repository, &["add", "."])?;
    run_git(
        repository,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "source snapshot",
        ],
    )?;

    Ok(())
}

#[tokio::test]
async fn captured_diff_detects_same_size_edits_with_racy_index_timestamps()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = changed_source_repository()?;
    let repository = directory.path();
    let source_path = repository.join("src/source file.rs");
    let index_path = repository.join(".git/index");
    run_git(repository, &["config", "core.trustctime", "false"])?;
    run_git(repository, &["config", "core.checkstat", "minimal"])?;
    fs::write(&source_path, "context\n\nold();\n\ntail\n")?;
    let source = fs::File::options().write(true).open(&source_path)?;
    source.set_times(
        fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(60)),
    )?;
    run_git(repository, &["add", "src/source file.rs"])?;
    let modified = source.metadata()?.modified()?;
    fs::File::options()
        .write(true)
        .open(&index_path)?
        .set_times(fs::FileTimes::new().set_modified(modified))?;
    fs::write(&source_path, "context\n\nnew();\n\ntail\n")?;
    source.set_times(fs::FileTimes::new().set_modified(modified))?;
    let index_before = fs::read(&index_path)?;
    let client = RealGitClient;

    // Act
    let changed = client
        .diff_changed_files(repository.to_path_buf(), "main".into())
        .await?;
    let captured = client.diff(repository.to_path_buf(), "main".into()).await?;

    // Assert
    assert_eq!(changed, ["src/source file.rs"]);
    let files = DiffFile::parse(&captured);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].source_ranges("new();", false), [(3, 3)]);
    assert_eq!(files[0].source_ranges("old();", true), [(3, 3)]);
    assert_eq!(fs::read(&index_path)?, index_before);
    assert_eq!(fs::metadata(&index_path)?.modified()?, modified);

    Ok(())
}

#[tokio::test]
async fn captured_diff_overrides_blank_context_color_and_configurable_prefixes()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = changed_source_repository()?;
    let repository = directory.path();
    run_git(repository, &["config", "diff.suppressBlankEmpty", "true"])?;
    run_git(repository, &["config", "color.ui", "always"])?;
    let client = RealGitClient;

    for (mnemonic, no_prefix, source_prefix, destination_prefix) in [
        ("true", "false", "a/", "b/"),
        ("false", "true", "a/", "b/"),
        ("true", "true", "custom-old/", "custom-new/"),
    ] {
        run_git(repository, &["config", "diff.mnemonicPrefix", mnemonic])?;
        run_git(repository, &["config", "diff.noprefix", no_prefix])?;
        run_git(repository, &["config", "diff.srcPrefix", source_prefix])?;
        run_git(
            repository,
            &["config", "diff.dstPrefix", destination_prefix],
        )?;
        let status_before = run_git(repository, &["status", "--porcelain=v1"])?;

        // Act
        let captured = client.diff(repository.to_path_buf(), "main".into()).await?;
        let changed = client
            .diff_changed_files(repository.to_path_buf(), "main".into())
            .await?;
        let files = DiffFile::parse(&captured);

        // Assert
        assert!(captured.starts_with("diff --git a/src/source file.rs b/src/source file.rs\n"));
        assert!(!captured.contains('\u{1b}'));
        assert!(!captured.lines().any(str::is_empty));
        assert_eq!(captured.lines().filter(|line| *line == " ").count(), 2);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].old_path, "src/source file.rs");
        assert_eq!(files[0].new_path, "src/source file.rs");
        assert_eq!(files[0].source_ranges("new();", false), [(3, 3)]);
        assert_eq!(files[0].source_ranges("old();", true), [(3, 3)]);
        assert_eq!(changed, ["src/source file.rs"]);
        assert_eq!(
            run_git(repository, &["status", "--porcelain=v1"])?,
            status_before
        );
    }

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn captured_diff_uses_original_source_coordinates_without_textconv()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = changed_source_repository()?;
    let repository = directory.path();
    fs::write(
        repository.join(".git/info/attributes"),
        "src/*.rs diff=capture-conversion\n",
    )?;
    fs::write(
        repository.join(".git/textconv.sh"),
        "printf 'called\\n' >> .git/driver-calls\nprintf 'converted prefix\\n'\ncat \"$1\"\n",
    )?;
    run_git(
        repository,
        &[
            "config",
            "diff.capture-conversion.textconv",
            "sh .git/textconv.sh",
        ],
    )?;
    let converted = run_git(
        repository,
        &[
            "-c",
            "diff.noprefix=false",
            "diff",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "main",
        ],
    )?;
    let calls_before = fs::read(repository.join(".git/driver-calls"))?;
    let client = RealGitClient;

    // Act
    let captured = client.diff(repository.to_path_buf(), "main".into()).await?;
    let changed = client
        .diff_changed_files(repository.to_path_buf(), "main".into())
        .await?;
    let files = DiffFile::parse(&captured);

    // Assert
    assert!(converted.contains(" converted prefix\n"));
    assert_eq!(
        DiffFile::parse(&converted)[0].source_ranges("new();", false),
        [(4, 4)]
    );
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].new_path, "src/source file.rs");
    assert_eq!(files[0].source_ranges("new();", false), [(3, 3)]);
    assert_eq!(files[0].source_ranges("old();", true), [(3, 3)]);
    assert!(!captured.contains("converted prefix"));
    assert_eq!(changed, ["src/source file.rs"]);
    assert_eq!(
        fs::read(repository.join(".git/driver-calls"))?,
        calls_before
    );

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn captured_diff_disables_attribute_and_global_external_drivers() -> Result<(), Box<dyn Error>>
{
    // Arrange
    for setting in ["diff.capture-driver.command", "diff.external"] {
        let directory = changed_source_repository()?;
        let repository = directory.path();
        fs::write(
            repository.join(".git/info/attributes"),
            "src/*.rs diff=capture-driver\n",
        )?;
        fs::write(
            repository.join(".git/external.sh"),
            "printf 'called\\n' >> .git/driver-calls\nprintf 'external driver replacement\\n'\n",
        )?;
        run_git(repository, &["config", setting, "sh .git/external.sh"])?;
        let replaced = run_git(repository, &["diff", "--no-color", "main"])?;
        let calls_before = fs::read(repository.join(".git/driver-calls"))?;
        let client = RealGitClient;

        // Act
        let captured = client.diff(repository.to_path_buf(), "main".into()).await?;
        let changed = client
            .diff_changed_files(repository.to_path_buf(), "main".into())
            .await?;
        let files = DiffFile::parse(&captured);

        // Assert
        assert_eq!(replaced, "external driver replacement\n");
        assert_eq!(DiffFile::parse(&replaced).len(), 0);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].new_path, "src/source file.rs");
        assert_eq!(files[0].source_ranges("new();", false), [(3, 3)]);
        assert!(captured.starts_with("diff --git a/src/source file.rs b/src/source file.rs\n"));
        assert!(!captured.contains("external driver replacement"));
        assert_eq!(changed, ["src/source file.rs"]);
        assert_eq!(
            fs::read(repository.join(".git/driver-calls"))?,
            calls_before
        );
    }

    Ok(())
}

fn changed_source_repository() -> Result<TempDir, Box<dyn Error>> {
    let directory = tempdir()?;
    let repository = directory.path();
    run_git(repository, &["init", "--template=", "-b", "main"])?;
    fs::create_dir_all(repository.join(".git/info"))?;
    fs::create_dir(repository.join("src"))?;
    let source = repository.join("src/source file.rs");
    fs::write(&source, "context\n\nold();\n\ntail\n")?;
    run_git(repository, &["add", "."])?;
    run_git(
        repository,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial source",
        ],
    )?;
    fs::write(&source, "context\n\nnew();\n\ntail\n")?;

    Ok(directory)
}

fn run_git(repository: &Path, arguments: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(String::from_utf8(output.stdout)?)
}

//! Public diff capture stays machine-readable under Git presentation settings.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use ag_git::{DiffFile, GitClient, RealGitClient};
use tempfile::{TempDir, tempdir};

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

//! VHS recording shared by the marketing asset generators.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::process::Command;

use crate::ShowcaseResult;

/// Fails unless the `vhs` recorder can run.
///
/// Generation is always explicit, so a missing recorder is an error rather
/// than a skipped run.
pub(crate) async fn require_installed() -> ShowcaseResult {
    let installed = Command::new("vhs")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !installed {
        return Err("VHS is required; run the generators through the `e2e-container` hook".into());
    }

    Ok(())
}

/// Records the tape built by `build_tape` and publishes it at `gif_path`.
///
/// `build_tape` receives the hidden staging GIF its `Output` must name. The
/// staging file replaces `gif_path` only after VHS succeeds with a nonempty
/// recording, so a failed run keeps the previous asset. The same-named PNG
/// poster is then removed: it shows the previous recording and must be
/// re-extracted.
pub(crate) async fn record(
    gif_path: &Path,
    build_tape: impl FnOnce(&Path) -> String,
) -> ShowcaseResult<PathBuf> {
    let (Some(directory), Some(stem)) = (gif_path.parent(), gif_path.file_stem()) else {
        return Err(format!("Invalid GIF path: {}", gif_path.display()).into());
    };
    fs::create_dir_all(directory).await?;
    let staging_path = directory.join(format!(".{}.recording.gif", stem.display()));
    let tape = tempfile::Builder::new().suffix(".tape").tempfile()?;
    fs::write(tape.path(), build_tape(&staging_path)).await?;

    let status = Command::new("vhs").arg(tape.path()).status().await?;
    let recorded = fs::metadata(&staging_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0);
    if !status.success() || !recorded {
        let _ = fs::remove_file(&staging_path).await;

        return Err(format!(
            "VHS did not record {} (exit status: {status})",
            gif_path.display()
        )
        .into());
    }
    fs::rename(&staging_path, gif_path).await?;
    match fs::remove_file(gif_path.with_extension("png")).await {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }

    Ok(gif_path.to_path_buf())
}

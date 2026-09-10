use std::path::{Path, PathBuf};

use super::super::VhsContext;
use crate::vhs::{VhsError, VhsTape, VhsTapeSettings};

pub(super) const GENERATED_GIF_BYTES: &[u8] = b"generated gif";

pub(super) fn test_vhs_context(
    settings: &VhsTapeSettings,
    check_vhs: fn() -> Result<(), VhsError>,
    execute_tape: fn(&VhsTape, &Path) -> Result<PathBuf, VhsError>,
) -> VhsContext<'_> {
    VhsContext {
        binary_path: Path::new("/usr/bin/true"),
        check_vhs,
        env_pairs: &[],
        execute_tape,
        settings,
    }
}

pub(super) fn vhs_available() -> Result<(), VhsError> {
    std::fs::metadata(".")
        .map(|_| ())
        .map_err(|err| VhsError::IoError(err.to_string()))
}

pub(super) fn vhs_unavailable() -> Result<(), VhsError> {
    Err(VhsError::NotInstalled(
        "VHS unavailable in test".to_string(),
    ))
}

pub(super) fn successful_tape_execution(
    tape: &VhsTape,
    tape_path: &Path,
) -> Result<PathBuf, VhsError> {
    stage_tape_execution(tape, tape_path, GENERATED_GIF_BYTES)
}

pub(super) fn failed_tape_execution(tape: &VhsTape, tape_path: &Path) -> Result<PathBuf, VhsError> {
    let _ = stage_tape_execution(tape, tape_path, GENERATED_GIF_BYTES)?;

    Err(VhsError::ExecutionFailed("simulated failure".to_string()))
}

pub(super) fn empty_tape_execution(tape: &VhsTape, tape_path: &Path) -> Result<PathBuf, VhsError> {
    stage_tape_execution(tape, tape_path, &[])
}

fn stage_tape_execution(
    tape: &VhsTape,
    tape_path: &Path,
    gif_bytes: &[u8],
) -> Result<PathBuf, VhsError> {
    tape.write_to(tape_path)
        .map_err(|err| VhsError::IoError(err.to_string()))?;
    std::fs::write(tape_gif_path(tape), gif_bytes)
        .map_err(|err| VhsError::IoError(err.to_string()))?;
    std::fs::write(tape.screenshot_path(), b"temporary screenshot")
        .map_err(|err| VhsError::IoError(err.to_string()))?;

    Ok(tape.screenshot_path().to_path_buf())
}

fn tape_gif_path(tape: &VhsTape) -> PathBuf {
    const OUTPUT_PREFIX: &str = "Output \"";

    let output_line = tape
        .render()
        .lines()
        .find(|line| line.starts_with(OUTPUT_PREFIX))
        .expect("tape must declare GIF output");
    let path = output_line
        .strip_prefix(OUTPUT_PREFIX)
        .and_then(|line| line.strip_suffix('"'))
        .expect("GIF output must be double quoted");

    PathBuf::from(path)
}

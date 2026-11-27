//! VHS recording shared by the marketing asset generators.
//!
//! VHS captures each tape as PNG frames, and [`record`] encodes the GIF from
//! them itself. VHS 0.11 encodes in one `ffmpeg` graph whose `split` holds
//! every decoded frame, about 5.8 MB at 1600×900, until `palettegen` has read
//! the last one, so its memory grows with the recording's length: the demo's
//! roughly 900 frames exceed a 6 GiB Podman machine. [`encode_gif`] runs the
//! same composition in two streaming passes, one that writes the palette and
//! one that applies it, which produces the same GIF in constant memory.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::process::Command;

use crate::ShowcaseResult;

/// A VHS theme with the background VHS pads its frames with.
#[derive(Clone, Copy)]
pub(crate) struct Theme {
    /// Name VHS knows the theme by.
    pub(crate) name: &'static str,
    /// The theme's `background` in VHS's `themes.json`.
    pub(crate) background: &'static str,
}

/// The `Dracula` theme of the marketing demo.
pub(crate) const DRACULA: Theme = Theme {
    name: "Dracula",
    background: "#1e1f29",
};

/// The `OneDark` theme of the feature recordings.
pub(crate) const ONE_DARK: Theme = Theme {
    name: "OneDark",
    background: "#1e2127",
};

/// The VHS settings that shape a recording's frames, shared by its tape and
/// its GIF encode.
pub(crate) struct Canvas {
    /// Terminal font size in points.
    pub(crate) font_size: u32,
    /// GIF width in pixels, padding included.
    pub(crate) width: u32,
    /// GIF height in pixels, padding included.
    pub(crate) height: u32,
    /// Theme-colored border around the terminal, in pixels.
    pub(crate) padding: u32,
    /// Frames VHS captures per second.
    pub(crate) framerate: u32,
    /// Terminal colors and the padding's background.
    pub(crate) theme: Theme,
}

impl Canvas {
    /// Returns the tape's `Set` lines for this canvas.
    fn settings(&self) -> String {
        let Self {
            font_size,
            width,
            height,
            padding,
            framerate,
            theme,
        } = self;

        format!(
            "Set FontSize {font_size}\nSet Width {width}\nSet Height {height}\nSet Padding \
             {padding}\nSet Framerate {framerate}\nSet Theme \"{}\"\n",
            theme.name
        )
    }

    /// Returns the `ffmpeg` inputs and the filter graph VHS 0.11 composes
    /// its GIF frames with, ending in the unlabeled composed stream.
    ///
    /// Inputs 0 and 1 are the text and cursor canvases at the capture rate.
    /// Input 2 is VHS's margin fill: the terminal covers it completely at
    /// margin 0, but its 25-frames-per-second timeline sets the GIF's rate.
    fn composition(&self, frames: &Path) -> (Vec<OsString>, String) {
        let Self {
            width,
            height,
            padding,
            framerate,
            theme,
            ..
        } = self;
        let background = theme.background;
        let mut inputs = Vec::<OsString>::new();
        for layer in ["text", "cursor"] {
            inputs.extend(
                ["-r", &framerate.to_string(), "-start_number", "1", "-i"].map(OsString::from),
            );
            inputs.push(frames.join(format!("frame-{layer}-%05d.png")).into());
        }
        inputs.extend(["-f".into(), "lavfi".into(), "-i".into()]);
        inputs.push(format!("color={background}:s={width}x{height}:r=25").into());
        let filters = format!(
            "[0][1]overlay,scale={}:{}:force_original_aspect_ratio=decrease,fps={framerate},\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:{background},fillborders=left={padding}:\
             right={padding}:top={padding}:bottom={padding}:mode=fixed:\
             color={background}[terminal];[2][terminal]overlay=(W-w)/2:(H-h)/2:shortest=1",
            width - 2 * padding,
            height - 2 * padding,
        );

        (inputs, filters)
    }
}

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

/// Records `tape` on `canvas` and publishes the GIF at `gif_path`.
///
/// `tape` holds every command after the canvas settings and the `Output`
/// line, which this function writes. The GIF is encoded into a hidden
/// staging file that replaces `gif_path` only after VHS succeeds and the
/// encode is nonempty, so a failed run keeps the previous asset. The
/// same-named PNG poster is then removed: it shows the previous recording and
/// must be re-extracted.
pub(crate) async fn record(
    gif_path: &Path,
    canvas: &Canvas,
    tape: &str,
) -> ShowcaseResult<PathBuf> {
    let (Some(directory), Some(stem)) = (gif_path.parent(), gif_path.file_stem()) else {
        return Err(format!("Invalid GIF path: {}", gif_path.display()).into());
    };
    fs::create_dir_all(directory).await?;
    // VHS renames its frame directory onto the `Output` directory, so keep
    // both in the system temporary directory, on one filesystem.
    let work = tempfile::TempDir::new()?;
    let frames = work.path().join("frames");
    let tape_path = work.path().join("recording.tape");
    let settings = canvas.settings();
    fs::write(
        &tape_path,
        format!("{settings}Output \"{}/\"\n{tape}", frames.display()),
    )
    .await?;

    let status = Command::new("vhs").arg(&tape_path).status().await?;
    if !status.success() || !fs::try_exists(frames.join("frame-text-00001.png")).await? {
        return Err(format!(
            "VHS did not record {} (exit status: {status})",
            gif_path.display()
        )
        .into());
    }
    let staging_path = directory.join(format!(".{}.recording.gif", stem.display()));
    let encoded = encode_gif(canvas, &frames, &staging_path).await;
    let recorded = fs::metadata(&staging_path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0);
    if encoded.is_err() || !recorded {
        let _ = fs::remove_file(&staging_path).await;
        encoded?;

        return Err(format!("ffmpeg wrote an empty {}", gif_path.display()).into());
    }
    fs::rename(&staging_path, gif_path).await?;
    match fs::remove_file(gif_path.with_extension("png")).await {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }

    Ok(gif_path.to_path_buf())
}

/// Encodes the VHS `frames` of `canvas` into `gif_path` in two passes.
///
/// Both passes stream the frames: the first reduces all of them to one
/// 256-color palette, as VHS's `palettegen` does, and the second maps every
/// frame onto it with `paletteuse`'s default dithering.
async fn encode_gif(canvas: &Canvas, frames: &Path, gif_path: &Path) -> ShowcaseResult {
    let (inputs, composition) = canvas.composition(frames);
    let palette = frames.join("palette.png");

    let mut palette_pass = inputs.clone();
    palette_pass.extend([
        "-filter_complex".into(),
        OsString::from(format!("{composition},palettegen=max_colors=256")),
    ]);
    palette_pass.extend(["-frames:v", "1", "-update", "1"].map(OsString::from));
    palette_pass.push(palette.clone().into());
    run_ffmpeg(&palette_pass).await?;

    let mut gif_pass = inputs;
    gif_pass.extend(["-i".into(), palette.into(), "-filter_complex".into()]);
    gif_pass.push(format!("{composition}[composed];[composed][3]paletteuse").into());
    gif_pass.push(gif_path.into());

    run_ffmpeg(&gif_pass).await
}

/// Runs one quiet, non-interactive `ffmpeg` pass with `arguments`.
async fn run_ffmpeg(arguments: &[OsString]) -> ShowcaseResult {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(arguments)
        .status()
        .await?;
    if !status.success() {
        return Err(format!("ffmpeg failed to encode the GIF (exit status: {status})").into());
    }

    Ok(())
}

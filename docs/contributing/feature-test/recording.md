# Recording Feature Artifacts

Committed hash sidecars must be produced by the same pinned container definition that CI
uses. `container/e2e.Containerfile` supports both `linux/amd64` and `linux/arm64`, with
architecture-specific checksums for the Rust installer, `prek`, `cargo-nextest`, `vhs`,
and `ttyd`, plus the full recording stack (Chromium, `ffmpeg`, JetBrains Mono).
Presubmit, postsubmit, and release checks call `.github/workflows/e2e.yml`, whose job
uses the published image index from GHCR as its digest-pinned container runtime, selects
the `linux/amd64` variant explicitly, overrides the image user with the root user GitHub
requires for workspace access, and runs the `test-agentty-e2e` hook directly. The same
index contains a native `linux/arm64` variant for recording on ARM64 hosts. The host
needs a running Podman environment only — no local Chrome or VHS — and the bare-host
recording restriction does not apply inside the container.

The canonical feature preset records at 1600×800 with an 18-point font, matching the
site poster dimensions and avoiding the excessive VHS and FFmpeg memory use caused by
larger canvases during long scenarios. Rendering settings participate in the freshness
hash, so changing the canvas, font, theme, framerate, or padding makes existing
recordings stale even when their captured terminal frames are unchanged.

A published digest is multi-architecture only when it resolves to an image index or
manifest list containing both `linux/amd64` and `linux/arm64`; the Containerfile's
support for both architectures does not prove that the registry reference contains both.
The hook below pulls the immutable digest with an explicit platform so a missing variant
fails instead of silently running the wrong architecture through emulation, where Rust
compiler probes can crash before the test starts.

On macOS or Windows, initialize the Podman machine once with `podman machine init`, then
start it with `podman machine start` before pulling or running the image. Linux hosts
run Podman directly without a machine.

Record or refresh feature artifacts with the `e2e-container` hook in `generate` mode,
which re-records only missing or stale GIFs:

```sh
TESTTY_GIF_MODE=generate AGENTTY_TEST_FILTER='test(<test_function>)' \
  prek run e2e-container --all-files --hook-stage manual
```

The hook runs the Agentty `e2e` target in the native variant of the image pinned by
`.github/workflows/e2e.yml`. `TESTTY_GIF_MODE` defaults to `check`, and the optional
`AGENTTY_TEST_FILTER` nextest filterset narrows the run. The container runs as the host
user so the bind-mounted workspace remains writable. A host-owned cache in
`${XDG_CACHE_HOME:-$HOME/.cache}/agentty-e2e` keeps its home, Cargo, and `prek`
directories between runs, plus one `target-<hash>` build directory per checkout path:
every checkout is mounted at `/workspace`, and Cargo's modification-time fingerprints
could otherwise reuse another checkout's binaries. Delete the directories of removed
checkouts to reclaim space. One build job without development debug symbols bounds
linker memory in smaller Podman machines while retaining runtime debug assertions. The
hook then fails if a GIF declared by a feature page is missing or empty: VHS can exit
successfully after creating its screenshots even when GIF finalization has not produced
a usable artifact.

Every local run shares that cache and the Podman machine, so the hook holds an OS lock
in the cache directory until its container exits. A concurrent run from any checkout or
session reports that it is waiting and starts after the lock is released. Run the hook
as one foreground command, or as a harness background task and wait for its completion
notice. Do not poll its output, `podman ps`, or processes, wrap it in wait loops, or
start the container by hand.

The reusable CI workflow instead overrides the image's unprivileged default user with
root so `actions/checkout` can populate the job's writable workspace; `check` mode
verifies recording freshness without rewriting the feature artifacts. Because GitHub's
mounted checkout retains a different owner, the workflow also registers the exact
`GITHUB_WORKSPACE` path as a Git `safe.directory` before invoking `prek`.

Review the changed GIF and `.{name}.hash` sidecar, then refresh the PNG poster for every
regenerated GIF using the poster procedure below before committing all three together.
Testty records to a hidden staging file and replaces the committed GIF only after the
recording is nonempty. Successful generation removes the previous same-named PNG
intentionally so a stale poster cannot pass the nonempty-poster integrity check; a
failed recording preserves the last valid GIF, hash sidecar, and poster.

## Create or Refresh the PNG Poster

Every feature GIF needs a same-named PNG poster for the site's `prefers-reduced-motion`
and `noscript` fallbacks. Create the poster when adding a GIF, and regenerate it
whenever the GIF changes. `TESTTY_GIF_MODE=check` verifies GIF freshness but does not
verify or update the poster.

First inspect the finished GIF and choose a timestamp that clearly communicates the
feature's result. Prefer a stable end state with the important UI visible. Do not
automatically use the first or midpoint frame: it may show an empty, loading, or
transitional state.

Check the GIF duration before choosing a timestamp:

```sh
ffprobe -v error \
  -show_entries format=duration \
  -of default=noprint_wrappers=1 \
  docs/site/static/features/<name>.gif
```

Extract exactly one frame with `ffmpeg`, preserving the aspect ratio and capping the
width at 1600 pixels without upscaling:

```sh
ffmpeg -y \
  -i docs/site/static/features/<name>.gif \
  -ss <timestamp> \
  -vf "scale=w='min(1600\,iw)':h=-1" \
  -frames:v 1 \
  -compression_level 9 \
  docs/site/static/features/<name>.png
```

Use a timestamp accepted by `ffmpeg`, such as `00:00:01.500`, that falls within the
reported duration. Open the resulting PNG and verify that it is sharp, legible, free of
transitional artifacts, and still represents the current GIF. If it does not, choose a
better timestamp and rerun the command.

Check that every GIF declared by a feature page has a nonempty poster before finalizing:

```sh
for feature_page in docs/site/content/features/*.md; do
  gif_name=$(sed -n 's/^gif = "\(.*\)"/\1/p' "${feature_page}")
  test -z "${gif_name}" && continue
  poster_path="docs/site/static/features/${gif_name%.gif}.png"
  test -s "${poster_path}" || {
    echo "missing PNG poster: ${poster_path}"
    exit 1
  }
done
```

## Recording Boundary

Do not run VHS recording directly in the developer host's operating-system environment.
This restriction does not prohibit running the pinned recording container on that host
with Podman; that is the supported workflow described above. Bare-host VHS records
through localhost sockets (`ttyd` plus the Chrome DevTools protocol), and sandboxed
agent shells can deny that network access and crash `vhs` before Chrome launches. On
macOS, even an unsandboxed bare-host run launches a local Chromium process that can
abort during AppKit registration and show a **Chromium quit unexpectedly** dialog. The
bare-host prohibition also covers direct `vhs` commands and ignored tests that
regenerate demo assets. Use the `e2e-container` hook above for every intentional
recording; do not suppress macOS crash reporting to hide a bare-host browser failure.

When changing the image itself, follow
`docs/contributing/feature-test/image-maintenance.md`.

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
Pull the immutable digest with an explicit platform so a missing variant fails instead
of silently running the wrong architecture through emulation.

On macOS or Windows, initialize the Podman machine once with `podman machine init`, then
start it with `podman machine start` before pulling or running the image. Linux hosts
run Podman directly without a machine.

Record or refresh feature artifacts with a writable workspace mount and `generate` mode,
which re-records only missing or stale GIFs. Run the local container as the host user so
Linux bind mounts remain writable. A host-owned cache directory provides writable home,
Cargo, `prek`, and build locations while preserving them between runs:

```sh
published_e2e_image=ghcr.io/agentty-xyz/agentty-e2e@sha256:a72f0ec28f53b2746b3b3cd224aaa1e511e555c6a1b5c2bbdfa21e79538e55f4
e2e_cache_root="${XDG_CACHE_HOME:-${HOME}/.cache}/agentty-e2e"
mkdir -p \
  "${e2e_cache_root}/home" \
  "${e2e_cache_root}/cargo" \
  "${e2e_cache_root}/prek" \
  "${e2e_cache_root}/target"

case "$(uname -m)" in
  x86_64 | amd64)
    e2e_platform=linux/amd64
    ;;
  arm64 | aarch64)
    e2e_platform=linux/arm64
    ;;
  *)
    echo "unsupported recording architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

e2e_image="${published_e2e_image}"
podman pull --platform "${e2e_platform}" "${e2e_image}"

podman run --rm \
  --platform "${e2e_platform}" \
  --user "$(id -u):$(id -g)" \
  --mount type=bind,source="$PWD",target=/workspace \
  --mount type=bind,source="${e2e_cache_root}",target=/cache \
  --env HOME=/cache/home \
  --env CARGO_HOME=/cache/cargo \
  --env PREK_HOME=/cache/prek \
  --env CARGO_TARGET_DIR=/cache/target \
  --env TESTTY_GIF_MODE=generate \
  "${e2e_image}" \
  cargo nextest run --locked --profile ci -p agentty --test e2e test_{name}

test -s docs/site/static/features/{name}.gif || {
  echo "generated feature GIF is missing or empty: {name}.gif" >&2
  exit 1
}
```

Both host branches pull their native variant from the same published image index; do not
run the other architecture through emulation because Rust compiler probes can crash
before the test starts. Local recording uses the host user's UID so the bind-mounted
workspace remains writable. The reusable CI workflow instead overrides the image's
unprivileged default user with root so `actions/checkout` can populate the job's
writable workspace; `check` mode verifies recording freshness without rewriting the
feature artifacts. Because GitHub's mounted checkout retains a different owner, the
workflow also registers the exact `GITHUB_WORKSPACE` path as a Git `safe.directory`
before invoking `prek`. Always perform the nonempty-file check after recording: VHS can
exit successfully after creating its screenshots even when GIF finalization has not
produced a usable artifact.

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
regenerate demo assets. Use the platform-explicit Podman workflow above for every
intentional recording; do not suppress macOS crash reporting to hide a bare-host browser
failure.

When changing the image itself, follow
`docs/contributing/feature-test/image-maintenance.md`.

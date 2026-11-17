# Recording Feature Artifacts

Committed hash sidecars must come from the same pinned container definition that CI
uses. `container/e2e.Containerfile` supports `linux/amd64` and `linux/arm64`, with
architecture-specific checksums for the Rust installer, `prek`, `cargo-nextest`, `vhs`,
and `ttyd`, plus the full recording stack (Chromium, `ffmpeg`, JetBrains Mono).
Presubmit, postsubmit, and release checks call `.github/workflows/e2e.yml`, which runs
the `test-agentty-e2e` hook directly in the digest-pinned GHCR image index, selecting
its `linux/amd64` variant explicitly. The same index contains a native `linux/arm64`
variant for recording on ARM64 hosts. The host needs only a running Podman environment,
not a local Chrome or VHS.

The canonical feature preset records at 1600×800 with an 18-point font, matching the
site poster dimensions and avoiding the excessive VHS and FFmpeg memory use of larger
canvases during long scenarios. Rendering settings participate in the freshness hash, so
changing the canvas, font, theme, framerate, or padding makes existing recordings stale
even when their captured terminal frames are unchanged.

A published digest is multi-architecture only when it resolves to an image index or
manifest list containing both `linux/amd64` and `linux/arm64`; Containerfile support for
both does not prove the registry reference contains both. The hook pulls the immutable
digest with an explicit platform, so a missing variant fails instead of silently running
the wrong architecture through emulation, where Rust compiler probes can crash before
the test starts.

On macOS or Windows, initialize the Podman machine once with `podman machine init`, then
start it with `podman machine start` before pulling or running the image. Linux hosts
run Podman directly without a machine.

Record or refresh feature artifacts with the `e2e-container` hook in `generate` mode,
which re-records only missing or stale GIFs:

```sh
TESTTY_GIF_MODE=generate AGENTTY_TEST_FILTER='test(<test_function>)' \
  prek run e2e-container --all-files --hook-stage manual
```

The hook runs an Agentty test target in the native variant of the image pinned by
`.github/workflows/e2e.yml`:

- `AGENTTY_TEST_TARGET` selects `e2e` (the default) or `showcase`.
- `TESTTY_GIF_MODE` defaults to `check`.
- `AGENTTY_TEST_FILTER` optionally narrows the run with a nextest filterset.
- `AGENTTY_RUN_IGNORED=only` runs only the target's ignored tests, which are the asset
  generators below.
- `AGENTTY_E2E_IMAGE` replaces the pinned image with a local candidate; see
  `docs/contributing/feature-test/image-maintenance.md`.

Any other `AGENTTY_RUN_IGNORED` value or test target fails before a container starts.
The container runs as the host user so the bind-mounted workspace remains writable, with
one build job and no development debug symbols to bound linker memory in smaller Podman
machines while retaining runtime debug assertions. The hook then fails if a GIF declared
by a feature page is missing or empty: VHS can exit successfully after creating its
screenshots even when GIF finalization has not produced a usable artifact.

A host-owned cache in `${XDG_CACHE_HOME:-$HOME/.cache}/agentty-e2e` keeps the
container's home, Cargo, and `prek` directories between runs, plus one `target-<hash>`
build directory per checkout path: every checkout is mounted at `/workspace`, and
Cargo's modification-time fingerprints could otherwise reuse another checkout's
binaries. Delete the directories of removed checkouts to reclaim space.

Every local run shares that cache and the Podman machine, so the hook holds an OS lock
in the cache directory until its container exits. A concurrent run from any checkout or
session reports that it is waiting and starts after the lock is released. The hook
labels its containers and, after taking the lock, also waits, without stopping them, for
labeled containers left running by a killed invocation. Run the hook as one foreground
command, or as a harness background task and wait for its completion notice. Do not poll
its output, `podman ps`, or processes, wrap it in wait loops, or start the container by
hand.

CI instead runs the image as root, which `actions/checkout` requires, registers
`GITHUB_WORKSPACE` as a Git `safe.directory`, and uses `check` mode to verify freshness
without rewriting artifacts.

Review the changed GIF and `.{name}.hash` sidecar, then refresh the PNG poster for every
regenerated GIF with the poster procedure below and commit all three together. Testty
records to a hidden staging file and replaces the committed GIF only after the recording
is nonempty. Successful generation intentionally removes the previous same-named PNG so
a stale poster cannot pass the nonempty-poster check; a failed recording preserves the
last valid GIF, hash sidecar, and poster.

## Regenerate Marketing Assets

The marketing demo and the `sessions` and `tab_tour` showcase recordings come from
ignored generators that always re-record and write no hash sidecar. Run each one alone
through the hook, then refresh its PNG poster with the procedure below. A rewritten GIF
makes `prek` report the hook as failed with `files were modified by this hook`; the
nextest summary above that message shows whether the generator passed.

`docs/site/static/demo/demo.gif` comes from `generate_marketing_demo_gif` in the `e2e`
target:

```sh
AGENTTY_RUN_IGNORED=only AGENTTY_TEST_FILTER='test(=demo::generate_marketing_demo_gif)' \
  prek run e2e-container --all-files --hook-stage manual
```

`docs/site/static/features/sessions.gif` and `tab_tour.gif` come from the `showcase`
target:

```sh
AGENTTY_TEST_TARGET=showcase AGENTTY_RUN_IGNORED=only \
  AGENTTY_TEST_FILTER='test(=feature_sessions_tab)' \
  prek run e2e-container --all-files --hook-stage manual
AGENTTY_TEST_TARGET=showcase AGENTTY_RUN_IGNORED=only \
  AGENTTY_TEST_FILTER='test(=feature_tab_tour)' \
  prek run e2e-container --all-files --hook-stage manual
```

## Create or Refresh the PNG Poster

Every feature GIF needs a same-named PNG poster for the site's `prefers-reduced-motion`
and `noscript` fallbacks. Create it when adding a GIF and regenerate it whenever the GIF
changes; `TESTTY_GIF_MODE=check` verifies GIF freshness but not the poster.

Inspect the finished GIF and choose a timestamp that clearly communicates the feature's
result, preferably a stable end state with the important UI visible. Do not
automatically use the first or midpoint frame, which may show an empty, loading, or
transitional state. Check the GIF duration first:

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

Use a timestamp accepted by `ffmpeg`, such as `00:00:01.500`, within the reported
duration. Open the PNG and verify that it is sharp, legible, free of transitional
artifacts, and still represents the current GIF; otherwise choose a better timestamp and
rerun the command.

Before finalizing, check that every GIF declared by a feature page has a nonempty
poster:

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

Do not run VHS recording directly in the developer host's operating-system environment,
including direct `vhs` commands and ignored tests that regenerate demo assets. Running
the pinned recording container on that host with Podman is the supported workflow above.
Bare-host VHS records through localhost sockets (`ttyd` plus the Chrome DevTools
protocol), which sandboxed agent shells can deny, crashing `vhs` before Chrome launches.
On macOS, even an unsandboxed bare-host run launches a local Chromium process that can
abort during AppKit registration and show a **Chromium quit unexpectedly** dialog. Use
the `e2e-container` hook for every intentional recording; do not suppress macOS crash
reporting to hide a bare-host browser failure.

When changing the image itself, follow
`docs/contributing/feature-test/image-maintenance.md`.

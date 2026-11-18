---
name: maintain-e2e-image
description: Build, test, and publish a replacement multi-architecture E2E recording image after changing container/e2e.Containerfile, verify its anonymous pulls, and pin the new digest with re-recorded artifacts.
---

# Maintain the E2E Image

Use this skill only when a maintainer changes `container/e2e.Containerfile`. Ordinary
recording uses the pinned image through
`skills/refresh-feature-recordings/references/recording.md`. Build both supported
platforms into one manifest, run affected focused feature tests against the native
candidate for each available architecture, then push `latest`.

## Publish Through the Workflow

Prefer the manual **Publish E2E Image** workflow in
`.github/workflows/publish-e2e-image.yml`, dispatched from the repository's default
branch. It builds and runs the E2E suite on native `ubuntu-24.04` AMD64 and
`ubuntu-24.04-arm` ARM64 runners, publishes architecture-specific candidates, assembles
the manifest, logs out of GHCR, verifies anonymous access to both variants, and reports
the digest in the workflow summary.

Before its first run, a package administrator must connect the existing `agentty-e2e`
package to this repository or grant this repository write access under the package's
**Manage Actions access** settings: the package predates the workflow, so its
`packages: write` permission alone cannot authorize the unconnected package. The
Containerfile's `org.opencontainers.image.source` label preserves the repository
association on later publications.

## Test a Local Candidate

To test a locally built candidate, set `AGENTTY_E2E_IMAGE` to its image ID from
`podman image inspect --format '{{.Id}}'` when running the `e2e-container` hook; the
hook then skips its pull and runs the image with `--pull=never`. Pulling a tag, or
running `--platform` with one, re-resolves it against the registry and silently replaces
the local build with a previously published image.

## Publish Locally

Maintainers with GHCR package-write permission can also publish locally. Use an explicit
registry destination so a later single-image push cannot be confused with the
manifest-list publication. Because the Containerfile contains `RUN` instructions, the
combined build requires binfmt/QEMU emulation for the non-native platform; without it,
use the workflow. Publication verification also requires `jq` on the maintainer host.

```sh
e2e_repository=ghcr.io/agentty-xyz/agentty-e2e
e2e_digest_file=$(mktemp)
trap 'rm -f "${e2e_digest_file}"' EXIT

podman build --jobs 2 \
  --platform linux/amd64,linux/arm64 \
  --manifest "${e2e_repository}:latest" \
  --file container/e2e.Containerfile \
  container

podman manifest push --all \
  --digestfile "${e2e_digest_file}" \
  "${e2e_repository}:latest" \
  "docker://${e2e_repository}:latest"

e2e_digest=$(sed -n '1p' "${e2e_digest_file}")
test -n "${e2e_digest}"
rm "${e2e_digest_file}"
trap - EXIT
e2e_published_image="${e2e_repository}@${e2e_digest}"
```

Before copying the pushed digest, log out so the inspection and pulls exercise the same
anonymous access that forked pull-request CI requires, then inspect the digest-qualified
remote reference and require both Linux platforms. The digest, unlike `latest`, keeps
Podman from answering with the local pre-push manifest, and the check rejects a
single-image manifest even when that image is `linux/amd64` or `linux/arm64`:

```sh
podman logout ghcr.io

podman manifest inspect "${e2e_published_image}" | jq -e '
  (.mediaType == "application/vnd.oci.image.index.v1+json"
    or .mediaType == "application/vnd.docker.distribution.manifest.list.v2+json")
  and ([
    .manifests[].platform
    | select(.os == "linux")
    | .architecture
  ] | unique | sort == ["amd64", "arm64"])
'

podman pull --platform linux/amd64 "${e2e_published_image}"
podman pull --platform linux/arm64 "${e2e_published_image}"
```

## Pin the Digest

1. Copy the reported digest into the `container.image` value in
   `.github/workflows/e2e.yml`, which is also the `e2e-container` hook's local image.
   The pinned digest must remain an image index with both platforms.
1. Re-record every feature affected by a tool, browser, font, or rendering change and
   refresh its PNG poster with `skills/refresh-feature-recordings/SKILL.md`.
1. Update the digest and artifacts together.

Do not update the repository when logout, inspection, either native test, or either
platform pull fails.

## Handoff

Report the published digest, the platforms verified, the focused tests run against each
candidate, and the recordings refreshed or still blocked.

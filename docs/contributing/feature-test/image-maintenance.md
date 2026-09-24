# Maintaining the Recording Image

Only maintainers changing `container/e2e.Containerfile` should build and publish a
replacement image. Build both supported platforms into one manifest, run affected
focused feature tests against the native candidate for each available architecture, then
push `latest`. The preferred path is the manual **Publish E2E Image** workflow in
`.github/workflows/publish-e2e-image.yml`, dispatched from the repository's default
branch. It builds and runs the E2E suite on native `ubuntu-24.04` AMD64 and
`ubuntu-24.04-arm` ARM64 runners, publishes architecture-specific candidates, assembles
the manifest, logs out of GHCR, verifies anonymous access to both variants, and reports
the digest in the workflow summary.

Before its first run, a package administrator must connect the existing `agentty-e2e`
package to this repository or grant this repository write access under the package's
**Manage Actions access** settings. The package predates this workflow, so the
workflow's `packages: write` permission cannot authorize an unconnected package by
itself. `container/e2e.Containerfile` carries the `org.opencontainers.image.source`
label to preserve the repository association on subsequent publications.

Copy the reported digest into the `container.image` value in `.github/workflows/e2e.yml`
and the `published_e2e_image` assignment in
`docs/contributing/feature-test/recording.md`. The pinned digest must remain an image
index with both platforms; do not update the repository when either native test or
either platform pull fails. Re-record every feature affected by a tool, browser, font,
or rendering change and refresh its PNG poster before updating the digest and artifacts
together.

The following local flow remains available to maintainers with GHCR package-write
permission. Use an explicit registry destination so a later single-image push cannot be
confused with the manifest-list publication. Because the Containerfile contains `RUN`
instructions, the combined build requires binfmt/QEMU emulation for the non-native
platform; without it, use the manual workflow instead.

When testing a locally built candidate, skip the `podman pull` step in
`docs/contributing/feature-test/recording.md` and run the candidate's image ID from
`podman image inspect --format '{{.Id}}'` or pass `--pull=never` to `podman run`.
Pulling a tag, or running `--platform` with one, re-resolves it against the registry and
silently replaces the local build with a previously published image.

The publication verification command also requires `jq` on the maintainer host.

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

Before copying the digest reported by `podman manifest push`, inspect its
digest-qualified remote reference and require both Linux platforms. Using the digest
instead of `latest` prevents Podman from satisfying the inspection with the local
pre-push manifest. This check rejects a single-image manifest even if that image itself
is `linux/amd64` or `linux/arm64`:

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

Do not update the repository when logout, inspection, or either platform pull fails. The
logout makes the inspection and pulls exercise the same anonymous access that forked
pull-request CI requires.

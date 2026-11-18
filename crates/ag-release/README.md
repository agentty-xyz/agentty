# ag-release

`ag-release` plans and packages GitHub releases of the binaries in a Cargo workspace. It
reads its configuration from `Cargo.toml`, prints a release plan (a JSON document that
release workflows consume), builds deterministic archives, and assembles the
release-wide checksums and manifest. It is a small, project-agnostic replacement for a
`dist` (`cargo-dist`) `dist-workspace.toml` setup.

## Quick Start

1. Install the tool:
   `cargo install --git https://github.com/agentty-xyz/agentty ag-release --locked`.
1. Add `[workspace.metadata.release]` to the workspace `Cargo.toml` (see
   [Configuration](#configuration)).
1. Set `repository = "https://github.com/<owner>/<name>"` on each released package.
1. Run `ag-release plan` from anywhere in the workspace to validate the configuration.
1. In the release workflow, run `ag-release build --target <triple>` on each matrix job,
   collect every job's output into one directory, and run
   `ag-release assemble --tag <tag> --artifacts-dir <dir>` on a checkout of the tagged
   commit. Upload the directory's files to the GitHub release.

## Configuration

```toml
[workspace.metadata.release]
profile = "dist"                     # Cargo profile for release builds
min-glibc = "2.35"                   # minimum glibc on *-linux-gnu targets
installers = ["shell", "npm"]        # installers to publish
include = ["README.md", "LICENSE"]   # workspace-root files added to every archive

[workspace.metadata.release.targets] # target triple -> GitHub Actions runner label
aarch64-apple-darwin = "macos-latest"
x86_64-unknown-linux-gnu = "ubuntu-22.04"

[workspace.metadata.release.apps.my-app]   # key = workspace package name
[workspace.metadata.release.apps.my-tool]
npm-scope = "@my-org"                      # optional npm scope
```

| Key          | Required | Default     | Value                                                   |
| ------------ | -------- | ----------- | ------------------------------------------------------- |
| `profile`    | no       | `"release"` | Cargo profile name                                      |
| `min-glibc`  | no       | none        | `"MAJOR.MINOR"`                                         |
| `installers` | no       | `[]`        | Unique entries from `"shell"` and `"npm"`               |
| `include`    | no       | `[]`        | Unique relative paths to files in the workspace root    |
| `targets`    | yes      |             | At least one supported target, each with a runner label |
| `apps`       | yes      |             | At least one workspace package with a `bin` target      |
| `npm-scope`  | no       | none        | Per app: `@` and a lowercase npm scope name             |

Supported targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, and `x86_64-unknown-linux-gnu`.

An app's binaries are its package's `bin` targets. Its description, license, and authors
come from the package's resolved Cargo metadata. All apps release in lockstep: they must
share one version and one GitHub `repository`.

Unknown keys, wrong types, and invalid values are errors that name the key.

## Commands

### `plan [--tag <tag>]`

Validates the configuration, checks that every `include` file exists, and prints the
release plan as JSON on stdout. With `--tag`, the tag must be `v<semver>` or `<semver>`
and name the apps' version. Errors go to stderr with a nonzero exit status.

| Field                   | Value                                                                                 |
| ----------------------- | ------------------------------------------------------------------------------------- |
| `tag`                   | The `--tag` value as given, or `null`                                                 |
| `version`               | Version every app shares                                                              |
| `prerelease`            | Whether `version` has a semver pre-release component                                  |
| `repository`            | `owner`, `name`, and canonical `url`                                                  |
| `profile`, `min_glibc`  | Configured values; `min_glibc` is `null` when unset                                   |
| `installers`, `include` | Configured lists                                                                      |
| `matrix`                | GitHub Actions matrix: `{"include": [{"runner", "target"}]}`                          |
| `artifacts`             | Release-wide file names: `checksums`, `manifest`, `source_archive`, `source_checksum` |
| `apps[]`                | Per app, in name order; see below                                                     |

Each `apps[]` entry has `name`, `binaries`, `description`, `license`, `authors`,
`archives[]` (`target`, `name`, `checksum`, `directory`), and `shell_installer`,
`npm_package`, and `npm_tarball`, which are `null` unless that installer is enabled.

### `build --target <triple> [--out-dir <dir>]`

Runs
`cargo build --locked --profile <profile> --target <triple> --package <app>... --bins`
in the workspace root, streaming Cargo's output to stderr, then writes each app's
`<app>-<target>.tar.gz` and `.sha256` into `<dir>` and prints their paths on stdout. The
target must be configured, and its Rust target must already be installed (for example
with `rustup target add`); `ag-release` does not install toolchains. `<dir>` defaults to
`ag-release.out` in Cargo's target directory.

### `assemble --tag <tag> --artifacts-dir <dir>`

Checks that `<dir>` holds every archive the plan names, each with a `.sha256` file that
matches it, and fails with a list of every missing or mismatched file otherwise. Then
writes `source.tar.gz` and its `.sha256`, `sha256.sum`, and `release-manifest.json` into
`<dir>` and prints their paths. The source archive is `git archive HEAD` of the
workspace root below a `<repository name>-<version>/` directory, so run `assemble` on a
checkout of the tagged commit. Other files in `<dir>` are ignored.

`sha256.sum` lists every archive and the source archive, sorted by file name.
`release-manifest.json` holds the release plan for the tag and the SHA-256 of each file
that `sha256.sum` lists:

```json
{
  "plan": { "tag": "v1.2.3", "version": "1.2.3", "...": "..." },
  "sha256": {
    "my-app-x86_64-unknown-linux-gnu.tar.gz": "<hex>",
    "source.tar.gz": "<hex>"
  }
}
```

## Artifacts

The plan names every file a release publishes:

| File                           | Contents                                                            |
| ------------------------------ | ------------------------------------------------------------------- |
| `<app>-<target>.tar.gz`        | The app's binaries and `include` files in `<app>-<target>/`         |
| `<app>-<target>.tar.gz.sha256` | `sha256sum`-compatible checksum: `<hex>  <file name>`               |
| `<app>-installer.sh`           | Shell installer (planned; `assemble` does not write it yet)         |
| `<app>-npm-package.tar.gz`     | npm package (planned; `assemble` does not write it yet)             |
| `source.tar.gz` and `.sha256`  | Source archive of the tagged commit                                 |
| `sha256.sum`                   | `sha256sum`-compatible checksums of the archives and source archive |
| `release-manifest.json`        | The plan and the SHA-256 of every file in `sha256.sum`              |

Archives are reproducible: the same inputs give byte-identical `.tar.gz` files. Entries
are sorted, owned by uid and gid 0 without user or group names, and have mode `0755`
(directories and binaries) or `0644` (other files). Every entry's modification time is
`SOURCE_DATE_EPOCH` when that variable is set to a number of seconds, and 0 otherwise.
The gzip header records no file name or timestamp. The source archive's entries carry
the commit time that `git archive` records.

## Limitations

- GitHub-hosted repositories only.
- macOS and glibc Linux targets only: no Windows or musl.
- No Homebrew, MSI, or PowerShell installers, updater binaries, or workflow generation.

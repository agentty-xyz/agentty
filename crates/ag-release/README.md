# ag-release

`ag-release` plans GitHub releases of the binaries in a Cargo workspace. It reads its
configuration from `Cargo.toml` and prints a release plan, a JSON document that release
workflows consume. It is a small, project-agnostic replacement for a `dist`
(`cargo-dist`) `dist-workspace.toml` setup.

## Quick Start

1. Install the tool from a pinned commit:
   `cargo install --git https://github.com/agentty-xyz/agentty --rev <commit> ag-release --locked`.
   The tool is not published to crates.io and its configuration, plan, and artifacts
   change with that repository, so an unpinned install can change a release.
1. Add `[workspace.metadata.release]` to the workspace `Cargo.toml` (see
   [Configuration](#configuration)).
1. Set `repository = "https://github.com/<owner>/<name>"` on each released package.
1. Run `ag-release plan` from anywhere in the workspace to validate the configuration.

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

## Artifacts

The plan names every file a release publishes:

| File                           | Contents                                   |
| ------------------------------ | ------------------------------------------ |
| `<app>-<target>.tar.gz`        | Top-level `<app>-<target>/` directory      |
| `<app>-<target>.tar.gz.sha256` | `sha256sum`-compatible checksum            |
| `<app>-installer.sh`           | Shell installer                            |
| `<app>-npm-package.tar.gz`     | npm package `<npm-scope>/<app>` or `<app>` |
| `source.tar.gz` and `.sha256`  | Source archive of the tagged commit        |
| `sha256.sum`                   | Checksums of every published file          |
| `release-manifest.json`        | The plan and the SHA-256 of every artifact |

## Limitations

- GitHub-hosted repositories only.
- macOS and glibc Linux targets only: no Windows or musl.
- No Homebrew, MSI, or PowerShell installers, updater binaries, or workflow generation.

# ag-release

`ag-release` plans and packages GitHub releases of the binaries in a Cargo workspace. It
reads its configuration from `Cargo.toml`, prints a release plan (a JSON document that
release workflows consume), builds deterministic archives, assembles the release-wide
checksums and manifest, and writes the GitHub release notes from `CHANGELOG.md`. It is a
small, project-agnostic replacement for a `dist` (`cargo-dist`) `dist-workspace.toml`
setup.

## Quick Start

1. Install the tool from a pinned commit:
   `cargo install --git https://github.com/agentty-xyz/agentty --rev <commit> ag-release --locked`.
   The tool is not published to crates.io and its configuration, plan, and artifacts
   change with that repository, so an unpinned install can change a release.
1. Add `[workspace.metadata.release]` to the workspace `Cargo.toml` (see
   [Configuration](#configuration)).
1. Set `repository = "https://github.com/<owner>/<name>"` on each released package.
1. Run `ag-release plan` from anywhere in the workspace to validate the configuration.
1. Add a release workflow that runs the commands as
   [Release Workflow](#release-workflow) describes.

## Configuration

```toml
[workspace.metadata.release]
profile = "dist"                     # Cargo profile for release builds
min-glibc = "2.35"                   # minimum glibc on *-linux-gnu targets
installers = ["shell", "npm"]        # installers to publish
include = ["README.md", "LICENSE"]   # files added to every archive

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
| `include`    | no       | `[]`        | Unique relative paths to files                          |
| `targets`    | yes      |             | At least one supported target, each with a runner label |
| `apps`       | yes      |             | At least one workspace package with a `bin` target      |
| `npm-scope`  | no       | none        | Per app: `@` and a lowercase npm scope name             |

Supported targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, and `x86_64-unknown-linux-gnu`.

Each app takes an `include` path from its package directory, the one holding its
`Cargo.toml`, when the file exists there, else from the workspace root; either way the
archive entry keeps the configured path. A path found in neither is an error that names
the path and the app.

An app's binaries are its package's `bin` targets. Its description, license, and authors
come from the package's resolved Cargo metadata. All apps release in lockstep: they must
share one version and one GitHub `repository`.

Unknown keys, wrong types, and invalid values are errors that name the key.

## Commands

### `plan [--tag <tag>] [--target <triple>]...`

Validates the configuration, checks that every `include` file exists for every app, and
prints the release plan as JSON on stdout. With `--tag`, the tag must be `v<semver>` or
`<semver>` and name the apps' version. With `--target`, the plan covers only the named
configured targets instead of all of them. Errors go to stderr with a nonzero exit
status.

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

### `build --target <triple> [--out-dir <dir>] [--profile <name>]`

Runs
`cargo build --locked --profile <profile> --target <triple> --package <app>... --bins`
in the workspace root, streaming Cargo's output to stderr, then writes each app's
`<app>-<target>.tar.gz` and `.sha256` into `<dir>` and prints their paths on stdout. The
target must be configured, and its Rust target must already be installed (for example
with `rustup target add`); `ag-release` does not install toolchains. `<dir>` defaults to
`ag-release.out` in Cargo's target directory. `--profile` replaces the configured
profile, for example `dev` for faster test builds.

### `assemble --tag <tag> --artifacts-dir <dir> [--target <triple>]...`

Checks that `<dir>` holds every archive the plan names, each with a `.sha256` file that
matches it, and fails with a list of every missing or mismatched file otherwise. With
`--target`, the plan, and so the archives, installers, and manifest, covers only the
named configured targets, so one machine can test what it built. Then writes each app's
`<app>-installer.sh` when the `shell` installer is configured (see
[Shell Installer](#shell-installer)), its `<app>-npm-package.tar.gz` when the `npm`
installer is configured (see [npm Package](#npm-package)), `source.tar.gz` and its
`.sha256`, `sha256.sum`, and `release-manifest.json` into `<dir>` and prints their
paths. The source archive is `git archive HEAD` of the workspace root below a
`<repository name>-<version>/` directory, so run `assemble` on a checkout of the tagged
commit. Other files in `<dir>` are ignored.

`sha256.sum` lists every archive, installer, npm package, and the source archive, sorted
by file name. `release-manifest.json` holds the release plan for the tag and the SHA-256
of each file that `sha256.sum` lists:

```json
{
  "plan": { "tag": "v1.2.3", "version": "1.2.3", "...": "..." },
  "sha256": {
    "my-app-x86_64-unknown-linux-gnu.tar.gz": "<hex>",
    "source.tar.gz": "<hex>"
  }
}
```

### `notes --tag <tag> [--title]`

Prints the GitHub release body as Markdown on stdout, or with `--title` only the release
title, for a tag that names the apps' version. It reads `CHANGELOG.md` in the workspace
root and needs no built artifacts.

The release's changelog entry starts at the first level-2 heading that names the
version, as `v<version>` or `<version>`, optionally in brackets and optionally followed
by ` - <date>`, such as `## [v1.2.3] - 2026-01-02` in the
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. It runs to the next
level-1 or level-2 heading outside a fenced code block and is copied verbatim, without
surrounding blank lines.

- Title: `<tag> - <date>`, or `<tag>` when the entry has no date or there is no entry.
- Body: `## Release Notes` and the entry, omitted when the entry is missing or empty;
  then for each app `# <app> <version>` with the configured installers' install
  commands, a table of the archives with their platforms and checksum files, and how to
  verify the release's GitHub artifact attestations with `gh attestation verify`.

A missing `CHANGELOG.md` counts as having no entry. The release workflow is expected to
attest the published files (for example with `actions/attest-build-provenance`), as the
body says. For example:

```sh
ag-release notes --tag "$TAG" > release-notes.md
gh release create "$TAG" --title "$(ag-release notes --tag "$TAG" --title)" \
  --notes-file release-notes.md artifacts/*
```

## Release Workflow

A GitHub Actions release workflow on a tag push runs one command per job and passes
files between jobs as workflow artifacts:

| Job       | Runs                                                         | Consumes                         | Produces                    |
| --------- | ------------------------------------------------------------ | -------------------------------- | --------------------------- |
| plan      | `ag-release plan --tag "$TAG"`                               | The checkout                     | Plan JSON as a job output   |
| build     | `rustup target add`, then `ag-release build --target`        | One `matrix` entry of the plan   | That target's archives      |
| assemble  | `ag-release assemble`, then `ag-release notes` and `--title` | Every archive, on the tag commit | Release files, title, body  |
| release   | `gh release create` with the title, body, and release files  | The assemble job's output        | The GitHub release          |
| npm, etc. | `npm publish` of each app's `npm_tarball`                    | The release files and the plan   | Packages, after the release |

Run each build job on its matrix entry's runner, for example with
`strategy: { matrix: ${{ fromJson(needs.plan.outputs.plan).matrix }} }` and
`runs-on: ${{ matrix.runner }}`. Publish packages only after the GitHub release exists,
because their installers download its archives, and skip them when the plan's
`prerelease` is `true` if prereleases should not reach package registries. Running
`plan` on pull requests catches configuration errors before a release.

To test a workflow without publishing, build the host target with `--profile dev`, pass
the same `--target` to `plan` and `assemble`, and install from the artifacts directory
with `<APP>_DOWNLOAD_URL=file://<dir>`. This repository's
[`release.yml`](https://github.com/agentty-xyz/agentty/blob/main/.github/workflows/release.yml)
is a complete example with release checks, attestations, draft-first publishing, and
these tests.

## Artifacts

The plan names every file a release publishes:

| File                           | Contents                                                                                       |
| ------------------------------ | ---------------------------------------------------------------------------------------------- |
| `<app>-<target>.tar.gz`        | The app's binaries and `include` files in `<app>-<target>/`                                    |
| `<app>-<target>.tar.gz.sha256` | `sha256sum`-compatible checksum: `<hex>  <file name>`                                          |
| `<app>-installer.sh`           | POSIX `sh` installer of the app's binaries                                                     |
| `<app>-npm-package.tar.gz`     | npm package tarball, files below `package/`, that installs the app's binaries                  |
| `source.tar.gz` and `.sha256`  | Source archive of the tagged commit                                                            |
| `sha256.sum`                   | `sha256sum`-compatible checksums of the archives, installers, npm packages, and source archive |
| `release-manifest.json`        | The plan and the SHA-256 of every file in `sha256.sum`                                         |

Archives and npm packages are reproducible: the same inputs give byte-identical
`.tar.gz` files. Entries are sorted, owned by uid and gid 0 without user or group names,
and have mode `0755` (directories, binaries, and npm commands) or `0644` (other files);
npm packages, like `npm pack` output, have no directory entries. Every entry's
modification time is `SOURCE_DATE_EPOCH` when that variable is set to a number of
seconds, and 0 otherwise. The gzip header records no file name or timestamp. The source
archive's entries carry the commit time that `git archive` records.

## Shell Installer

`<app>-installer.sh` installs the app's binaries for the current platform:

```sh
curl -LsSf https://github.com/<owner>/<name>/releases/latest/download/<app>-installer.sh | sh
```

The script is POSIX `sh` plus `local` (which dash, bash, BusyBox ash, and zsh provide)
with the release's values baked in, and its whole body is a function called on the last
line, so a truncated download runs nothing. `--help` (or `-h`) prints its usage and
environment variables without installing; any other argument fails before anything is
downloaded or written. It:

1. Maps `uname -s` and `uname -m` to a configured target, preferring
   `aarch64-apple-darwin` under Rosetta 2. On Linux it rejects a 32-bit userland (read
   from the ELF header of `/proc/self/exe`, else `/bin/sh`, and assumed 64-bit when
   neither is readable), musl, and, when `min-glibc` is set, a glibc older than that
   (read from `ldd --version`). Other platforms fail with the list of supported targets.
1. Downloads `<app>-<target>.tar.gz` with `curl`, or `wget` without `curl` or when
   `curl` is a snap (whose private `/tmp` it cannot use; it fails when `wget` is absent
   too), and fails unless the archive's SHA-256, recorded by `assemble`, matches by
   `sha256sum` or `shasum -a 256`. It also fails when neither tool exists.
1. Installs the binaries into `<prefix>/bin`, where `<prefix>` is `$<APP>_INSTALL_DIR`,
   else `$CARGO_HOME`, else `$HOME/.cargo`. Each binary is copied to a temporary file in
   that directory and renamed over the old one, so a running binary is never written
   over and running the installer again updates the app.
1. Writes the [receipt](#receipt) and [puts the binaries on `PATH`](#path-setup).

`<APP>` is the app name in upper case with `-` replaced by `_`:

| Variable                  | Effect                                                                                                    |
| ------------------------- | --------------------------------------------------------------------------------------------------------- |
| `<APP>_DOWNLOAD_URL`      | Archive base URL instead of `https://github.com/<owner>/<name>/releases/download/<tag>`; `file://` works  |
| `<APP>_INSTALL_DIR`       | Install prefix                                                                                            |
| `<APP>_UNMANAGED_INSTALL` | Directory to install the binaries into directly, with no receipt or `PATH` setup; overrides `INSTALL_DIR` |
| `<APP>_NO_MODIFY_PATH`    | `1` (any value but empty or `0`) skips `PATH` setup                                                       |

### Receipt

A managed install, one without `<APP>_UNMANAGED_INSTALL`, writes
`${XDG_CONFIG_HOME:-$HOME/.config}/<app>/<app>-receipt.json` on one line:

```json
{"binaries":["my-app"],"install_layout":"cargo-home","install_prefix":"/home/me/.cargo","modify_path":true,"provider":{"source":"ag-release","version":"<ag-release version>"},"source":{"app_name":"my-app","name":"<repository name>","owner":"<owner>","release_type":"github"},"version":"1.2.3"}
```

`install_prefix` is the absolute prefix, and `modify_path` is `false` when
`<APP>_NO_MODIFY_PATH` is set.

### PATH Setup

Unless `<APP>_NO_MODIFY_PATH` is set or `<prefix>/bin` is already on `PATH`, the
installer:

- creates `<prefix>/env` and `<prefix>/env.fish`, the scripts `rustup` also writes, when
  they are absent, writing a prefix below `$HOME` as `$HOME/...`;
- appends `. "<prefix>/env"` to `~/.profile`, to each existing `~/.bashrc`,
  `~/.bash_profile`, and `~/.bash_login`, and to `.zshrc` (or an existing `.zshenv` when
  `.zshrc` is absent) in `${ZDOTDIR:-$HOME}`;
- appends `source "<prefix>/env.fish"` to
  `${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/<app>.env.fish`;
- appends `<prefix>/bin` to `$GITHUB_PATH` when it is set.

A file that already sources the script is left unchanged.

## npm Package

`<app>-npm-package.tar.gz` is a ready-to-publish npm package named `<npm-scope>/<app>`,
or `<app>` without a scope. Publish it with public access, which a scoped package needs
because npm publishes scoped packages as restricted by default:

```sh
npm publish --access public <app>-npm-package.tar.gz
```

npm 11 and newer refuse to publish a prerelease version without a dist-tag, so add
`--tag <dist-tag>` (for example `--tag next`) for one.

Its `package.json` takes `version`, `description`, `license`, `repository`, and
`contributors` (the authors) from Cargo metadata, declares one `bin` command per binary,
requires Node 18 or newer, and has no dependencies. Its `os` and `cpu` fields list the
configured targets' platforms, so npm rejects other platforms before installing. The
package holds `package.json`, `binary.js`, `install.js`, one `run-<binary>.js` per
binary, and `README.md`, `LICENSE`, and `CHANGELOG.md` when they exist, each taken from
the app's package directory before the workspace root as `include` paths are.

`install.js`, the `postinstall` script, picks the target and checks glibc as the
[shell installer](#shell-installer) does, downloads `<app>-<target>.tar.gz`, fails
unless its SHA-256 matches the one `assemble` recorded, and extracts it with the system
`tar`. It installs the binaries into `<package>/node_modules/.bin_real/`, renaming each
into place. This location is stable: an app can recognize an npm, Bun, or pnpm global
install by its executable path ending in `<package>/node_modules/.bin_real/<binary>`.
When install scripts did not run, as with `npm install --ignore-scripts` or a package
manager that blocks dependency scripts, the first run of a command installs the binaries
first.

Each command runs its binary with the same arguments, standard streams, and exit status,
and ends by the binary's signal when the binary is killed. It ignores `SIGINT` and
`SIGQUIT`, which the terminal also sends to the binary, and passes `SIGHUP` and
`SIGTERM` on to it.

| Variable                                                                 | Effect                                                                                                   |
| ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| `<APP>_DOWNLOAD_URL`                                                     | Archive base URL instead of `https://github.com/<owner>/<name>/releases/download/<tag>`; `file://` works |
| `npm_config_https_proxy`, `https_proxy`, `npm_config_proxy`, `all_proxy` | Proxy: download with `curl --proxy <proxy>` instead of Node's built-in `fetch`, which ignores proxies    |
| `npm_config_no_proxy`, `npm_config_noproxy`, `no_proxy`                  | With a proxy, hosts to reach directly, passed as `curl --noproxy <hosts>`                                |

The first variable of each row that is set, in the order listed and each name in lower,
then upper case, wins, as in `proxy-from-env` 1.x. npm passes its `https-proxy`,
`proxy`, and `noproxy` settings (from `npm config set` or `.npmrc`) to install scripts
as these `npm_config_*` variables.

npm, Bun, and pnpm installs update through their package manager, for example
`npm install --global <package>@latest`.

## Updates

`ag-release` publishes no updater binaries: the shell installer is the update path for
shell installs, and package managers update npm installs. An app installed by the shell
installer updates itself in three steps:

1. Read the receipt. A receipt whose `provider.source` is `ag-release`, whose
   `source.app_name` names the app, and whose `<install_prefix>/bin/<binary>` is the
   running executable proves a shell install.
1. Download
   `https://github.com/<owner>/<name>/releases/latest/download/<app>-installer.sh`.
1. Run it with `sh`, `<APP>_INSTALL_DIR=<install_prefix>`, `<APP>_NO_MODIFY_PATH=1`, and
   `<APP>_UNMANAGED_INSTALL` set to empty, because an inherited value would install
   somewhere else.

## Limitations

- GitHub-hosted repositories only.
- macOS and glibc Linux targets only: no Windows or musl.
- No Homebrew, MSI, or PowerShell installers, updater binaries, or workflow generation.
- The shell installer needs `curl` (not a snap) or `wget`, `tar` with gzip support, and
  `sha256sum` or `shasum`.
- The npm package needs Node 18 or newer, `tar` with gzip support, and `curl` when a
  proxy variable is set. Yarn Plug'n'Play must unplug it (`preferUnplugged` asks for
  that).

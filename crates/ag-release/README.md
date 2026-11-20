# ag-release

`ag-release` plans and packages GitHub releases of the binaries in a Cargo workspace. It
reads its configuration from `Cargo.toml`, bumps the release version, prints a release
plan (a JSON document that release workflows consume), builds deterministic archives,
assembles the installers, release-wide checksums, and manifest, writes the GitHub
release notes from `CHANGELOG.md`, and generates the GitHub Actions workflow that runs
these steps. It is a small, project-agnostic replacement for a `dist` (`cargo-dist`)
`dist-workspace.toml` setup.

## Quick Start

1. Install the tool from a pinned commit:
   `cargo install --git https://github.com/agentty-xyz/agentty --rev <commit> ag-release --locked`.
   The tool is not published to crates.io and its configuration, plan, and artifacts
   change with that repository, so an unpinned install can change a release.
1. Add `[workspace.metadata.release]` to the workspace `Cargo.toml` (see
   [Configuration](#configuration)).
1. Set `repository = "https://github.com/<owner>/<name>"` on each released package.
1. Set `[workspace.metadata.release.workflow]` `tool` to the same repository and commit,
   so the release jobs install the version you run locally.
1. Run `ag-release plan` from anywhere in the workspace to validate the configuration.
1. For each release, run `ag-release prepare <version> --date <YYYY-MM-DD>` (see
   [`prepare`](#prepare-version---date-yyyy-mm-dd)), fill in the changelog entry, and
   tag the merged commit.
1. Run `ag-release workflow` and commit `.github/workflows/release.yml` (see
   [Release Workflow](#release-workflow)).

## Configuration

```toml
[workspace.metadata.release]
profile = "dist"                     # Cargo profile for release builds
min-glibc = "2.35"                   # minimum glibc on *-linux-gnu targets
installers = ["shell", "homebrew", "npm", "binstall"]  # installers to publish
homebrew-tap = "my-org/tap"          # tap of the Homebrew formulas
include = ["README.md", "LICENSE"]   # files added to every archive

[workspace.metadata.release.targets] # target triple -> GitHub Actions runner label
aarch64-apple-darwin = "macos-latest"
x86_64-unknown-linux-gnu = "ubuntu-22.04"
x86_64-unknown-linux-musl = "ubuntu-22.04"

[workspace.metadata.release.apps.my-app]   # key = workspace package name
[workspace.metadata.release.apps.my-tool]
npm-scope = "@my-org"                      # optional npm scope
```

| Key            | Required        | Default     | Value                                                                  |
| -------------- | --------------- | ----------- | ---------------------------------------------------------------------- |
| `profile`      | no              | `"release"` | Cargo profile name                                                     |
| `min-glibc`    | no              | none        | `"MAJOR.MINOR"`                                                        |
| `installers`   | no              | `[]`        | Unique entries from `"shell"`, `"homebrew"`, `"npm"`, and `"binstall"` |
| `homebrew-tap` | with `homebrew` | none        | `<owner>/<name>` of the tap repository `<owner>/homebrew-<name>`       |
| `include`      | no              | `[]`        | Unique relative paths to files                                         |
| `targets`      | yes             |             | At least one supported target, each with a runner label                |
| `apps`         | yes             |             | At least one workspace package with a `bin` target                     |
| `npm-scope`    | no              | none        | Per app: `@` and a lowercase npm scope name                            |

Supported targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`,
and `x86_64-unknown-linux-musl`. The `*-linux-musl` targets link statically and run on
any Linux of their architecture, so installers fall back to them on systems whose glibc
is missing or older than `min-glibc`; they build on Linux runners of their architecture
with `musl-gcc` (see [Release Workflow](#release-workflow)).

The `homebrew` installer requires `homebrew-tap`, which is the `<owner>/<name>` that
`brew install <owner>/<name>/<app>` takes, without the `homebrew-` repository prefix;
`homebrew-tap` without the installer is an error. The `binstall` installer requires each
app's `Cargo.toml` to point `cargo binstall` at the release archives, and `plan` fails
with the table to add otherwise (see [cargo-binstall](#cargo-binstall)).

Each app takes an `include` path from its package directory, the one holding its
`Cargo.toml`, when the file exists there, else from the workspace root; either way the
archive entry keeps the configured path. A path found in neither is an error that names
the path and the app.

An app's binaries are its package's `bin` targets. Its description, license, and authors
come from the package's resolved Cargo metadata. All apps release in lockstep: they must
share one version and one GitHub `repository`.

Unknown keys, wrong types, and invalid values are errors that name the key.

The optional `workflow` table configures the generated
[release workflow](#release-workflow):

```toml
[workspace.metadata.release.workflow]
tool = { git = "https://github.com/<owner>/<name>", rev = "<commit>" }
node-version = "24.x"
smoke-paths = [".github/workflows/publish-npm.yml"]

[workspace.metadata.release.workflow.distributions]  # container image -> expected error
"ubuntu:22.04" = ""                                    # must install and run
"ubuntu:20.04" = "needs glibc"                         # must fail with this text

[workspace.metadata.release.workflow.jobs.publish-npm]  # key = job id
stage = "publish"
uses = "./.github/workflows/publish-npm.yml"
permissions = { contents = "read", id-token = "write" }
plan = true

[workspace.metadata.release.workflow.jobs.publish-homebrew]
stage = "publish"
uses = "./.github/workflows/publish-homebrew.yml"
plan = true
secrets = ["HOMEBREW_TAP_DEPLOY_KEY"]
```

| Key                     | Default                  | Value                                                                                                   |
| ----------------------- | ------------------------ | ------------------------------------------------------------------------------------------------------- |
| `tool`                  | build from the workspace | `git` (`https://` URL) and `rev` (full 40-digit commit) to install `ag-release` from                    |
| `node-version`          | the runner's Node.js     | `actions/setup-node` version for `smoke`                                                                |
| `smoke-paths`           | `[]`                     | Unique relative paths whose change runs `smoke`                                                         |
| `distributions`         | none                     | Image to `""` (must install) or text the installer's failure contains; needs `shell` and a Linux target |
| `jobs.<id>.stage`       | required                 | `"check"` or `"publish"`                                                                                |
| `jobs.<id>.uses`        | required                 | Reusable workflow: `./<path>`, `$/<path>`, or `<owner>/<repo>/<path>@<ref>`, copied verbatim            |
| `jobs.<id>.permissions` | `{}`                     | `GITHUB_TOKEN` permission names to `read`, `write`, or `none`, as GitHub documents them                 |
| `jobs.<id>.plan`        | `false`                  | Pass the plan JSON as the reusable workflow's `plan` input                                              |
| `jobs.<id>.prerelease`  | `false`                  | Run a `publish` job for prereleases too                                                                 |
| `jobs.<id>.secrets`     | `[]`                     | Unique repository secret names passed to the reusable workflow under the same names                     |

`tool` is required unless `ag-release` is a package of the workspace. Job ids must be
valid GitHub job ids that differ, ignoring case, from each other and from the built-in
`plan`, `smoke`, `build`, `assemble`, `distributions`, and `host`. Values the workflow
embeds may not contain line breaks, other control characters, or `${{`. Secret names are
letters, digits, and `_`, and start with neither a digit nor `GITHUB_`; each one is
passed explicitly, never with `secrets: inherit`.

## Commands

### `prepare <version> --date <YYYY-MM-DD>`

Bumps the workspace to `<version>`, `<semver>` or `v<semver>`, which must be newer than
the apps' version, and prints the paths it wrote:

1. Every workspace member at the apps' version moves to `<version>`: its own `[package]`
   `version`, or the root `[workspace.package]` `version` it inherits.
1. Each `version` requirement of a path dependency on one of those members, in the root
   `[workspace.dependencies]` or a member's `[dependencies]`, `[dev-dependencies]`,
   `[build-dependencies]`, or their `[target.<cfg>]` tables, moves to `<version>` when
   it names the old version, keeping a `=`, `^`, or `~` operator. Other requirements,
   such as `"1.2"`, stay as they are.
1. `cargo update --workspace` refreshes `Cargo.lock`.
1. Unless `CHANGELOG.md` in the workspace root is missing or already has an entry for
   `<version>`, it gains the heading `## <label> - <date>`: directly below an
   `## [Unreleased]` heading, so the unreleased notes become the release's, else above
   the first level-2 heading. `<label>` is spelled like the newest release heading, with
   or without brackets and the `v` prefix, and `[v<version>]` without one.

Manifests are edited at the version strings only, so formatting and comments survive,
and none is written unless every manifest parses. Commit the result as the release
change, then tag its commit.

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
| `homebrew_tap`          | `name` (`<owner>/<name>`) and `repository` (`<owner>/homebrew-<name>`), or `null`     |
| `matrix`                | GitHub Actions matrix: `{"include": [{"runner", "target"}]}`                          |
| `artifacts`             | Release-wide file names: `checksums`, `manifest`, `source_archive`, `source_checksum` |
| `apps[]`                | Per app, in name order; see below                                                     |

Each `apps[]` entry has `name`, `binaries`, `description`, `license`, `authors`,
`archives[]` (`target`, `name`, `checksum`, `directory`), and `shell_installer`,
`homebrew_formula`, `npm_package`, and `npm_tarball`, which are `null` unless that
installer is enabled.

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
[Shell Installer](#shell-installer)), its `<app>.rb` when the `homebrew` installer is
configured (see [Homebrew Formula](#homebrew-formula)), its `<app>-npm-package.tar.gz`
when the `npm` installer is configured (see [npm Package](#npm-package)),
`source.tar.gz` and its `.sha256`, `sha256.sum`, and `release-manifest.json` into
`<dir>` and prints their paths. The source archive is `git archive HEAD` of the
workspace root below a `<repository name>-<version>/` directory, so run `assemble` on a
checkout of the tagged commit. Other files in `<dir>` are ignored.

`sha256.sum` lists every archive, installer, formula, npm package, and the source
archive, sorted by file name. `release-manifest.json` holds the release plan for the tag
and the SHA-256 of each file that `sha256.sum` lists:

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

### `workflow [--check]`

Writes the [release workflow](#release-workflow) to `.github/workflows/release.yml` in
the workspace root, creating its directories, and prints its path. For each action it
uses, it keeps the `uses: <action>@<ref> # <version>` pin it finds in the existing file
and uses its own default pin only for an action the file lacks, so regenerating never
reverts a Dependabot update. With `--check`, it writes nothing, prints that the file is
up to date, and fails, naming `ag-release workflow`, when the file is missing or
differs.

## Release Workflow

`ag-release workflow` generates the GitHub Actions workflow that runs the commands.
Commit it and regenerate it after changing the configuration or `ag-release`, never by
hand: its `plan` job runs `ag-release workflow --check`, so a stale file fails every
pull request. Generate it with the same `ag-release` version as `tool`, which the jobs
install, and keep the workspace root at the repository root.

| Job             | Runs on                                                      | Does                                                                                                     |
| --------------- | ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| `plan`          | Every pull request, merge-queue check, tag push, and dry run | `plan` (with the pushed tag), `workflow --check`, and detection of release tooling changes               |
| `smoke`         | Pull requests that change release tooling                    | On each runner label of `targets`: builds, assembles, and installs the host target, and runs each binary |
| `build`         | Tag pushes and dry runs                                      | `rustup target add` and `build` for each `matrix` entry, on its runner; `musl-tools` for musl targets    |
| `assemble`      | After every build                                            | `assemble`, `notes`, and `notes --title`                                                                 |
| `distributions` | Dry runs, when configured                                    | Runs the shell installer in each image and checks its success or error                                   |
| `host`          | Tag pushes, after every `check` job and build                | Attests the files, uploads them to a draft GitHub release, and then publishes it                         |

Release tooling is the workflow file, the `[workspace.metadata.release` tables, the
`ag-release` package when the workspace builds it, and `smoke-paths`. A
`workflow_dispatch` run is a dry run: it builds every target with the configured profile
and keeps the files as workflow artifacts, but publishes nothing. Each job gets only the
token permissions it needs and checks out without persisted credentials. Jobs install
the toolchain that a `rust-toolchain.toml` pins, or use the runner's. With a musl target
configured, `build` installs `musl-tools` with `apt-get` on that target's runner, which
must be an Ubuntu runner of the target's architecture, and compiles C dependencies with
`musl-gcc` through `CC_<target>`.

Configured `jobs` call reusable workflows at one of two stages:

- `check` jobs run on tag pushes, and `host` needs them, so a failing check stops the
  release.
- `publish` jobs run after `host` on tag pushes, for example to publish packages, whose
  installers download the release's archives. They skip prereleases unless
  `prerelease = true`.

To test a workflow without publishing, as `smoke` does, build the host target with
`--profile dev`, pass the same `--target` to `plan` and `assemble`, and install from the
artifacts directory with `<APP>_DOWNLOAD_URL=file://<dir>`.

## Artifacts

The plan names every file a release publishes:

| File                           | Contents                                                                               |
| ------------------------------ | -------------------------------------------------------------------------------------- |
| `<app>-<target>.tar.gz`        | The app's binaries and `include` files in `<app>-<target>/`                            |
| `<app>-<target>.tar.gz.sha256` | `sha256sum`-compatible checksum: `<hex>  <file name>`                                  |
| `<app>-installer.sh`           | POSIX `sh` installer of the app's binaries                                             |
| `<app>.rb`                     | Homebrew formula that installs the app's archive                                       |
| `<app>-npm-package.tar.gz`     | npm package tarball, files below `package/`, that installs the app's binaries          |
| `source.tar.gz` and `.sha256`  | Source archive of the tagged commit                                                    |
| `sha256.sum`                   | `sha256sum`-compatible checksums of every file above but the archives' `.sha256` files |
| `release-manifest.json`        | The plan and the SHA-256 of every file in `sha256.sum`                                 |

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
   `aarch64-apple-darwin` under Rosetta 2. On Linux it prefers the `*-linux-gnu` target
   when the system has glibc, at least `min-glibc` when that is set (read from
   `ldd --version`), and otherwise takes the `*-linux-musl` target, which also serves
   musl systems such as Alpine. Without a musl target, musl and an old or unreadable
   glibc fail with the reason. It rejects a 32-bit userland (read from the ELF header of
   `/proc/self/exe`, else `/bin/sh`, and assumed 64-bit when neither is readable). Other
   platforms fail with the list of supported targets.
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

`install.js`, the `postinstall` script, picks the target as the
[shell installer](#shell-installer) does, reading glibc from Node's process report
instead of `ldd`, downloads `<app>-<target>.tar.gz`, fails unless its SHA-256 matches
the one `assemble` recorded, and extracts it with the system `tar`. It installs the
binaries into `<package>/node_modules/.bin_real/`, renaming each into place. This
location is stable: an app can recognize an npm, Bun, or pnpm global install by its
executable path ending in `<package>/node_modules/.bin_real/<binary>`. When install
scripts did not run, as with `npm install --ignore-scripts` or a package manager that
blocks dependency scripts, the first run of a command installs the binaries first.

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

## Homebrew Formula

`<app>.rb` is a formula for the tap that `homebrew-tap` names, `<owner>/<name>`, whose
repository is `github.com/<owner>/homebrew-<name>`. Copy it to the tap's `Formula/`
directory, for example from a `publish` job, and the app installs with:

```sh
brew install <owner>/<name>/<app>
```

Installing by this full name also trusts just that formula, which Homebrew requires of
formulas from other taps. The formula takes `desc` (without a trailing full stop),
`license`, and `version` from Cargo metadata and the repository URL as its `homepage`.
It has an `on_macos` and an `on_linux` block with an `on_arm` and an `on_intel` block
for each configured architecture, each downloading the release archive and checking the
SHA-256 that `assemble` recorded; Linux takes the `*-linux-gnu` archive of an
architecture when one is configured and the `*-linux-musl` archive otherwise. `install`
copies the binaries into the formula's `bin`. A Cargo `license` becomes `license` when
it is one SPDX identifier or identifiers joined only by `OR` (`any_of:`) or only by
`AND` (`all_of:`), and is left out otherwise. The formula has no `test` block, and
Homebrew updates it with `brew upgrade` once the tap holds a newer one.

## cargo-binstall

The `binstall` installer adds `cargo binstall <app>@<version>` to the release notes and
checks that each app's `Cargo.toml` points
[`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall) at the release
archives, so it installs them instead of building from crates.io:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz"
bin-dir = "{ name }-{ target }/{ bin }{ binary-ext }"
pkg-fmt = "tgz"
```

Drop the `v` from `pkg-url` when release tags have none. `cargo-binstall` picks the
target, including a `*-linux-musl` one on musl or old glibc, and falls back to
`cargo install` when no archive fits. The app must be published to crates.io, where
`cargo-binstall` reads the metadata.

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
- 64-bit macOS and Linux targets only: no Windows, 32-bit, or other architectures.
- No MSI or PowerShell installers or updater binaries. `ag-release` writes Homebrew
  formulas but does not push them to the tap; a `publish` job does.
- The generated workflow is for GitHub Actions only and supports custom jobs only as
  reusable workflows at the `check` and `publish` stages.
- The shell installer needs `curl` (not a snap) or `wget`, `tar` with gzip support, and
  `sha256sum` or `shasum`.
- The npm package needs Node 18 or newer, `tar` with gzip support, and `curl` when a
  proxy variable is set. Yarn Plug'n'Play must unplug it (`preferUnplugged` asks for
  that).

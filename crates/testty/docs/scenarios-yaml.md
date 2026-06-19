# YAML scenarios (no Rust required)

The `testty` command-line binary runs **declarative YAML scenarios** against any
terminal binary, so projects in any language can write end-to-end TUI tests without a
Rust harness.

```sh
testty run scenario.yaml
```

`testty run` loads the file, drives the binary under test, asserts every expectation
against the final frame, and uses the **process exit code** as the pass/fail signal (`0`
= passed, non-zero = failed or errored). Failures print the same failure messages as the
Rust assertion API.

## Scenario format

```yaml
version: 1                     # optional; if set it must match this build (defaults to 1)
name: tab switches view        # optional; names the scenario in proof output
session:
  bin: ./myapp                 # binary under test (override with --bin)
  size: [80, 24]               # optional [cols, rows]
  args: [--demo]               # optional arguments
  env: { THEME: dark }         # optional environment variables
  workdir: ./fixtures          # optional working directory
steps:
  - wait_for_stable_frame: { stable_ms: 500, timeout_ms: 5000 }
  - press_key: Tab
expect:
  - selected_tab: Sessions
  - text_in_region: { text: "Counter: 3", region: [0, 0, 80, 24] }
```

Each `steps` and `expect` entry is a **single-key map**: the key names the action or
check, and the value carries its arguments.

### Steps

- Press a key — `press_key: Tab`
- Type text — `write_text: hello`
- Sleep — `sleep_ms: 250`
- Wait for text — `wait_for_text: { needle: Ready, timeout_ms: 5000 }`
- Wait for a stable frame —
  `wait_for_stable_frame: { stable_ms: 500, timeout_ms: 5000 }`
- Poll an expectation —
  `eventually: { match: { not_visible: Loading }, timeout_ms: 3000, poll_ms: 50 }`
- Capture (for proof) — `capture`
- Capture with a label —
  `capture_labeled: { label: start, description: "Initial view" }`

`capture` and `capture_labeled` are accepted but produce no output through `testty run`
yet — proof artifacts are only emitted by the Rust proof API, and the CLI `--proof` flag
is not wired up (see [CLI](#cli)). They are inert in YAML runs until then.

### Expectations

- Selected tab — `selected_tab: Sessions`
- Unselected tab — `unselected_tab: Projects`
- Instruction visible — `instruction_visible: "Press Enter"`
- Keybinding hint — `keybinding_hint: "q quit"`
- Footer action — `footer_action: Submit`
- Dialog title — `dialog_title: Confirm`
- Status message — `status_message: Saved`
- Text not visible — `not_visible: Error`
- Text within a region —
  `text_in_region: { text: "Counter: 3", region: [col, row, width, height] }`

`expect` entries assert against the **final** frame after all steps run. To assert
mid-scenario, use the `eventually` step, which polls an expectation while the scenario
is running.

## CLI

```sh
testty run <scenario.yaml> [--bin <BIN>] [--proof <DIR>]
```

- `--bin` overrides `session.bin` (handy for pointing one scenario at different builds).
- `--proof` is reserved for proof-artifact output and is not yet wired up.

## Versioning

`version` is required to match the format this build supports. A file with an
unrecognized version is rejected with a clear error rather than misbehaving, so
scenarios stay forward-compatible across `testty` releases.

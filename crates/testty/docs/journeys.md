# Journeys

`Journey` is a reusable, composable step block. Build scenarios declaratively instead of
repeating low-level steps.

```rust
use testty::prelude::*;

let startup  = Journey::wait_for_startup_default();
let navigate = Journey::navigate_with_key("Tab", "Settings", 3_000);
let input    = Journey::type_and_confirm("search query");
let snapshot = Journey::capture_labeled("final", "End state");

let scenario = Scenario::new("settings_search")
    .compose(&startup)
    .compose(&navigate)
    .compose(&input)
    .compose(&snapshot);
```

## Built-in journeys

- **`wait_for_startup_default()`** — wait with the balanced default preset
- **`wait_for_startup_preset(StartupWait)`** — wait with a named preset
- **`wait_for_startup(stable_ms, timeout_ms)`** — raw-number entry point
- **`navigate_with_key(key, expected, timeout)`** — press key, wait for expected text
- **`type_and_confirm(text)`** — type text, press Enter
- **`press_and_wait(key, ms)`** — press key, sleep briefly
- **`capture_labeled(label, desc)`** — capture for proofs

## Startup-wait presets

- **`StartupWait::Default`** — 300ms stable / 5 000ms timeout. Balanced; matches
  historical defaults.
- **`StartupWait::FastNative`** — 200ms / 3 000ms. Native binaries (Rust TUIs).
- **`StartupWait::SlowNode`** — 500ms / 10 000ms. Node-based CLIs.
- **`StartupWait::Custom { stable_ms, timeout_ms }`** — explicit values for anything
  else.

```rust
Journey::wait_for_startup_preset(StartupWait::FastNative);
Journey::wait_for_startup_preset(StartupWait::Custom {
    stable_ms: 250,
    timeout_ms: 4_000,
});
```

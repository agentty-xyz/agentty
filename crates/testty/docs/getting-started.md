# Getting started

## Install

```toml
[dev-dependencies]
testty = "0.9"
tempfile = "3"
```

## First test

```rust
use testty::prelude::*;

#[test]
fn startup_shows_welcome() {
    let temp = tempfile::TempDir::new().unwrap();
    let builder = PtySessionBuilder::new(env!("CARGO_BIN_EXE_myapp"))
        .size(80, 24)
        .env("MY_APP_ROOT", temp.path().to_string_lossy())
        .workdir(temp.path());

    let scenario = Scenario::new("startup")
        .wait_for_stable_frame(500, 5_000)
        .capture();

    let frame = scenario.run(builder).expect("scenario failed");

    testty::recipe::expect_instruction_visible(&frame, "Welcome");
}
```

## Run

```sh
cargo test -p my-app --test e2e
```

Cargo builds the binary before integration tests, so `CARGO_BIN_EXE_<name>` is always
available.

Next: [Scenarios](scenarios.md).

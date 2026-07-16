#!/bin/bash -eu

cd "$SRC/agentty"

# Full LTO breaks cargo-fuzz sanitizer coverage instrumentation.
CARGO_PROFILE_RELEASE_LTO=false cargo fuzz build -O protocol_response
cp target/x86_64-unknown-linux-gnu/release/protocol_response "$OUT/"

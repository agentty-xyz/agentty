FROM gcr.io/oss-fuzz-base/base-builder-rust:v1@sha256:d1b7545a6a5680d66886e9ee729f6575e0e885143a1bb74289f14e7e91de3463

RUN groupadd --gid 10001 fuzz-builder \
    && useradd --create-home --uid 10001 --gid 10001 fuzz-builder \
    && install -d --owner 10001 --group 10001 "$WORK/cargo" /rustc

# The OSS-Fuzz wrapper prepares these paths before invoking build.sh.
ENV CARGO_HOME="$WORK/cargo" \
    LIB_FUZZING_ENGINE_DEPRECATED="$WORK/libFuzzingEngine.a"

COPY --chown=fuzz-builder:fuzz-builder Cargo.toml Cargo.lock "$SRC/agentty/"
COPY --chown=fuzz-builder:fuzz-builder \
    crates/ag-protocol/Cargo.toml \
    crates/ag-protocol/askama.toml \
    "$SRC/agentty/crates/ag-protocol/"
COPY --chown=fuzz-builder:fuzz-builder \
    crates/ag-protocol/src/envelope.rs \
    crates/ag-protocol/src/lib.rs \
    crates/ag-protocol/src/model.rs \
    crates/ag-protocol/src/parse.rs \
    crates/ag-protocol/src/prompt.rs \
    crates/ag-protocol/src/question.rs \
    crates/ag-protocol/src/schema.rs \
    "$SRC/agentty/crates/ag-protocol/src/"
COPY --chown=fuzz-builder:fuzz-builder \
    crates/ag-protocol/src/template/protocol_instruction_policy_prompt.md \
    crates/ag-protocol/src/template/protocol_instruction_prompt.md \
    crates/ag-protocol/src/template/protocol_instruction_session_turn_usage.md \
    crates/ag-protocol/src/template/protocol_instruction_utility_prompt_usage.md \
    crates/ag-protocol/src/template/protocol_refresh_prompt.md \
    crates/ag-protocol/src/template/protocol_refresh_session_turn_instruction.md \
    crates/ag-protocol/src/template/protocol_refresh_utility_prompt_instruction.md \
    crates/ag-protocol/src/template/protocol_repair_prompt.md \
    crates/ag-protocol/src/template/questions_field_description.md \
    "$SRC/agentty/crates/ag-protocol/src/template/"
COPY --chown=fuzz-builder:fuzz-builder fuzz/Cargo.toml "$SRC/agentty/fuzz/"
COPY --chown=fuzz-builder:fuzz-builder fuzz/fuzz_targets/protocol_response.rs "$SRC/agentty/fuzz/fuzz_targets/"
COPY --chown=fuzz-builder:fuzz-builder .clusterfuzzlite/build.sh "$SRC/"

WORKDIR "$SRC/agentty"
USER fuzz-builder

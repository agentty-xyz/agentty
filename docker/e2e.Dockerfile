# Canonical `linux/amd64` environment for the Agentty end-to-end feature suite.
#
# CI builds this image and runs the `test-agentty-e2e` hook inside it with a
# read-only checkout, so `TESTTY_GIF_MODE=check` verifies committed GIF hash
# sidecars without rewriting anything. Developers record or refresh feature
# artifacts in the same image with a writable mount and
# `TESTTY_GIF_MODE=generate` (see `skills/feature-test/SKILL.md`), which keeps
# committed hashes portable between local recording and CI verification.
# Every tool is pinned; upgrade pins deliberately and re-record affected GIFs.
FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    TZ=UTC \
    # Chromium runs as root inside the container, so VHS must disable the
    # Chromium sandbox to launch it.
    VHS_NO_SANDBOX=true

# Build toolchain plus the VHS recording stack: `ttyd`, `ffmpeg`, Chromium,
# and the JetBrains Mono font VHS renders with by default, all from the pinned
# Debian release. `check` mode needs none of the recording stack, but one
# shared image for checking and recording is what makes the hashes portable.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    chromium \
    curl \
    ffmpeg \
    fonts-dejavu \
    fonts-jetbrains-mono \
    fonts-noto-color-emoji \
    git \
    pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    # CI bind-mounts a checkout owned by the host runner user while the
    # container runs as root; without this git refuses to operate on the
    # repository and `prek` cannot enumerate files.
    && git config --system --add safe.directory /workspace

# Pin the nightly toolchain to a date so image rebuilds keep the same
# compiler. `RUSTUP_TOOLCHAIN` overrides the floating `nightly` channel from
# `rust-toolchain.toml` inside the container, so rustup never downloads a
# newer nightly at run time. Bump the date deliberately and re-verify the
# committed GIF hash sidecars.
ARG RUST_TOOLCHAIN=nightly-2026-07-15
ENV RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}
RUN curl -fsSL https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}"

ARG NEXTEST_VERSION=0.9.140
RUN curl -fsSL "https://get.nexte.st/${NEXTEST_VERSION}/linux" \
    | tar -xz -C "${CARGO_HOME}/bin"

# The `prek` pin matches `.github/actions/setup-rust-prek/action.yml`.
ARG PREK_VERSION=0.4.3
RUN mkdir /tmp/prek \
    && curl -fsSL "https://github.com/j178/prek/releases/download/v${PREK_VERSION}/prek-x86_64-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /tmp/prek \
    && install -m 0755 "$(find /tmp/prek -type f -name prek)" /usr/local/bin/prek \
    && rm -rf /tmp/prek

# Debian bookworm does not package `ttyd`, so pin the upstream static binary.
ARG TTYD_VERSION=1.7.7
RUN curl -fsSL "https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/ttyd.x86_64" \
    -o /usr/local/bin/ttyd \
    && chmod 0755 /usr/local/bin/ttyd

ARG VHS_VERSION=0.11.0
RUN mkdir /tmp/vhs \
    && curl -fsSL "https://github.com/charmbracelet/vhs/releases/download/v${VHS_VERSION}/vhs_${VHS_VERSION}_Linux_x86_64.tar.gz" \
    | tar -xz -C /tmp/vhs \
    && install -m 0755 "$(find /tmp/vhs -type f -name vhs)" /usr/local/bin/vhs \
    && rm -rf /tmp/vhs

WORKDIR /workspace

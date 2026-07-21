# Canonical `linux/amd64` environment for the Agentty end-to-end feature suite.
#
# CI builds this image and runs the `test-agentty-e2e` hook inside it with a
# read-only checkout, so `TESTTY_GIF_MODE=check` verifies committed GIF hash
# sidecars without rewriting anything. Developers record or refresh feature
# artifacts in the same image with a writable mount and
# `TESTTY_GIF_MODE=generate` (see `skills/feature-test/SKILL.md`), which keeps
# committed hashes portable between local recording and CI verification.
# Every tool is pinned; upgrade pins deliberately and re-verify the committed
# GIF hash sidecars.
FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_TARGET_DIR=/opt/target \
    PATH=/usr/local/cargo/bin:$PATH \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    TZ=UTC \
    # Unprivileged containers lack the kernel privileges Chromium's own
    # sandbox needs, so VHS must disable it to launch Chromium.
    VHS_NO_SANDBOX=true

# Build toolchain plus the VHS recording stack: `ffmpeg`, Chromium, and the
# JetBrains Mono font VHS renders with by default, all from the pinned Debian
# release (`ttyd` is pinned separately below). `check` mode needs none of the
# recording stack, but one shared image for checking and recording is what
# makes the hashes portable.
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
    # CI bind-mounts a checkout owned by the host runner user; keep git
    # working when that owner differs from the container user, so `prek` can
    # enumerate files.
    && git config --system --add safe.directory /workspace

# Pin the nightly toolchain to a date so image rebuilds keep the same
# compiler. `RUSTUP_TOOLCHAIN` overrides the floating `nightly` channel from
# `rust-toolchain.toml` inside the container, so rustup never downloads a
# newer nightly at run time. Bump the date deliberately and re-verify the
# committed GIF hash sidecars.
ARG RUST_TOOLCHAIN=nightly-2026-07-15
ENV RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}

# Install rustup from a pinned, checksum-verified `rustup-init` binary
# instead of piping the install script into a shell. The digest comes from
# the official `rustup-init.sha256` published next to the binary.
ARG RUSTUP_VERSION=1.29.0
ARG RUSTUP_INIT_SHA256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
RUN curl --proto '=https' --tlsv1.2 -fsSL \
    "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
    -o /tmp/rustup-init \
    && echo "${RUSTUP_INIT_SHA256} */tmp/rustup-init" | sha256sum -c - \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rm /tmp/rustup-init

ARG NEXTEST_VERSION=0.9.140
ARG NEXTEST_SHA256=4ee9aaa0d0171a985a5d0eb735b87355894c1c455972e9674fb9fdbd1387c9a3
RUN curl --proto '=https' --tlsv1.2 -fsSL \
    "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
    -o /tmp/nextest.tar.gz \
    && echo "${NEXTEST_SHA256} */tmp/nextest.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/nextest.tar.gz -C "${CARGO_HOME}/bin" \
    && rm /tmp/nextest.tar.gz

# The `prek` pin matches `.github/actions/setup-rust-prek/action.yml`.
ARG PREK_VERSION=0.4.3
ARG PREK_SHA256=8a8210d64476657cac3e797afa109011d8d872c09e3a407f50c5a4dde063b381
RUN mkdir /tmp/prek \
    && curl --proto '=https' --tlsv1.2 -fsSL "https://github.com/j178/prek/releases/download/v${PREK_VERSION}/prek-x86_64-unknown-linux-gnu.tar.gz" \
    -o /tmp/prek.tar.gz \
    && echo "${PREK_SHA256} */tmp/prek.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/prek.tar.gz -C /tmp/prek \
    && install -m 0755 "$(find /tmp/prek -type f -name prek)" /usr/local/bin/prek \
    && rm -rf /tmp/prek /tmp/prek.tar.gz

# Debian bookworm does not package `ttyd`, so pin the upstream static binary.
ARG TTYD_VERSION=1.7.7
ARG TTYD_SHA256=8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55
RUN curl --proto '=https' --tlsv1.2 -fsSL "https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/ttyd.x86_64" \
    -o /tmp/ttyd \
    && echo "${TTYD_SHA256} */tmp/ttyd" | sha256sum -c - \
    && install -m 0755 /tmp/ttyd /usr/local/bin/ttyd \
    && rm /tmp/ttyd

ARG VHS_VERSION=0.11.0
ARG VHS_SHA256=99cb634587eaae0473c1ea377db80c3a048c27f99fe0a7febb1a1e8cb7ee5009
RUN mkdir /tmp/vhs \
    && curl --proto '=https' --tlsv1.2 -fsSL "https://github.com/charmbracelet/vhs/releases/download/v${VHS_VERSION}/vhs_${VHS_VERSION}_Linux_x86_64.tar.gz" \
    -o /tmp/vhs.tar.gz \
    && echo "${VHS_SHA256} */tmp/vhs.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/vhs.tar.gz -C /tmp/vhs \
    && install -m 0755 "$(find /tmp/vhs -type f -name vhs)" /usr/local/bin/vhs \
    && rm -rf /tmp/vhs /tmp/vhs.tar.gz

# Run the suite as an unprivileged user. Uid 1001 matches the GitHub Actions
# runner user that owns the bind-mounted checkout. Cargo only needs write
# access to its registry caches and the baked-in target directory; the
# toolchain itself stays root-owned and read-only.
RUN useradd --uid 1001 --user-group --create-home agentty \
    && install -d -o agentty -g agentty \
    "${CARGO_HOME}/registry" "${CARGO_HOME}/git" "${CARGO_TARGET_DIR}"

USER agentty
ENV HOME=/home/agentty

WORKDIR /workspace

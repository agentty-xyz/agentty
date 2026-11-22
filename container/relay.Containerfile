# Self-hostable image of the remote-control relay, `ag-relay`.
#
# The image holds one statically linked binary, which also serves the pairing
# page, and nothing else: no shell and no system libraries. Build it from the
# repository root, where `.containerignore` keeps the context small:
#
#   podman build --file container/relay.Containerfile --tag agentty-relay .
#
# The relay listens on port 8787 inside the container; publish that port to
# choose the outside one. Arguments after the image name go to the relay, such
# as `--client-address-header X-Real-IP` behind a reverse proxy.
#
# The relay speaks plain HTTP and does not limit plain TCP connections or slow
# request headers. Run it behind a reverse proxy that terminates TLS and limits
# connections.
#
# The base image and `rustup` are pinned like `container/e2e.Containerfile`;
# upgrade both files together.
FROM debian@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS build

ARG TARGETARCH

ARG RUSTUP_VERSION=1.29.0
ARG RUSTUP_INIT_SHA256_AMD64=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
ARG RUSTUP_INIT_SHA256_ARM64=9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gcc \
    libc6-dev \
    musl-tools \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY rust-toolchain.toml ./

# Install the toolchain `rust-toolchain.toml` pins, with the musl target for a
# static binary. Source changes below reuse this layer.
RUN case "${TARGETARCH}" in \
    amd64) \
        rust_host=x86_64-unknown-linux-gnu; \
        rust_target=x86_64-unknown-linux-musl; \
        rustup_init_sha256="${RUSTUP_INIT_SHA256_AMD64}" \
        ;; \
    arm64) \
        rust_host=aarch64-unknown-linux-gnu; \
        rust_target=aarch64-unknown-linux-musl; \
        rustup_init_sha256="${RUSTUP_INIT_SHA256_ARM64}" \
        ;; \
    *) \
        echo "unsupported target architecture: ${TARGETARCH}" >&2; \
        exit 1 \
        ;; \
    esac \
    && readonly https_only='=https' \
    && curl --proto "${https_only}" --proto-redir "${https_only}" --fail --location --silent --show-error \
        --output /rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${rust_host}/rustup-init" \
    && echo "${rustup_init_sha256} */rustup-init" | sha256sum -c - \
    && chmod +x /rustup-init \
    && rust_channel="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)" \
    && /rustup-init -y --no-modify-path --profile minimal \
        --default-toolchain "${rust_channel}" --target "${rust_target}" \
    && rm /rustup-init \
    && echo "${rust_target}" > /rust-target

COPY Cargo.toml Cargo.lock ./
COPY crates crates

# Build only the relay.
RUN rust_target="$(cat /rust-target)" \
    && cargo build --locked --release --package ag-relay --target "${rust_target}" \
    && cp "target/${rust_target}/release/ag-relay" /ag-relay

FROM scratch

LABEL org.opencontainers.image.source="https://github.com/agentty-xyz/agentty" \
      org.opencontainers.image.description="Relay for Agentty remote control"

COPY --from=build /ag-relay /ag-relay

# An unprivileged user; the relay writes no files.
USER 65532:65532

EXPOSE 8787

# The bind address is part of the entry point, so arguments given at run time
# add to it instead of replacing it.
ENTRYPOINT ["/ag-relay", "--listen", "0.0.0.0:8787"]

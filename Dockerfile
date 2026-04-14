# Stage 1: Build the Rust binaries.
#
# Uses stable Rust (matching CI) with lld linker for fast linking.
# Node.js 22 + TypeScript 6.x are required because agent packages
# contain TypeScript sources compiled during the build pipeline.
FROM rust:1.86-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev libclang-dev lld \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g typescript@6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy manifests first for better layer caching.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/

# Override to stable (rust-toolchain.toml pins nightly for local dev).
ENV RUSTUP_TOOLCHAIN=stable
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=lld"

# Override workspace LTO profile: full LTO needs >8GB RAM during linking.
# Thin LTO produces equivalent binaries within constrained build environments.
ENV CARGO_PROFILE_RELEASE_LTO=thin
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

RUN cargo build --release -p baml-agent-runner --features http-tools \
    && cargo build --release -p baml-rt-builder --features http-tools --bin baml-agent-builder

# Stage 2: Slim runtime image.
#
# Node.js is needed at runtime because baml-agent-builder compiles
# TypeScript when agents are published via POST /deploy.
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g typescript@6 \
    && rm -rf /var/lib/apt/lists/*

ENV NPM_CONFIG_CACHE=/tmp/npm-cache

RUN groupadd -r agentium && useradd -r -g agentium -d /data -s /sbin/nologin agentium
RUN mkdir -p /data && chown -R agentium:agentium /data

COPY --from=builder /build/target/release/baml-agent-runner /usr/local/bin/
COPY --from=builder /build/target/release/baml-agent-builder /usr/local/bin/

# ONNX models for embedding/drift detection (git-lfs tracked).
# Fail fast if LFS pointers were not resolved (e.g. missing `git lfs pull`).
COPY models/fastembed /models/fastembed
RUN find /models/fastembed -name '*.onnx' -exec sh -c \
    'head -c 7 "$1" | grep -q "version" && echo "ERROR: $1 is an LFS pointer stub, run git lfs pull" && exit 1 || true' _ {} \;
ENV BAML_MODELS_DIR=/models

USER 1000

EXPOSE 18080

ENTRYPOINT ["baml-agent-runner"]
CMD ["--serve-http", "0.0.0.0:18080"]

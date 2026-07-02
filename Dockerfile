# Stage 1: Build the Rust binaries.
#
# Uses stable Rust (matching CI) with lld linker for fast linking.
# Node.js 22 + TypeScript 6.x are required because agent packages
# contain TypeScript sources compiled during the build pipeline.
FROM rust:1.86-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev libdbus-1-dev libcap-ng-dev \
    libclang-dev lld \
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

RUN cargo build --release -p agentium --features http-tools,memory,sandbox-provider,dev-tools

# Stage 2: Slim runtime image.
#
# Node.js is needed at runtime because repository publish compiles
# TypeScript when agents are published via POST /repository/publish.
FROM debian:bookworm-slim AS runtime

ARG VERSION=dev
LABEL org.opencontainers.image.version="${VERSION}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 libdbus-1-3 libcap-ng0 curl \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g typescript@6 \
    && rm -rf /var/lib/apt/lists/*

ENV NPM_CONFIG_CACHE=/tmp/npm-cache

RUN groupadd -r agentium && useradd -r -g agentium -d /data -s /sbin/nologin agentium
RUN mkdir -p /data && chown -R agentium:agentium /data

COPY --from=builder /build/target/release/agentium /usr/local/bin/

# Host-owned context compaction BAML (SummarizeConversationPrefix).
COPY baml_src/host /opt/agentium/baml_src/host
ENV BAML_HOST_SCHEMA_DIR=/opt/agentium

# ONNX models for embedding/drift detection (git-lfs tracked).
# Fail fast if LFS pointers were not resolved (e.g. missing `git lfs pull`).
COPY models/fastembed /models/fastembed
RUN stubs=$(find /models/fastembed -name '*.onnx' -exec sh -c \
      'head -c 7 "$1" | grep -q "^version" && echo "$1"' _ {} \;); \
    if [ -n "$stubs" ]; then \
      echo "ERROR: ONNX models are LFS pointer stubs (run 'git lfs pull'):" >&2; \
      echo "$stubs" >&2; \
      exit 1; \
    fi
ENV BAML_MODELS_DIR=/models

USER 1000

EXPOSE 18080

ENTRYPOINT ["agentium", "serve"]
CMD ["--serve-http", "0.0.0.0:18080"]

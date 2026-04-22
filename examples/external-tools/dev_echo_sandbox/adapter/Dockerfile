# syntax=docker/dockerfile:1.7
#
# Bind-rootfs note (for local microsandbox bind mode):
#   CID=$(docker create dev-echo-sandbox:local)
#   mkdir -p .tmp/dev-echo-rootfs
#   docker export "$CID" | tar -x -C .tmp/dev-echo-rootfs
#   docker rm "$CID"
# Use .tmp/dev-echo-rootfs as runtime.image.path (kind=bind).

FROM rust:slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install nightly --profile minimal \
    && rustup target add --toolchain nightly x86_64-unknown-linux-musl

WORKDIR /build
COPY . .

RUN CARGO_TARGET_DIR=/build/target cargo build --release \
    --manifest-path examples/external-tools/dev_echo_sandbox/adapter/Cargo.toml \
    --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/dev-echo-sandbox-adapter /tool-adapter
ENTRYPOINT ["/tool-adapter"]

# syntax=docker/dockerfile:1.7

FROM rust:slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .

RUN cargo build --release --locked \
    -p sandbox-echo-adapter \
    --target x86_64-unknown-linux-musl

FROM gcr.io/distroless/static
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/sandbox-echo-adapter /tool-adapter
ENTRYPOINT ["/tool-adapter"]

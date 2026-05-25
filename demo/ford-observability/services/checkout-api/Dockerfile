# syntax=docker/dockerfile:1.7
FROM rust:1.88-slim-bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
COPY rust-toolchain.toml Cargo.toml ./
COPY services ./services
RUN cargo build --release --bin checkout-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/checkout-api /usr/local/bin/checkout-api
EXPOSE 8080
ENV PORT=8080
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/checkout-api"]

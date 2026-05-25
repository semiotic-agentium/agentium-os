FROM rust:1.88-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY services ./services
RUN cargo build --release -p failure-harness

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/failure-harness /usr/local/bin/failure-harness
ENV PORT=8080 LEDGER_DB_PATH=/data/failure-harness/ledger.sqlite
EXPOSE 8080
CMD ["failure-harness"]

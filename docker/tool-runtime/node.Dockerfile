# SPDX-FileCopyrightText: 2026 Karakara Labs
# SPDX-License-Identifier: Apache-2.0
#
# Node runtime image for MCP toolsets. Bundles vendor their MCP SDK and other
# dependencies under code/node_modules. Bump supervisor_abi in
# default_node_runtime_image() when this contract changes.

# syntax=docker/dockerfile:1

FROM rustlang/rust:nightly-bookworm AS supervisor-build
WORKDIR /src
COPY . .
# BuildKit cache mounts keep the cargo registry and target/ across image
# rebuilds, so iterating on the supervisor recompiles only what changed
# (seconds) instead of a cold ~12 min build every time. The mounts are
# ephemeral (not part of the image), so copy the binary out to a real path
# for the next stage to COPY from.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p baml-tool-supervisor \
    && cp /src/target/release/baml-tool-supervisor /baml-tool-supervisor

FROM node:22-bookworm-slim
RUN groupadd --gid 100000 ktool \
    && useradd --uid 100000 --gid 100000 --no-create-home --shell /usr/sbin/nologin ktool \
    && install -d -m 0755 /tool \
    && install -d -m 0700 /run/ktool

COPY --from=supervisor-build /baml-tool-supervisor /usr/local/bin/baml-tool-supervisor

ENV PATH=/usr/local/bin:/usr/bin:/bin
ENTRYPOINT ["/usr/local/bin/baml-tool-supervisor"]

# SPDX-FileCopyrightText: 2026 Karakara Labs
# SPDX-License-Identifier: Apache-2.0
#
# Node runtime image for MCP toolsets. Bundles vendor their MCP SDK and other
# dependencies under code/node_modules. Bump supervisor_abi in
# default_node_runtime_image() when this contract changes.

# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim
RUN groupadd --gid 100000 ktool \
    && useradd --uid 100000 --gid 100000 --no-create-home --shell /usr/sbin/nologin ktool \
    && install -d -m 0755 /tool \
    && install -d -m 0700 /run/ktool

COPY target/release/baml-tool-supervisor /usr/local/bin/baml-tool-supervisor

ENV PATH=/usr/local/bin:/usr/bin:/bin
ENTRYPOINT ["/usr/local/bin/baml-tool-supervisor"]

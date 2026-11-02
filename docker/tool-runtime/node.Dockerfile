# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0
#
# The blessed `node` runtime image for external tools v2 (P4.9).
#
# Contract (bump `supervisor_abi` in `default_node_runtime_image()` on any
# change here — it changes every tool digest by design):
# - node 22 on PATH (`/usr/local/bin/node`)
# - the guest supervisor as PID 1 / entrypoint
# - `@karakara/tool` at /node_modules/@karakara/tool
# - empty `/tool` mount point, private `/run/ktool` (tmpfs mounted by the
#   supervisor when it runs as root)
# - uid/gid range 100000+ for per-tool workers
# - the seccomp profile is compiled into the supervisor binary (pinned by
#   `supervisor_abi`), so it cannot drift from the image.

# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim AS sdk
WORKDIR /sdk
COPY ts/packages/tool/package.json ts/packages/tool/package-lock.json ./
RUN npm ci --omit=dev --no-audit --no-fund
COPY ts/packages/tool/tsconfig.json ./
COPY ts/packages/tool/src ./src
RUN npm install --no-save --no-audit --no-fund typescript@6 \
    && ./node_modules/.bin/tsc -p tsconfig.json \
    && rm -rf src tsconfig.json node_modules/typescript node_modules/.bin

FROM node:22-bookworm-slim
RUN groupadd --gid 100000 ktool \
    && useradd --uid 100000 --gid 100000 --no-create-home --shell /usr/sbin/nologin ktool \
    && install -d -m 0755 /tool /node_modules \
    && install -d -m 0700 /run/ktool

COPY --from=sdk /sdk /node_modules/@karakara/tool
COPY target/release/baml-tool-supervisor /usr/local/bin/baml-tool-supervisor

ENV PATH=/usr/local/bin:/usr/bin:/bin
ENTRYPOINT ["/usr/local/bin/baml-tool-supervisor"]

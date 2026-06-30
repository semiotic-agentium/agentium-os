// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import type { ProxyOptions } from "vite";

// Must match `justfile` `runner_http_bind` / typical local `agentium serve --serve-http`.
const RUNNER_ORIGIN = "http://127.0.0.1:18080";

/** Proxy to baml-agent-runner; disable gzip on agent/API routes so SSE (`POST .../a2a`) is not buffered. */
function runnerProxy(): ProxyOptions {
  return {
    target: RUNNER_ORIGIN,
    changeOrigin: true,
    /** Long agent turns + buffered SSE responses can exceed default proxy/socket timeouts. */
    timeout: 0,
    proxyTimeout: 0,
    configure(proxy) {
      proxy.on("proxyReq", (proxyReq, req) => {
        const url = req.url ?? "";
        // `/agents/.../a2a` does not contain "sse" — still needs identity encoding for streaming bodies.
        if (
          url.startsWith("/agents") ||
          url.includes("sse") ||
          url.includes("/conversation-history/stream") ||
          url.includes("/observe/stream")
        ) {
          proxyReq.setHeader("accept-encoding", "identity");
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5173,
    host: true, // listen on 0.0.0.0 so 127.0.0.1 and localhost both work
    proxy: {
      "/agents": runnerProxy(),
      "/config": runnerProxy(),
      "/openapi.json": runnerProxy(),
      "/mermaid": runnerProxy(),
      "/contexts": runnerProxy(),
      "/provenance": runnerProxy(),
      "/tasks": runnerProxy(),
      "/deploy": runnerProxy(),
      "/undeploy": runnerProxy(),
      "/deployments": runnerProxy(),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { defineConfig, loadEnv } from "vite";
import vue from "@vitejs/plugin-vue";
import type { ProxyOptions } from "vite";

/** Must match `justfile` `runner_http_bind` / typical local `agentium serve --serve-http`. */
const DEFAULT_RUNNER_ORIGIN = "http://127.0.0.1:18080";

/** Proxy to Agentium OS instance; disable gzip on agent/API routes so SSE is not buffered. */
function runnerProxy(target: string): ProxyOptions {
  return {
    target,
    changeOrigin: true,
    timeout: 0,
    proxyTimeout: 0,
    configure(proxy) {
      proxy.on("proxyReq", (proxyReq, req) => {
        const url = req.url ?? "";
        if (
          url.startsWith("/agents") ||
          url.includes("sse") ||
          url.includes("/conversation-history/stream") ||
          url.includes("/observe/stream") ||
          url.includes("/episode/stream")
        ) {
          proxyReq.setHeader("accept-encoding", "identity");
        }
      });
    },
  };
}

const PROXY_PREFIXES = [
  "/agents",
  "/config",
  "/openapi.json",
  "/mermaid",
  "/contexts",
  "/provenance",
  "/tasks",
  "/deploy",
  "/undeploy",
  "/deployments",
  "/repository",
  "/events",
  "/event-dispatch",
  "/message-shapes",
  "/healthz",
  "/eval",
] as const;

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const runnerOrigin = env.VITE_INSTANCE_URL?.trim() || DEFAULT_RUNNER_ORIGIN;
  const proxy: Record<string, ProxyOptions> = {};
  for (const prefix of PROXY_PREFIXES) {
    proxy[prefix] = runnerProxy(runnerOrigin);
  }

  return {
    plugins: [vue()],
    server: {
      port: 5173,
      host: true,
      proxy,
    },
    build: {
      outDir: "dist",
      emptyOutDir: true,
    },
  };
});

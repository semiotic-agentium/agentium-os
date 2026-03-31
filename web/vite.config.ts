import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import type { ProxyOptions } from "vite";

// Same origin as `justfile` `runner_http_bind` and `baml-agent-runner` defaults (127.0.0.1:8080).
const RUNNER_ORIGIN = "http://127.0.0.1:8080";

/** Proxy to baml-agent-runner; avoid gzip on SSE so chunks are not buffered by the dev proxy. */
function runnerProxy(): ProxyOptions {
  return {
    target: RUNNER_ORIGIN,
    changeOrigin: true,
    configure(proxy) {
      proxy.on("proxyReq", (proxyReq, req) => {
        if (req.url?.includes("sse")) {
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
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});

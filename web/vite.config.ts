import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

const runnerUrl = process.env.A2A_RUNNER_URL || "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5173,
    proxy: {
      "/agents": {
        target: runnerUrl,
        changeOrigin: true,
      },
      "/openapi.json": {
        target: runnerUrl,
        changeOrigin: true,
      },
      "/mermaid": {
        target: runnerUrl,
        changeOrigin: true,
      },
      "/contexts": {
        target: runnerUrl,
        changeOrigin: true,
      },
      "/tasks": {
        target: runnerUrl,
        changeOrigin: true,
      },
      "/contexts": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});

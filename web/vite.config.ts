import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  server: {
    port: 5173,
    proxy: {
      "/agents": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
      },
      "/openapi.json": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
      },
      "/mermaid": {
        target: "http://127.0.0.1:8080",
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

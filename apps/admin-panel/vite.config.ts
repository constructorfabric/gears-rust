import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Production: the SPA is served under the API Gateway prefix at /cf/admin by
// the example server (tower-http ServeDir), so base = /cf/admin/.
//
// Dev: serve the SPA from root (/) and proxy /cf to the local example server
// (`make admin`, port 8087). Serving from root avoids the dev proxy's /cf rule
// swallowing the SPA's own /cf/admin asset paths, while same-origin /cf API
// calls still proxy correctly (no CORS).
export default defineConfig(({ command }) => ({
  base: command === "serve" ? "/" : "/cf/admin/",
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/cf": {
        target: "http://127.0.0.1:8087",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
  },
}));

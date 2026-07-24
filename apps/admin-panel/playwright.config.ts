import { defineConfig } from "@playwright/test";

// Local browser smoke for the admin SPA. Prerequisite: the Gears backend must
// be running on :8087 (run `make admin` from the repo root). Playwright starts
// the Vite dev server itself (which proxies /cf to the backend).
//
// Run:  npx playwright test     (from apps/admin-panel)
//
// Not wired into the Rust CI gate — it needs a browser + a running backend.
export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  reporter: "list",
  use: {
    baseURL: "http://localhost:5173",
    headless: true,
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: true,
    timeout: 30_000,
  },
});

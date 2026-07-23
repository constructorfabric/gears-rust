import React from "react";
import ReactDOM from "react-dom/client";

import { ensureRegistryResolved } from "./resources";

// Resolve the resource registry (CRUD paths + fields) from the public
// aggregated OpenAPI spec BEFORE the app module evaluates: `App` builds its
// routes and navigation from each descriptor's resolved `paths` at module load,
// so resolution must complete first. Best-effort — if the spec is unreachable
// the app still mounts (the auth gate retries), only route shaping degrades.
async function bootstrap(): Promise<void> {
  await ensureRegistryResolved();
  const { App } = await import("./App");
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void bootstrap();

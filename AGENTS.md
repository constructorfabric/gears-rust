<!-- @cf:root-agents -->
```toml
cf-studio-path = ".cf-studio"
```

ALWAYS resolve and enforce prerequisites of skills/workflows/commands BEFORE applying user intent.
<!-- /@cf:root-agents -->

These instructions are for AI assistants working in this project.

If the instruction sounds unclear, vague or requires more context. Ask for clarification.

Always open `@/guidelines/README.md` first (entry point for project-wide guidelines).

Open additional docs only when relevant:

- If the task adds/changes dependencies (Cargo.toml), introduces a new crate, involves working with 3rd-party crates (such as those for serialization/deserialization), open `@/guidelines/DEPENDENCIES.md`.

- If the task touches ToolKit/Gear architecture (Gear layout, `@/lib/toolkit*`, plugins, REST wiring, ClientHub, OpenAPI, lifecycle/stateful tasks, SSE, standardized HTTP errors), open `@/docs/toolkit_unified_system/README.md`.

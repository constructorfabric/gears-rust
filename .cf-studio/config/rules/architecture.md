---
cf: true
type: project-rule
topic: architecture
generated-by: auto-config
version: 1.0
---
# Architecture
Use these rules when changing module boundaries, ModKit integration, lifecycle, plugins, or cross-cutting platform design.

<!-- toc -->

- [Critical Files](#critical-files)
- [Module Boundaries](#module-boundaries)
  - [Follow DDD-light module layout](#follow-ddd-light-module-layout)
  - [Register provider clients during module init](#register-provider-clients-during-module-init)
  - [Keep plugin implementations behind main module public APIs](#keep-plugin-implementations-behind-main-module-public-apis)

<!-- /toc -->

## Critical Files
| File | Why |
|---|---|
| `Cargo.toml` | Workspace membership, shared deps, lint policy. |
| `apps/cyberware-example-server/src/main.rs` | Main runtime/CLI entry point. |
| `apps/cyberware-example-server/src/registered_modules.rs` | Link-time module/plugin registration. |
| `libs/modkit/src/lib.rs` | Core ModKit public API surface. |
| `docs/modkit_unified_system/README.md` | ModKit routing and invariants. |

## Module Boundaries
### Follow DDD-light module layout
Place module code in `module.rs`, `config.rs`, `api/rest`, `domain`, and `infra`, with optional sibling `*-sdk` crates for public traits/models.
Evidence: `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md:3` - canonical module layout and naming.

### Register provider clients during module init
Provider modules that expose typed clients must register them through `ClientHub` during `Module::init`.
Evidence: `docs/modkit_unified_system/03_clienthub_and_plugins.md:28` - provider registration flow.

### Keep plugin implementations behind main module public APIs
Regular modules must consume the main module API, not plugin modules directly; plugin clients are scoped by instance.
Evidence: `docs/MODKIT_PLUGINS.md:10` - public-vs-plugin trait separation.

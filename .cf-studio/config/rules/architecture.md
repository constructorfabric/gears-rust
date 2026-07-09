---
cf: true
type: project-rule
topic: architecture
generated-by: auto-config
version: 1.0
---
# Architecture

<!-- toc -->

- [Critical Files](#critical-files)
- [Runtime Boundaries](#runtime-boundaries)

<!-- /toc -->

Use this when modifying architecture, gear boundaries, or runtime behavior.

## Critical Files
| File | Why critical |
|---|---|
| `Cargo.toml` | Workspace membership, resolver, lint policy |
| `apps/cf-gears-example-server/src/main.rs` | Process entry and CLI bootstrap |
| `apps/cf-gears-example-server/src/registered_gears.rs` | Link-time gear/plugin registration |
| `libs/toolkit/src/contracts.rs` | Gear capability contracts |
| `libs/toolkit/src/runtime/host_runtime.rs` | Runtime phase ordering |
| `.cf-studio/config/artifacts.toml` | Studio system/artifact autodetect registry |

## Runtime Boundaries
- Keep applications thin; delegate runtime work to ToolKit bootstrap helpers. Evidence: `apps/cf-gears-example-server/src/main.rs:65-115`
- Preserve inventory/link-time gear registration through explicit imports. Evidence: `apps/cf-gears-example-server/src/registered_gears.rs:1-70`
- Model gear lifecycle with `Gear`, `DatabaseCapability`, `RestApiCapability`, `RunnableCapability`, and gRPC capability traits. Evidence: `libs/toolkit/src/contracts.rs:35-194`
- Keep DB privilege runtime-owned; gears expose migrations and receive scoped access. Evidence: `libs/toolkit/src/contracts.rs:41-49`, `libs/toolkit/src/context.rs:133-215`

---
cf: true
type: project-rule
topic: conventions
generated-by: auto-config
version: 1.0
---
# Conventions

<!-- toc -->

- [Rust Workspace](#rust-workspace)
  - [Use workspace-managed dependencies](#use-workspace-managed-dependencies)
  - [Keep module names kebab-case](#keep-module-names-kebab-case)
  - [Put REST-only wire shapes in `api/rest/dto.rs`](#put-rest-only-wire-shapes-in-apirestdtors)

<!-- /toc -->

## Rust Workspace
### Use workspace-managed dependencies
Prefer root workspace dependency declarations and path deps instead of local duplicate versions.
Evidence: `Cargo.toml:225` - workspace dependency section.

### Keep module names kebab-case
Use kebab-case for module directories and ModKit module names.
Evidence: `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md:3` - module naming guidance.

### Put REST-only wire shapes in `api/rest/dto.rs`
Keep SDK models transport-agnostic and convert REST DTOs at the boundary.
Evidence: `modules/mini-chat/mini-chat/src/api/rest/dto.rs:1` - DTO file contract.

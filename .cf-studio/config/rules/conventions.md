---
cf: true
type: project-rule
topic: conventions
generated-by: auto-config
version: 1.0
---
# Conventions

<!-- toc -->

- [Rust Workspace Style](#rust-workspace-style)
- [Code Organization](#code-organization)

<!-- /toc -->

Use this when writing or reviewing Rust code.

## Rust Workspace Style
- Keep workspace code on Rust 2024 and the pinned toolchain. Evidence: `Cargo.toml:2-8`, `rust-toolchain.toml:1-3`
- Treat workspace lint settings as design constraints, not suggestions. Evidence: `Cargo.toml:101-210`
- Keep gear directory names kebab-case. Evidence: `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:3-18`
- Preserve LF/final-newline formatting and Markdown trailing whitespace behavior. Evidence: `.editorconfig:1-28`

## Code Organization
- Put public SDK contracts in SDK crates and implementation internals in gear crates. Evidence: `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:20-52`
- Keep REST DTOs under `src/api/rest/`; keep transport-agnostic models in SDK/domain layers. Evidence: `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:61-80`

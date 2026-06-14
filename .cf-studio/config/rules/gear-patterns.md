---
cf: true
type: project-rule
topic: gear-patterns
generated-by: auto-config
version: 1.0
---
# Gear Patterns

<!-- toc -->

- [Layout](#layout)
- [Plugins](#plugins)

<!-- /toc -->

Use this when creating or changing gears, SDK crates, plugins, or registration.

## Layout
- Place gear docs under `gears/<name>/docs/`. Evidence: `gears/file-storage/README.md:40-45`
- Use implementation crates with `src/gear.rs` as the capability/dependency entry point. Evidence: `gears/file-parser/src/gear.rs:14-21`
- Use optional `<gear>-sdk` crates for ClientHub-facing traits, models, and errors. Evidence: `gears/simple-user-settings/simple-user-settings-sdk/src/lib.rs:1-26`
- Split implementation code into `api`, `domain`, and `infra` where behavior warrants it. Evidence: `gears/file-parser/src/lib.rs:1-17`

## Plugins
- Prefer sibling plugin crates under `plugins/` for replaceable backend behavior. Evidence: `gears/system/tenant-resolver/README.md:170-209`
- Co-locate an internal plugin only when it depends on owning-gear invariants. Evidence: `gears/system/account-management/account-management/src/tr_plugin/mod.rs:1-17`

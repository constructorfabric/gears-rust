---
cf: true
type: project-rule
topic: docs-navigation
generated-by: auto-config
version: 1.0
---
# Documentation Navigation

<!-- toc -->

- [Activity Routing](#activity-routing)
  - [Start with project guidelines](#start-with-project-guidelines)
  - [Use ModKit routing for module work](#use-modkit-routing-for-module-work)
  - [Use module docs for module behavior](#use-module-docs-for-module-behavior)

<!-- /toc -->

## Activity Routing
### Start with project guidelines
Open `guidelines/README.md` before project work, then follow its Rust, REST, ModKit, security, or dependency routing.
Evidence: `guidelines/README.md:1` - guideline index.

### Use ModKit routing for module work
Use `docs/modkit_unified_system/README.md` to select the precise ModKit guide for architecture, REST, errors, DB, lifecycle, or tests.
Evidence: `docs/modkit_unified_system/README.md:12` - task-to-document routing.

### Use module docs for module behavior
When work is module-specific, inspect that module's `docs/PRD.md`, `docs/DESIGN.md`, ADRs, decomposition, and feature files.
Evidence: `modules/chat-engine/docs/DECOMPOSITION.md` - module-local SDLC artifact pattern.

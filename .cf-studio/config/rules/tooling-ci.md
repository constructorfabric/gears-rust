---
cf: true
type: project-rule
topic: tooling-ci
generated-by: auto-config
version: 1.0
---
# Tooling And CI

<!-- toc -->

- [Quality Gates](#quality-gates)
  - [Prefer Make targets for routine checks](#prefer-make-targets-for-routine-checks)
  - [Keep CI aligned with local gates](#keep-ci-aligned-with-local-gates)
  - [Preserve pinned Rust toolchain assumptions](#preserve-pinned-rust-toolchain-assumptions)

<!-- /toc -->

## Quality Gates
### Prefer Make targets for routine checks
Use the project Makefile targets for fmt, clippy, tests, security, E2E, coverage, and CI parity.
Evidence: `Makefile:227` - quality and test targets.

### Keep CI aligned with local gates
Changes to lint/test/security behavior should update GitHub Actions and corresponding Make targets together.
Evidence: `.github/workflows/ci.yml:28` - multi-OS lint and test workflow.

### Preserve pinned Rust toolchain assumptions
Do not silently change the Rust channel/components without updating local and CI expectations.
Evidence: `rust-toolchain.toml:1` - Rust toolchain pin.

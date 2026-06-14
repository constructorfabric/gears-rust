---
cf: true
type: project-rule
topic: infrastructure
generated-by: auto-config
version: 1.0
---
# Infrastructure

<!-- toc -->

- [Tooling](#tooling)
- [CI and Releases](#ci-and-releases)

<!-- /toc -->

Use this when changing build tooling, CI, linting, releases, or dependency policy.

## Tooling
- Use `Makefile` targets as the main local automation surface. Evidence: `Makefile:120-173`, `Makefile:218-320`
- Preserve fast PR Clippy and deeper validation split. Evidence: `Makefile:224-246`
- Keep custom Dylint rules aligned with architecture categories. Evidence: `tools/dylint_lints/README.md:16-70`

## CI and Releases
- Preserve cross-OS test matrix and DB integration jobs. Evidence: `.github/workflows/ci.yml:85-220`
- Keep E2E workflows capable of nightly failure issue creation. Evidence: `.github/workflows/e2e.yml:80-161`
- Use release-plz flow for release PRs and publishing gates. Evidence: `.github/workflows/release-plz.yml:1-207`

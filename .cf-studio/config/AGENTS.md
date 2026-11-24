# Constructor Studio Adapter: CF/Gears

**Version**: 1.0
**Last Updated**: 2026-02-05

---

## Variables

**While Constructor Studio is enabled**, remember these variables:

| Variable | Value | Description |
|----------|-------|-------------|
| `{cf-studio-path}/config` | Directory containing this AGENTS.md | Root path for Constructor Studio Adapter navigation |

Use `{cf-studio-path}/config` as the base path for all relative Constructor Studio Adapter file references.

---

## Project Overview

This repository is a **modular monolith** built on top of **CF/Gears**.

- **CF/Gears base**: core apps/libraries live under `apps/`, `libs/`, etc.
- **Subsystems / modules**: each subsystem is a module under `gears/<gear_name>/`.
- **Constructor Studio registry convention**: subsystems are registered as `children[]` of the root `cf-gears` system in `{cf-studio-path}/config/artifacts.toml`.
- **Docs convention**: each module keeps its artifacts under `gears/<gear_name>/docs/`.
- **Repository Playbook**: `docs/REPO_PLAYBOOK.md` — comprehensive map of all repository artifacts, standards, tooling, and planned gaps (with per-item status, phase, and ID).

---

## Navigation Rules

ALWAYS sign commits with DCO: use `git commit -s` for all commits

ALWAYS open and follow `{cf-studio-path}/requirements/artifacts-registry.md` WHEN working with artifacts.toml

ALWAYS open and follow `artifacts.toml` WHEN registering Constructor Studio artifacts, updating codebase paths, changing traceability settings, or running Constructor Studio validation

ALWAYS open and follow `CONTRIBUTING.md` WHEN setting up development environment, creating feature branches, running quality checks (make all, cargo clippy, cargo fmt), signing commits with DCO, writing commit messages, creating pull requests, or understanding the review process

ALWAYS open `docs/REPO_PLAYBOOK.md` WHEN looking for a map of repository artifacts, understanding what standards/tooling exist, identifying coverage gaps, or onboarding to the project structure

## Project Documentation (auto-configured)
<!-- auto-config:docs:start -->
ALWAYS open and follow `guidelines/README.md` WHEN starting project work or deciding which project standards apply
ALWAYS open and follow `README.md#quick-start` WHEN onboarding, running the server, or using local example commands
ALWAYS open and follow `CONTRIBUTING.md#2-development-workflow` WHEN contributing code, creating branches, running quality checks, signing commits, or opening PRs
ALWAYS open and follow `docs/REPO_PLAYBOOK.md` WHEN locating repository standards, tooling, CI, testing, security, or documentation ownership
ALWAYS open and follow `docs/ARCHITECTURE_MANIFEST.md#3-architectural-principles` WHEN changing architecture, module boundaries, or cross-cutting platform behavior
ALWAYS open and follow `docs/toolkit_unified_system/README.md#task--document-routing` WHEN working with ToolKit or gears
ALWAYS open and follow `docs/security/SECURITY.md` WHEN changing security controls, dependency security, FIPS behavior, credentials, or scanner expectations
<!-- auto-config:docs:end -->

## Project Rules (auto-configured)
<!-- auto-config:rules:start -->
ALWAYS open and follow `{cf-studio-path}/config/rules/conventions.md` WHEN writing or reviewing Rust code
ALWAYS open and follow `{cf-studio-path}/config/rules/architecture.md` WHEN modifying architecture, gear boundaries, or runtime behavior
ALWAYS open and follow `{cf-studio-path}/config/rules/gear-patterns.md` WHEN creating or changing gears, SDK crates, plugins, or registration
ALWAYS open and follow `{cf-studio-path}/config/rules/api-contracts.md` WHEN adding REST endpoints, OpenAPI routes, DTOs, SDK errors, or Problem mappings
ALWAYS open and follow `{cf-studio-path}/config/rules/testing.md` WHEN writing or running unit, integration, compile-fail, end-to-end, or fuzz tests
ALWAYS open and follow `{cf-studio-path}/config/rules/infrastructure.md` WHEN changing build tooling, CI, linting, releases, or dependency policy
ALWAYS open and follow `{cf-studio-path}/config/rules/security.md` WHEN writing security-sensitive code, tenant-scoped data access, credentials, or FIPS behavior
ALWAYS open and follow `{cf-studio-path}/config/rules/anti-patterns.md` WHEN reviewing code or refactoring risky patterns
<!-- auto-config:rules:end -->

---

## Gear Rules

ALWAYS register new gears under `gears/<gear_name>/` as a `children[]` entry of the root `cf-gears` system in `artifacts.toml` WHEN adding a new gear / subsystem

ALWAYS open `docs/toolkit_unified_system/README.md` and follow its Task → Document routing table WHEN onboarding to ToolKit, or creating or changing a gear, SDK crate, ClientHub client, plugin, REST endpoint, OData query, database access, error handling, lifecycle or background task, out-of-process gear, or tests; load only the documents the table names for the task

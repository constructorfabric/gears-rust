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

ALWAYS open `docs/toolkit_unified_system/01_overview.md` WHEN onboarding to ToolKit, understanding core concepts, or reviewing the golden path for module development

ALWAYS open `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md` WHEN starting to define requirements, architecture design, or implement any module; creating new gear directory structure; deciding where to place files; understanding SDK pattern; creating Cargo.toml; naming data types; implementing local client; registering module in cf-gears-example-server; or creating QUICKSTART.md

ALWAYS open `docs/toolkit_unified_system/03_clienthub_and_plugins.md` WHEN implementing inter-module communication via ClientHub, registering or resolving typed clients, implementing plugin architecture, creating main module with plugins, or registering scoped clients via GTS

ALWAYS open `docs/toolkit_unified_system/03_clienthub_and_plugins.md` AND `docs/TOOLKIT_PLUGINS.md` WHEN implementing full plugin architecture with GTS schema/instance registration, plugin selection, or studying the tenant-resolver reference implementation

ALWAYS open `docs/toolkit_unified_system/04_rest_operation_builder.md` WHEN adding REST endpoints, creating DTOs, implementing handlers, using OperationBuilder, adding SSE events, or configuring endpoint authentication

ALWAYS open `docs/toolkit_unified_system/05_errors_rfc9457.md` WHEN implementing error handling, creating DomainError, mapping errors to Problem (RFC-9457), defining SDK errors, or adding From impls for error conversion

ALWAYS open `docs/toolkit_unified_system/06_authn_authz_secure_orm.md` WHEN adding SeaORM entities, using SecureConn, implementing AuthN/AuthZ, using PolicyEnforcer PEP pattern, or working with AccessScope from PDP constraints

ALWAYS open `docs/toolkit_unified_system/11_database_patterns.md` WHEN implementing repositories, creating database migrations, using DBRunner/SecureTx, or implementing transaction patterns

ALWAYS open `docs/toolkit_unified_system/07_odata_pagination_select_filter.md` WHEN adding OData filtering, pagination, $select, $orderby, implementing ODataFilterable derive, creating FieldToColumn/ODataFieldMapping, or using cursor-based pagination

ALWAYS open `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` WHEN using #[toolkit::gear] macro, implementing Gear trait, registering clients in ClientHub, configuring gear lifecycle, or using WithLifecycle/CancellationToken for background tasks

ALWAYS open `docs/toolkit_unified_system/09_oop_grpc_sdk_pattern.md` WHEN creating out-of-process gear, implementing gRPC service, setting up OoP binary, or wiring gRPC clients via DirectoryApi

ALWAYS open `docs/toolkit_unified_system/10_checklists_and_templates.md` WHEN writing module tests, creating SecurityContext for tests, implementing integration tests, or looking for quick checklists and code templates

ALWAYS open `docs/toolkit_unified_system/12_unit_testing.md` WHEN writing unit tests, setting up test infrastructure, creating test fixtures, implementing mock-based tests, or defining test file organization (`*_tests.rs` pattern)

ALWAYS open `docs/toolkit_unified_system/13_e2e_testing.md` WHEN writing end-to-end tests, setting up E2E test infrastructure, implementing cross-module integration tests, or working with the `testing/e2e/` directory

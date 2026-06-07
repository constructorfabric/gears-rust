# Constructor Studio Adapter: Gears Rust

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

This repository is a **modular monolith** built on top of **Cyber Ware**.

- **Cyber Ware base**: core apps/libraries live under `apps/`, `libs/`, etc.
- **Subsystems / modules**: each subsystem is a module under `modules/<module_name>/`.
- **Constructor Studio registry convention**: subsystems are registered as `children[]` of the root `cyberware` system in `{cf-studio-path}/config/artifacts.toml`.
- **Docs convention**: each module keeps its artifacts under `modules/<module_name>/docs/`.
- **Repository Playbook**: `docs/REPO_PLAYBOOK.md` — comprehensive map of all repository artifacts, standards, tooling, and planned gaps (with per-item status, phase, and ID).

---

## Navigation Rules

ALWAYS sign commits with DCO: use `git commit -s` for all commits

ALWAYS open and follow `{cf-studio-path}/requirements/artifacts-registry.md` WHEN working with artifacts.toml

ALWAYS open and follow `artifacts.toml` WHEN registering Constructor Studio artifacts, updating codebase paths, changing traceability settings, or running Constructor Studio validation

ALWAYS open and follow `CONTRIBUTING.md` WHEN setting up development environment, creating feature branches, running quality checks (make all, cargo clippy, cargo fmt), signing commits with DCO, writing commit messages, creating pull requests, or understanding the review process

ALWAYS open `docs/REPO_PLAYBOOK.md` WHEN looking for a map of repository artifacts, understanding what standards/tooling exist, identifying coverage gaps, or onboarding to the project structure

---

## Module Rules

ALWAYS register new modules under `modules/<module_name>/` as a `children[]` entry of the root `cyberware` system in `artifacts.toml` WHEN adding a new module / subsystem

ALWAYS open `docs/modkit_unified_system/01_overview.md` WHEN onboarding to ModKit, understanding core concepts, or reviewing the golden path for module development

ALWAYS open `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md` WHEN starting to define requirements, architecture design, or implement any module; creating new module directory structure; deciding where to place files; understanding SDK pattern; creating Cargo.toml; naming data types; implementing local client; registering module in cyberware-example-server; or creating QUICKSTART.md

ALWAYS open `docs/modkit_unified_system/03_clienthub_and_plugins.md` WHEN implementing inter-module communication via ClientHub, registering or resolving typed clients, implementing plugin architecture, creating main module with plugins, or registering scoped clients via GTS

ALWAYS open `docs/modkit_unified_system/03_clienthub_and_plugins.md` AND `docs/MODKIT_PLUGINS.md` WHEN implementing full plugin architecture with GTS schema/instance registration, plugin selection, or studying the tenant-resolver reference implementation

ALWAYS open `docs/modkit_unified_system/04_rest_operation_builder.md` WHEN adding REST endpoints, creating DTOs, implementing handlers, using OperationBuilder, adding SSE events, or configuring endpoint authentication

ALWAYS open `docs/modkit_unified_system/05_errors_rfc9457.md` WHEN implementing error handling, creating DomainError, mapping errors to Problem (RFC-9457), defining SDK errors, or adding From impls for error conversion

ALWAYS open `docs/modkit_unified_system/06_authn_authz_secure_orm.md` WHEN adding SeaORM entities, using SecureConn, implementing AuthN/AuthZ, using PolicyEnforcer PEP pattern, or working with AccessScope from PDP constraints

ALWAYS open `docs/modkit_unified_system/11_database_patterns.md` WHEN implementing repositories, creating database migrations, using DBRunner/SecureTx, or implementing transaction patterns

ALWAYS open `docs/modkit_unified_system/07_odata_pagination_select_filter.md` WHEN adding OData filtering, pagination, $select, $orderby, implementing ODataFilterable derive, creating FieldToColumn/ODataFieldMapping, or using cursor-based pagination

ALWAYS open `docs/modkit_unified_system/08_lifecycle_stateful_tasks.md` WHEN using #[modkit::module] macro, implementing Module trait, registering clients in ClientHub, configuring module lifecycle, or using WithLifecycle/CancellationToken for background tasks

ALWAYS open `docs/modkit_unified_system/09_oop_grpc_sdk_pattern.md` WHEN creating out-of-process module, implementing gRPC service, setting up OoP binary, or wiring gRPC clients via DirectoryApi

ALWAYS open `docs/modkit_unified_system/10_checklists_and_templates.md` WHEN writing module tests, creating SecurityContext for tests, implementing integration tests, or looking for quick checklists and code templates

ALWAYS open `docs/modkit_unified_system/12_unit_testing.md` WHEN writing unit tests, setting up test infrastructure, creating test fixtures, implementing mock-based tests, or defining test file organization (`*_tests.rs` pattern)

ALWAYS open `docs/modkit_unified_system/13_e2e_testing.md` WHEN writing end-to-end tests, setting up E2E test infrastructure, implementing cross-module integration tests, or working with the `testing/e2e/` directory

---

## Project Documentation (auto-configured)
<!-- auto-config:docs:start -->
ALWAYS open and follow `README.md#quick-start` WHEN onboarding, running the example server, or locating local development commands

ALWAYS open and follow `CONTRIBUTING.md#development-workflow` WHEN contributing code, preparing branches, running quality checks, or preparing PRs

ALWAYS open and follow `guidelines/DEPENDENCIES.md` WHEN changing Cargo.toml, adding dependencies, or choosing third-party crates

ALWAYS open and follow `guidelines/SECURITY.md` WHEN handling user input, secrets, AuthN/AuthZ, tenant isolation, or secure persistence

ALWAYS open and follow `guidelines/GTS.md` WHEN adding GTS schemas, GTS IDs, permissions, plugin discovery IDs, or typed extensibility

ALWAYS open and follow `docs/REPO_PLAYBOOK.md` WHEN locating repository standards, tooling, CI, testing strategy, or coverage gaps

ALWAYS open and follow `docs/ARCHITECTURE_MANIFEST.md` WHEN changing high-level architecture, module hierarchy, security architecture, or API/error contracts

ALWAYS open and follow `docs/modkit_unified_system/README.md` WHEN touching ModKit/module architecture, REST wiring, ClientHub, OpenAPI, lifecycle, SSE, standardized errors, DB, or tests

ALWAYS open and follow `docs/MODKIT_PLUGINS.md` WHEN implementing full plugin architecture, GTS plugin registration, plugin selection, or scoped ClientHub clients

ALWAYS open and follow `docs/pr-review/README.md` WHEN reviewing PRs or producing PR status reports

ALWAYS open and follow `docs/spec-templates/README.md` WHEN authoring or revising SDLC artifacts

ALWAYS open and follow `modules/<module>/docs/` WHEN implementing or reviewing behavior for a specific module
<!-- auto-config:docs:end -->

## Project Rules (auto-configured)
<!-- auto-config:rules:start -->
ALWAYS open and follow `{cf-studio-path}/config/rules/architecture.md` WHEN modifying architecture, adding modules, changing module boundaries, or touching ModKit lifecycle/client wiring

ALWAYS open and follow `{cf-studio-path}/config/rules/conventions.md` WHEN writing or reviewing Rust code

ALWAYS open and follow `{cf-studio-path}/config/rules/api-contracts.md` WHEN writing REST endpoints, DTOs, handlers, OpenAPI registration, SSE, or OData surfaces

ALWAYS open and follow `{cf-studio-path}/config/rules/security-data-access.md` WHEN handling AuthN/AuthZ, SecurityContext, tenant scope, secure ORM, repositories, or sensitive input

ALWAYS open and follow `{cf-studio-path}/config/rules/error-handling.md` WHEN adding domain errors, SDK errors, canonical errors, or Problem mappings

ALWAYS open and follow `{cf-studio-path}/config/rules/testing.md` WHEN writing unit, integration, or E2E tests

ALWAYS open and follow `{cf-studio-path}/config/rules/tooling-ci.md` WHEN running quality gates, changing CI, Make targets, coverage, FIPS, or release tooling

ALWAYS open and follow `{cf-studio-path}/config/rules/docs-navigation.md` WHEN authoring docs, locating module SDLC artifacts, or updating documentation navigation
<!-- auto-config:rules:end -->

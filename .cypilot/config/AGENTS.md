# Cypilot Adapter: CyberFabric

**Version**: 1.0
**Last Updated**: 2026-02-05

---

## Variables

**While Cypilot is enabled**, remember these variables:

| Variable | Value | Description |
|----------|-------|-------------|
| `{cypilot_path}/config` | Directory containing this AGENTS.md | Root path for Cypilot Adapter navigation |

Use `{cypilot_path}/config` as the base path for all relative Cypilot Adapter file references.

---

## Project Overview

This repository is a **modular monolith** built on top of **CyberFabric**.

- **CyberFabric base**: core apps/libraries live under `apps/`, `libs/`, etc.
- **Subsystems / modules**: each subsystem is a module under `modules/<module_name>/`.
- **Cypilot registry convention**: subsystems are registered as `children[]` of the root `cyberfabric` system in `{cypilot_path}/config/artifacts.toml`.
- **Docs convention**: each module keeps its artifacts under `modules/<module_name>/docs/`.
- **Repository Playbook**: `docs/REPO_PLAYBOOK.md` — comprehensive map of all repository artifacts, standards, tooling, and planned gaps (with per-item status, phase, and ID).

---

## Navigation Rules

ALWAYS sign commits with DCO: use `git commit -s` for all commits

ALWAYS open and follow `{cypilot_path}/requirements/artifacts-registry.md` WHEN working with artifacts.toml

ALWAYS open and follow `artifacts.toml` WHEN registering Cypilot artifacts, updating codebase paths, changing traceability settings, or running Cypilot validation

ALWAYS open and follow `CONTRIBUTING.md` WHEN setting up development environment, creating feature branches, running quality checks (make all, cargo clippy, cargo fmt), signing commits with DCO, writing commit messages, creating pull requests, or understanding the review process

ALWAYS open `docs/REPO_PLAYBOOK.md` WHEN looking for a map of repository artifacts, understanding what standards/tooling exist, identifying coverage gaps, or onboarding to the project structure

---

## Module Rules

ALWAYS register new modules under `modules/<module_name>/` as a `children[]` entry of the root `cyberfabric` system in `artifacts.toml` WHEN adding a new module / subsystem

ALWAYS open `docs/modkit_unified_system/01_overview.md` WHEN onboarding to ModKit, understanding core concepts, or reviewing the golden path for module development

ALWAYS open `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md` WHEN starting to define requirements, architecture design, or implement any module; creating new module directory structure; deciding where to place files; understanding SDK pattern; creating Cargo.toml; naming data types; implementing local client; registering module in hyperspot-server; or creating QUICKSTART.md

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

---
title: Spec-driven workflow (SDD + Studio)
description: The specification templates that guide larger features and how Constructor Studio drives the flow from requirements to implementation and tests.
sidebar:
  label: Spec-driven workflow
  order: 4
---

Gears follows a **spec-driven development (SDD)** approach for large features. Development starts with specifications that live alongside the code and stay aligned with the implementation. For the conceptual rationale, see [Spec-driven, AI-native development](../../introduction/spec-driven-ai-native/).

## The specification templates

Large features start from specifications that live alongside the code in the repository, not in a separate wiki that drifts.

Every Gear must always have **PRD** and **DESIGN** documents.

**ADR**, **DECOMPOSITION**, **FEATURE**, and **UPSTREAM_REQS** are optional documents that you add when they are needed for decisions, planning, feature-level definition, or cross-Gear requirements.

Use these templates and keep them aligned with the code:

- **[Overview & guide](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts)** — template set and placement under the Gear SDLC spec structure.
- **[PRD](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/PRD)** — product requirements: vision, actors, capabilities, use cases, FR/NFR.
- **[DESIGN](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/DESIGN)** — technical design: architecture, principles, constraints, domain model, API contracts.
- **[ADR](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/ADR)** — architecture decision records: decisions, options, trade-offs, consequences.
- **[DECOMPOSITION](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/DECOMPOSITION)** — decomposition of work into features, sequencing, and dependency structure.
- **[FEATURE](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/FEATURE)** — feature specs: flows, algorithms, states, definition of done.
- **[UPSTREAM_REQS](https://github.com/constructorfabric/gears-rust/tree/main/studio-kit-gears/artifacts/UPSTREAM_REQS)** — technical requirements flowing from other gears into this one.

## Constructor Studio

Constructor Studio helps drive the flow from requirements to implementation and tests, keeping documentation and code consistent with traceability across artifacts and code changes. It supports AI-assisted development **without making architecture and quality rules optional** — the same [quality gates](../architecture-and-quality-gates/) still apply.

Studio is included as a submodule when you clone with `--recurse-submodules` (see [Development setup](../development-setup/)), and its PR-review workflows are invoked with the `cf-gears-pr-review` / `cf-gears-pr-status` commands described in the [code contribution guide](../code-contribution-guide/).

## See also

- [Add or change a gear](../add-or-change-a-gear/) — where SDD applies most directly.
- [Testing model](../../concepts/testing-model/) — the tests these specs feed into.

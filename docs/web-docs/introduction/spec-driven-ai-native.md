---
title: Spec-driven, AI-native development
description: How Gears keeps requirements, design, code, and tests traceable — and why that makes it a strong base for AI-assisted development.
sidebar:
  label: Spec-driven & AI-native
  order: 4
---

Gears is designed for an **AI-native, local-first developer workflow** where specifications, code, tests, and architecture feedback stay connected as a product evolves. This is a deliberate value proposition, not a side effect.

## Spec-driven development (SDD)

Large features start from specifications that live **alongside the code** in the repository, not in a separate wiki that drifts. The template set is:

- **PRD** — product requirements: vision, actors, capabilities, use cases, FR/NFR.
- **DESIGN** — technical design: architecture, principles, constraints, domain model, API
  contracts.
- **ADR** — architecture decision records: decisions, options, trade-offs, consequences.
- **FEATURE** — feature specs: flows, algorithms, states, definition of done.
- **UPSTREAM_REQS** — technical requirements flowing from other gears into this one.

The specs are not ceremony; they guide implementation, review, and long-term maintenance, and they keep documentation, code, and intent aligned. See the [spec templates](https://github.com/constructorfabric/gears-rust/blob/main/studio-kit-gears/artifacts/README.md) in the framework repository.

## Why this is AI-native

AI accelerates the volume of change. That makes two things more valuable, not less:

- **Traceability** — a clear line from requirement to design to code to test lets both humans and AI agents keep intent and implementation consistent.
- **Fast, local quality gates** — build, lint, unit, integration, and E2E feedback run before CI, so mistakes (whoever or whatever produced them) are caught early. Custom architectural lints enforce layer boundaries and security invariants at compile time, so AI-assisted development does not make the rules optional.

## The local-first quality loop

The same logical building blocks compose locally first, then carry into cloud, hybrid, edge, or on-prem deployments. You can run multiple gears together in one process, exercise cross-gear behavior, and inspect generated API docs, config, logs, and framework feedback — all before a distributed deployment.

## Where this shows up in practice

- Contributors follow the SDD flow — see the [spec-driven workflow](../../contribute/spec-driven-workflow/) in the contribution guide.
- The quality gates that back it are documented in  [Architecture and quality gates](../../contribute/architecture-and-quality-gates/).
- The extension model (GTS + plugins) lets product teams add custom types and logic without forking — see [Plugins and extension points](../../concepts/plugins-and-extension-points/).

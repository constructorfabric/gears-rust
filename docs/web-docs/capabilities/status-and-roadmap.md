---
title: Status and roadmap
description: What is implemented today versus what is designed and documented but not yet in the source tree.
sidebar:
  label: Status and roadmap
  order: 6
---

Gears documents its **target architecture** alongside its **current state**, so you can plan
honestly. Use this page to read the rest of the docs correctly.

:::note[How to read status]
`✓` means implemented in the framework source repository. _planned_ means designed and
documented but not yet in the source tree. When you see a planned component referenced
elsewhere in the docs, treat it as roadmap unless this page or the relevant capability page
marks it `✓`.
:::

## Implemented today

- **Toolkit substrate** — runtime, secure ORM, canonical errors, REST/OpenAPI, OData,
  observability, gRPC transport, GTS. See [Toolkit](../toolkit/).
- **System gears** — API Gateway, Gear Orchestrator, AuthN/AuthZ resolvers, Tenant Resolver,
  Outbound API Gateway, Types Registry, Nodes Registry, Resource Group, gRPC Hub. See
  [System gears](../system-gears/).
- **Service gears (examples)** — File Parser, Credentials Store, Mini Chat, Simple User
  Settings. See [Reusable service gears](../service-gears/).

## Designed but not yet implemented

These are designed and documented in the framework but **not yet implemented**:

- **Cluster plane** — leader election, distributed locks, distributed cache.
- **GenAI gears** — Chat Engine, LLM Gateway, Models / Prompts / AI Agents registries, MCP
  Registry, Agent Memory, Web Search Gateway, URL Crawler, Model Scheduler, Local Search Index.
- **Serverless gears** — Serverless Gateway & Runtimes, Durable Objects.
- **Core functionality gears** — Events Broker, File Storage, Jobs Manager, Notifications,
  Approvals, Analytics, Quota Enforcer, Audit, Usage Collector (impl).

## Scenario architecture (design-level)

The roadmap includes scenario-level architecture for incoming API-call processing, chat
hooks, synchronous and asynchronous file-attachment processing, and SSE streaming with
throttling. These are described in the framework
[GEARS.md](https://github.com/constructorfabric/gears-rust/blob/main/docs/GEARS.md) reference.

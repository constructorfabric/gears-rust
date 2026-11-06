---
title: Where Gears fits
description: Adoption scenarios for Gears — greenfield platforms, existing-platform integration, GenAI backends, and edge/on-prem/regulated deployments.
sidebar:
  label: Where Gears fits
  order: 3
---

Gears is a framework and middleware layer for building **XaaS/SaaS platforms**, not a
ready-made product. This page helps you decide whether it fits your situation and which
adoption path to take.

## Who it is for

- **XaaS / SaaS vendors** building a multi-tenant product backend who want tenancy, authorization, licensing, usage, and consistent APIs as framework guarantees.
- **Platform teams** who need long-term structure, explicit contracts, and compile-time guardrails across many services.
- **GenAI builders** who need a secure, multi-tenant base for chat, retrieval, model access, agents, and tools.
- **Edge / on-prem / regulated vendors** who care about small footprint, local execution, and supply-chain controls.
- **Enterprise integration teams** embedding capabilities into an existing platform whose identity, policy, licensing, or credentials are already provided by other systems.

## Two adoption paths

Gears supports two distinct entry points. Most teams start with one and grow into the other.

### Path A — Greenfield

Start a new platform or product on Gears. You build your first gear, compose it with the core gears (gateway, authn/authz, tenancy), and grow the platform capability by capability. Follow [Build your first gear](../../build-with-gears/your-first-gear/).

Choose greenfield when you are starting a new XaaS product, an on-prem or edge appliance, or a GenAI platform backbone.

### Path B — Integrate into an existing platform

Add Gears capabilities into an existing SaaS or platform without forking your core. You reuse existing IdP, license engine, tenant directory, secret vault, or native UI by wiring adapters and plugins, extending the domain model through the [Global Type System (GTS)](https://github.com/GlobalTypeSystem/gts-rust), and adding custom logic through hooks. See [Integrate into an existing platform](../../build-with-gears/integrate-existing-platform/).

Choose integration when you already have platform services and want Gears capabilities without replacing them.

## Deployment shapes it targets

The same gear code runs as a single node (edge/on-prem/dev), across multiple processes over gRPC, or as containers in Kubernetes — selected by configuration. See [Deployment shapes](../../concepts/deployment-shapes/).

## When Gears is not the right choice

- A tiny standalone service that does not need a platform, tenancy, or security backbone.
- Teams that want minimalism and the lowest possible learning curve first.
- A drop-in PaaS or a comprehensive catalog of ready-made end-user SaaS services.

See [Why Gears](../why-gears/) for the full rationale and [What is Gears?](../) for the framework's own non-goals.

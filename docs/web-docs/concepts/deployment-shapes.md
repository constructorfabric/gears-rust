---
title: Deployment shapes
description: One codebase, three deployment shapes — single-node, multi-node over gRPC, and Kubernetes — chosen by configuration, not code changes.
sidebar:
  label: Deployment shapes
  order: 14
---

One codebase compiles into three deployment shapes. What changes between them is configuration (`runtime.type`, backend
selection, bootstrap entry point) — the gear code is identical.

| Shape           | Where gears run                            | How they talk                            |
|-----------------|--------------------------------------------|------------------------------------------|
| **Single-node** | one process (edge, on-prem, dev)           | in-process via `ClientHub`               |
| **Multi-node**  | across processes/machines, no orchestrator | gRPC, `SecurityContext` over headers     |
| **Kubernetes**  | containerized services                     | cluster DNS discovery, external gateways |

## Why one codebase can do this

The [SDK facade + backend pattern](../sdk-and-clienthub/) means a consumer resolves a trait and calls it without knowing
whether the implementation is an in-process adapter or a gRPC client. Because that choice is configuration, the same
logical composition of gears carries from a laptop to a cluster. This underpins the local-first workflow: compose and
test gears together in one process, then deploy the same building blocks distributed.

## Status

Single-node and multi-node (gRPC out-of-process) are implemented. The cluster-plane coordination primitives that a large
Kubernetes deployment relies on — distributed cache, leader election, distributed locks — are *
*designed but not yet implemented**. See [Status and roadmap](../../capabilities/status-and-roadmap/) and
the [cluster plane note](../runtime-and-lifecycle/).

## See also

- [Deploy Gears](../../build-with-gears/deploy/) — the how-to for each shape.
- [Run a gear out-of-process](../../build-with-gears/out-of-process/) — the gRPC path.
- [Configure a Gears application](../../build-with-gears/configure/) — the config knobs.

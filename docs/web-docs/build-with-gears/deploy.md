---
title: Deploy Gears
description: Run the same gear code as a single node, across processes over gRPC, or as containers on Kubernetes — selected by configuration.
sidebar:
  label: Deploy Gears
  order: 14
---

The same gear code compiles into three deployment shapes. You choose with configuration, not by rewriting code. For the conceptual model, see [Deployment shapes](../../concepts/deployment-shapes/).

## Single-node

Every gear runs in one process (edge, on-prem, development). Gears talk in-process through `ClientHub`. This is what the quickstart runs — see [Install and run](../).

```yaml
gears:
  my-gear:
    runtime:
      type: local
```

## Multi-node (gRPC)

Gears split across processes or machines over gRPC, without container orchestration. Out-of-process gears self-register with a directory (the gRPC hub) and consumers get a gRPC client behind the same SDK trait.

```yaml
gears:
  my-gear:
    runtime:
      type: oop
```

The example ships a master config wiring the gateway, gRPC hub, and orchestrator. See [Run a gear out-of-process](../out-of-process/).

## Kubernetes

Gears run as containerized services with cluster-native discovery. Each service is an out-of-process gear plus the system gears it depends on. The cluster-plane coordination primitives (leader election, distributed locks, distributed cache) are **designed but not yet implemented** — see [Status and roadmap](../../capabilities/status-and-roadmap/).

## Build considerations

- **FIPS** — build with `--features fips` to route TLS through a validated crypto provider on Linux/macOS/Windows. See [Compliance and FIPS](../../concepts/compliance-and-fips/).
- **Configuration** — deployment differences are expressed in config and environment overrides. See [Configure a Gears application](../configure/).

## See also

- [Deployment shapes](../../concepts/deployment-shapes/) — the model in depth.
- [Runtime and lifecycle](../../concepts/runtime-and-lifecycle/) — startup and shutdown.

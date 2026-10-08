---
title: Deploy Gears
description: Run the same gear code as a single node, across processes, or as containers on Kubernetes — selected by configuration.
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

## Self-hosted / multi-node (Profile 2)

A `HostRuntime` process runs one or more gears in-process, while selected gears run as separate REST-first worker processes managed by the same host. Out-of-process gears self-register with the directory service, and the host's edge reverse-proxies their public routes.

```yaml
gears:
  hello:
    runtime:
      type: oop
      execution:
        executable_path: "./target/debug/hello-oop"
        args: ["--config", "config/oop-hello.yaml"]
```

The spawned worker connects to the host's `DirectoryService` using `TOOLKIT_DIRECTORY_ENDPOINT` and serves its own REST endpoints. gRPC remains available as an opt-in transport per gear. See [Run a gear out-of-process](../out-of-process/).

## Kubernetes

Gears run as containerized services with cluster-native discovery. Each service is an out-of-process gear plus the system gears it depends on. The cluster-plane coordination primitives (leader election, distributed locks, distributed cache) are **designed but not yet implemented** — see [Status and roadmap](../../capabilities/status-and-roadmap/).

## Build considerations

- **FIPS** — build with `--features fips` to route TLS through a validated crypto provider on Linux/macOS/Windows. See [Compliance and FIPS](../../concepts/compliance-and-fips/).
- **Configuration** — deployment differences are expressed in config and environment overrides. See [Configure a Gears application](../configure/).

## See also

- [Deployment shapes](../../concepts/deployment-shapes/) — the model in depth.
- [Runtime and lifecycle](../../concepts/runtime-and-lifecycle/) — startup and shutdown.

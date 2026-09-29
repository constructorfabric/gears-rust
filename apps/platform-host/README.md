# CF/Gears Platform Host

The orchestrating process for a *distributed* (out-of-process) CF/Gears
deployment.

## Overview

`platform-host` is the composed "platform-host image": it links a small
**co-located core** plus the shared **system gears** and runs them under the
ToolKit `HostRuntime`. Application gears run *elsewhere* — as separate
out-of-process (OoP) worker processes or Kubernetes pods — and discover this
host at runtime via its `DirectoryService`.

It is the distributed counterpart to `cf-gears-example-server`, which links
*every* gear into a single embedded (Profile 1) process. `platform-host`
deliberately links *only* the gears that must stay co-located, so the remaining
application gears can be deployed and scaled independently:

- **Profile 2 (Host + Workers)** — this host runs the directory + edge; OoP
  worker processes register with it over UDS (single-node) or TCP.
- **Profile 3 (K8s Native)** — this host runs as the platform pod; each
  application gear runs as its own pod, fronted by an external gateway.

The authoritative model lives in `docs/arch/toolkit-oop/`: ADR-0001 (deployment
profiles) and the DESIGN "Platform Host Composition" section.

## What it links

Everything below is co-linked into this single image *today*, but the bundle is
not a fixed boundary: the long-term Profile 3 shape extracts gears into their
own pods/images as the enabling work lands. Both groups can be split eventually;
what differs is how much work a split still needs (see DESIGN § Platform Host
Composition).

### Core gears (co-located for now)

These ship together in this image *today* because they currently reach their
peers under a synthetic / anonymous `SecurityContext` (the DESIGN calls this the
*trust-coupled core*). That coupling is a removable property, not an intrinsic
one: splitting them across a network boundary needs enabling work first — remote
(REST/gRPC) surfaces on the peers they call, plus IdP-issued / S2S credentials
to replace the anonymous context. Until then, they run in one process.

- `authz-resolver` — policy decision point; chains to `tenant-resolver` →
  `resource-group` under an anonymous context.
- `tenant-resolver` — resolves the tenant hierarchy (the `rg` plugin reads
  `resource-group`'s DB directly).
- `resource-group` — foundation the AuthZ + tenant-resolution chain builds on.
- `account-management` — tenant/user lifecycle; runs background flows as a
  synthetic system actor (`am.system`).

### System gears

Shared platform infrastructure. These have no such coupling, but they are
*not uniformly* splittable: some already expose a remote (REST/gRPC) surface and
can be extracted as-is, while others have no remote surface yet and would need
one added before they could run as their own pod.

- `gear-orchestrator` — hosts the `DirectoryService` that OoP gears register with.
- `grpc-hub` — the gRPC transport surface for the directory and platform-plane RPCs.
- `api-gateway` — the built-in edge; reverse-proxies `exposed` OoP routes it
  discovers via the directory (see ADR-0003, gateway abstraction).
- `types-registry` — the Global Type System (GTS) catalogue.
- `credstore` — secret retrieval.
- `authn-resolver` — turns a bearer token into a tenant `SecurityContext`.

Gear isolation for OoP images is achieved by the dependency graph (which gears a
binary links), not by `#[cfg]` gates — see `src/registered_gears.rs`. That is
also what makes an eventual split mechanical: once a gear's prerequisites are
met, extracting it is a matter of moving it to its own binary's dependency set.

## Plugins & vendor selection

Plugin *availability* is a build-time choice (which plugin crates are linked),
selected via the `dev-plugins` / `prod-plugins` Cargo feature presets. The
*active* vendor is chosen at runtime from each resolver's GTS `vendor` config.
`dev-plugins` (the default) boots with zero external dependencies for
CI/demo/on-prem; `prod-plugins` wires the real OIDC/tenant-resolver-backed
stack. See `Cargo.toml` for the exact preset definitions.

## Feature flags

- `fips` — route all crypto through the AWS-LC FIPS-validated module.
- `k8s` — Kubernetes platform-plane auth (SA-token TokenReview validation).
- `otel` — OpenTelemetry tracing/metrics export.

## Usage

```text
platform-host --config <path> [run]     # start the host (run is the default)
platform-host --config <path> migrate   # run DB migrations and exit
platform-host --config <path> --list-gears
platform-host --config <path> --print-config
```

A ready-to-run dev config (the `dev-plugins` preset over SQLite) lives at
`config/platform-host.yaml`.

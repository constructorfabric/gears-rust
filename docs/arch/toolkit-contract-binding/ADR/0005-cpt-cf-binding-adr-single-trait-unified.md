---
status: rejected
date: 2026-04-10
---

# Single Unified Contract Trait (rejected — two-layer design retained)

**ID**: `cpt-cf-binding-adr-single-trait-unified`

## Context

This ADR slot was reserved to evaluate collapsing the **base contract trait** and its
**transport projection** into a single unified trait carrying both the domain contract and the
transport annotations (e.g. `#[toolkit_contract(binding = [compile, rest])]`).

## Decision

**Rejected.** The two-layer design (a plain base trait + a separate `*Rest`/`*Grpc` projection that
extends it) is retained, as decided in **ADR-0001** (`cpt-cf-binding-adr-contract-source-of-truth`)
and extended in **ADR-0003** (`cpt-cf-binding-adr-projection-server-gen`).

A single unified trait was rejected for the reasons recorded in ADR-0001's alternatives table and in
ADR-0003 Option B:

- it mixes transport concerns into the domain interface, forcing compile-time plugins to carry
  annotation weight for transports they do not use;
- REST annotations do not apply to gRPC, so a unified trait accumulates a stack of per-transport
  attributes on every method as transports are added;
- adding a new transport would require modifying the base trait rather than being purely additive.

## Status

No separate mechanism is introduced by this ADR. It exists only to record that the single-trait
alternative was considered and not adopted; see ADR-0001 and ADR-0003 for the governing decisions.

## More Information

- ADR-0001 — contract source of truth: [`./0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
- ADR-0003 — projection server generation (Option B "collapse onto base" rejected): [`./0003-cpt-cf-binding-adr-projection-server-gen.md`](./0003-cpt-cf-binding-adr-projection-server-gen.md)

# Feature: Lock-Misuse Lint (no-remote-in-critical-section)

- [ ] `p1` - **ID**: `cpt-cf-clst-featstatus-lock-lint-implemented`

- [x] `p2` - `cpt-cf-clst-feature-lock-lint`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Build Fails on Remote Call Inside a Critical Section](#build-fails-on-remote-call-inside-a-critical-section)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Detect Remote I/O in Critical Section](#detect-remote-io-in-critical-section)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Workspace Lock-Misuse Lint](#workspace-lock-misuse-lint)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Makes the no-remote-I/O-in-critical-section rule enforceable rather than aspirational, via a workspace architecture lint rule (via `cargo gears lint`) that flags cross-instance remote calls inside a cluster lock's critical section at compile time. It is sequenced after the lock primitive so the lint has real acquire/release scopes to target.

### 1.2 Purpose

Forbidding remote I/O inside the critical section, combined with async timeouts on every operation, bounds the critical section (see [ADR-002](../ADR/002-async-boundary-no-remote-in-critical-section.md)). Compile-time enforcement prevents the rule from rotting into documentation nobody reads.

**Requirements**: `cpt-cf-clst-fr-lock-no-remote`, `cpt-cf-clst-nfr-bounded-critical-section`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Writes lock-using code that the lint checks at build time |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.3 (no remote I/O in critical section), §6.1 (bounded critical section NFR)
- **Design**: [DESIGN.md](../DESIGN.md) §2.2 (constraint and lint scope), §1.2 (NFR allocation)
- **ADRs**: [ADR-002](../ADR/002-async-boundary-no-remote-in-critical-section.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-distributed-lock`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — not applicable: the lint runs at build time and has no runtime performance dimension.
- Reliability — addressed: compile-time enforcement of the no-remote-in-critical-section rule prevents the unbounded-pause failure mode (§2–§3).

## 2. Actor Flows (CDSL)

### Build Fails on Remote Call Inside a Critical Section

- [ ] `p1` - **ID**: `cpt-cf-clst-flow-lock-lint-build-fail`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- Lock-using code that performs only local work inside the critical section builds cleanly.

**Error Scenarios**:
- A remote cluster call placed between acquisition and release fails the build with a deny-level diagnostic.

**Steps**:
1. [ ] - `p1` - Developer writes code that acquires a lock and runs a critical section - `inst-bf-acquire`
2. [ ] - `p1` - **IF** the critical section contains a remote cluster backend call - `inst-bf-remote`
   1. [ ] - `p1` - The lint emits a deny-level diagnostic and the build fails - `inst-bf-deny`
3. [ ] - `p1` - **ELSE** - `inst-bf-local`
   1. [ ] - `p1` - The build succeeds - `inst-bf-ok`

## 3. Processes / Business Logic (CDSL)

### Detect Remote I/O in Critical Section

- [ ] `p1` - **ID**: `cpt-cf-clst-algo-lock-lint-detect`

**Input**: Consumer code using the cluster lock primitive

**Output**: A deny-level diagnostic when remote I/O occurs inside a critical section

**Steps**:
1. [ ] - `p1` - Identify the scope between lock acquisition and release - `inst-dt-scope`
2. [ ] - `p1` - **FOR EACH** call within that scope - `inst-dt-foreach`
   1. [ ] - `p1` - **IF** the call targets a remote method of one of the four cluster backend traits - `inst-dt-match`
      1. [ ] - `p1` - Emit a deny-level diagnostic naming the offending call - `inst-dt-flag`
3. [ ] - `p1` - Limit the initial scope to the four cluster backend traits; database-transaction enforcement is a follow-up extension - `inst-dt-scope-limit`

## 4. States (CDSL)

Not applicable — the lint is a static-analysis rule with no runtime entity lifecycle.

## 5. Definitions of Done

### Workspace Lock-Misuse Lint

- [ ] `p1` - **ID**: `cpt-cf-clst-dod-lock-lint-rule`

The system **MUST** provide a workspace architecture lint rule (via `cargo gears lint`, modeled on the existing drop-zeroize lint) that flags, at deny level, cross-instance remote calls inside a cluster lock's critical section, scoped initially to the four cluster backend traits between acquisition and release.

**Implements**:
- `cpt-cf-clst-flow-lock-lint-build-fail`
- `cpt-cf-clst-algo-lock-lint-detect`

**Constraints**: `cpt-cf-clst-constraint-no-remote-in-critical-section`

**Touches**:
- Entities: workspace architecture lint rule (in `cargo-gears` CLI)

## 6. Acceptance Criteria

- [ ] Lock-using code that performs only local work inside the critical section builds cleanly.
- [ ] A remote cluster backend call inside a critical section produces a deny-level diagnostic that fails the build.
- [ ] The lint scope is restricted to the four cluster backend traits between acquisition and release; database-transaction enforcement is documented as a follow-up extension.
- [ ] Zero static-analysis violations exist in workspace consumer code at adoption.

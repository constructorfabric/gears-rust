# Feature: Distributed Lock Primitive

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-distributed-lock-implemented`

- [x] `p2` - `cpt-cf-clst-feature-distributed-lock`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Non-Blocking Acquire and Local Critical Section](#non-blocking-acquire-and-local-critical-section)
  - [Blocking Acquire with Timeout and Extension](#blocking-acquire-with-timeout-and-extension)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [TTL Safety Net](#ttl-safety-net)
  - [Release-If-Still-Holder](#release-if-still-holder)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Lock Backend Trait and Facade](#lock-backend-trait-and-facade)
  - [Lock Guard with Explicit Release and Extension](#lock-guard-with-explicit-release-and-extension)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Provides TTL-bounded distributed locks with non-blocking and blocking-with-timeout acquisition, explicit asynchronous release, and TTL extension for long-running operations. There are no fencing tokens and the lock guard has a no-op drop, consistent with the no-remote-in-critical-section rule (see [ADR-002](../ADR/002-async-boundary-no-remote-in-critical-section.md)).

### 1.2 Purpose

Rate limiting and serialized critical sections need mutual exclusion that recovers from crashed holders. This feature delivers acquire-or-fail and acquire-with-wait semantics, TTL-bounded recovery, and explicit async release, with cleanup safety provided by TTL rather than by Rust drop.

**Requirements**: `cpt-cf-clst-fr-lock-acquire`, `cpt-cf-clst-fr-lock-release`

The companion no-remote-in-critical-section rule (`cpt-cf-clst-fr-lock-no-remote`) is enforced by the separate lock-misuse lint feature; this feature establishes the lock API that the rule governs.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-oagw` | Holds short-lived locks for cross-instance rate limiting |
| `cpt-cf-clst-actor-platform-gear` | Serializes critical sections with TTL-bounded recovery |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.3
- **Design**: [DESIGN.md](../DESIGN.md) §3.3 (lock contract, critical-section rule), §3.1 (LockGuard), §2.2 (constraints)
- **ADRs**: [ADR-002](../ADR/002-async-boundary-no-remote-in-critical-section.md)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-sdk-foundation`
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — backend-determined: acquisition and release latency depend on the bound backend; the bounded-critical-section NFR is enforced by the separate lock-misuse lint feature.
- Reliability — addressed: the TTL safety net and conditional release-if-still-holder (§3) bound crash recovery and prevent a foreign holder from releasing the lock.

## 2. Actor Flows (CDSL)

### Non-Blocking Acquire and Local Critical Section

- [x] `p1` - **ID**: `cpt-cf-clst-flow-distributed-lock-try-critical`

**Actor**: `cpt-cf-clst-actor-oagw`

**Success Scenarios**:
- The lock is acquired, a local-only critical section runs, and the lock is released explicitly.

**Error Scenarios**:
- The lock is already held — acquisition fails fast with a contention error.

**Steps**:
1. [x] - `p1` - Consumer requests non-blocking acquisition of a named lock with a TTL - `inst-tc-try`
2. [x] - `p1` - **IF** the lock is already held - `inst-tc-held`
   1. [x] - `p1` - **RETURN** a lock-contended error - `inst-tc-contended`
3. [x] - `p1` - Consumer performs only local computation inside the critical section (no remote calls) - `inst-tc-local`
4. [x] - `p1` - Consumer releases the lock explicitly - `inst-tc-release`
5. [x] - `p1` - **RETURN** success - `inst-tc-return`

### Blocking Acquire with Timeout and Extension

- [x] `p1` - **ID**: `cpt-cf-clst-flow-distributed-lock-wait`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- The consumer waits up to a timeout to acquire, extends the TTL for a longer operation, then releases.

**Error Scenarios**:
- Acquisition does not succeed within the timeout — a timeout error is returned.
- The TTL elapsed before extension — an expired error tells the consumer it lost the lock.

**Steps**:
1. [x] - `p1` - Consumer requests blocking acquisition with a TTL and a wait timeout - `inst-wt-lock`
2. [x] - `p1` - **IF** not acquired within the timeout - `inst-wt-timeout`
   1. [x] - `p1` - **RETURN** a lock-timeout error reporting how long it waited - `inst-wt-timeout-return`
3. [x] - `p1` - Consumer extends the TTL when the local operation needs more time - `inst-wt-extend`
4. [x] - `p1` - **IF** the lock TTL already elapsed - `inst-wt-expired`
   1. [x] - `p1` - **RETURN** a lock-expired error so the consumer aborts the operation - `inst-wt-expired-return`
5. [x] - `p1` - Consumer releases the lock explicitly - `inst-wt-release`

## 3. Processes / Business Logic (CDSL)

### TTL Safety Net

- [x] `p1` - **ID**: `cpt-cf-clst-algo-distributed-lock-ttl-safety`

**Input**: An acquired lock with a TTL

**Output**: Automatic release bound on the leak window if the holder crashes or forgets to release

**Steps**:
1. [x] - `p1` - Attach the consumer-supplied TTL to the lock entry at acquisition - `inst-ts-attach`
2. [x] - `p1` - **IF** the holder crashes or never releases - `inst-ts-crash`
   1. [x] - `p1` - The backend automatically releases the entry after the TTL elapses - `inst-ts-auto`
3. [x] - `p1` - **RETURN** the bounded leak window to the next acquirer - `inst-ts-return`

### Release-If-Still-Holder

- [x] `p1` - **ID**: `cpt-cf-clst-algo-distributed-lock-release-if-holder`

**Input**: A release request carrying the holder's identity

**Output**: Release of the entry only if the requester still holds it

**Steps**:
1. [x] - `p1` - Compare the requester's holder identity against the current lock entry - `inst-rh-compare`
2. [x] - `p1` - **IF** the requester is not the current holder - `inst-rh-foreign`
   1. [x] - `p1` - **RETURN** without releasing another holder's lock - `inst-rh-skip`
3. [x] - `p1` - Release the entry conditionally (compare-and-delete on the holder token) so a foreign holder cannot release it while this holder's TTL is unexpired - `inst-rh-release`

## 4. States (CDSL)

Not applicable — a lock guard is held or released; there is no multi-state entity lifecycle. Recovery from a crashed holder is bounded by TTL, not by an explicit state transition.

## 5. Definitions of Done

### Lock Backend Trait and Facade

- [x] `p1` - **ID**: `cpt-cf-clst-dod-distributed-lock-backend-facade`

The system **MUST** provide the lock backend trait and facade with non-blocking acquisition (contention error if held) and blocking acquisition with a timeout (timeout error if not acquired), each carrying a TTL, plus a fluent resolver with capability validation. The backend trait **MUST** be dyn-compatible.

**Implements**:
- `cpt-cf-clst-flow-distributed-lock-try-critical`
- `cpt-cf-clst-flow-distributed-lock-wait`
- `cpt-cf-clst-algo-distributed-lock-ttl-safety`

**Touches**:
- Entities: DistributedLockBackend, DistributedLockV1, LockCapability, LockFeatures

### Lock Guard with Explicit Release and Extension

- [x] `p1` - **ID**: `cpt-cf-clst-dod-distributed-lock-guard`

The system **MUST** provide a lock guard supporting explicit asynchronous release and TTL extension, returning an expired error when extension is attempted after the TTL elapsed, with a no-op drop (no I/O in drop) and no fencing tokens. Release **MUST** be conditional on still holding the lock.

**Implements**:
- `cpt-cf-clst-flow-distributed-lock-try-critical`
- `cpt-cf-clst-flow-distributed-lock-wait`
- `cpt-cf-clst-algo-distributed-lock-release-if-holder`

**Constraints**: `cpt-cf-clst-constraint-no-remote-in-critical-section`

**Touches**:
- Entities: LockGuard

## 6. Acceptance Criteria

- [x] Non-blocking acquisition returns a contention error when the lock is held; blocking acquisition returns a timeout error when not acquired within the timeout.
- [x] Every acquisition carries a TTL so a crashed holder cannot block others indefinitely.
- [x] Consumers release locks explicitly; TTL bounds the leak window when they do not.
- [x] Extending an already-expired lock returns a specific expired error.
- [x] A foreign holder cannot release another holder's lock, provided the holder releases before its TTL expires: the conditional (get-then-delete) release narrows but cannot fully close the post-expiry window, so extension before TTL expiry is required for safety.
- [x] The lock guard performs no I/O on drop and exposes no fencing tokens.

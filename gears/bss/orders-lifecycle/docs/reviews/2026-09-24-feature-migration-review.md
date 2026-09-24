# Feature migration review — 2026-09-24

Scope: all eight current feature documents, their summaries and retained detailed contracts, DESIGN, DECOMPOSITION, PRD, decision records, upstream requirements and the saved pre-migration source slices. Review used the FEATURE template, checklist and examples directly, without Studio. This is a document review, not runtime acceptance.

## Review Report (Issues Only)

### 1. Summary contracts and references needed clarification — corrected

**Checklist Item**: `ARCH-FDESIGN-004`, `SEM-FDESIGN-004` — algorithm completeness and semantic consistency.

**Severity**: MEDIUM

#### Why Applicable

Implementers need deterministic guard order and unambiguous references from each feature summary.

#### Issue

Preconditions' recording-party summary could imply refusing during preparation, before the duplicate-acceptance guard. Several summaries omitted explicit inputs/outputs or DoD constraints. Foundation and Capture summaries used ambiguous old section numbers. Audit-read wording did not explicitly distinguish own-tenant reads carrying proof.

#### Evidence

Corrected [Preconditions §3.2](../features/05-preconditions.md#32-bind-assent-to-actor-and-commercial-version), its combined-error acceptance criterion, process fields in features 04–08, DoD constraints in 04–06, Foundation/Capture canonical links and [audit behavior](../features/08-read-and-authz.md#contract-08-3-6).

#### Why It Matters

Preparation order must not change observable refusal precedence; readers must reach the intended canonical contract.

#### Proposal

Applied explicit preparation/evaluation separation, current links, labeled fields and the proof-presence logging condition. Verify the combined duplicate/barred-actor case during implementation.

### 2. Feature boundaries and implementation-status wording needed correction — corrected

**Checklist Item**: `ARCH-FDESIGN-NO-003`, `MAINT-FDESIGN-NO-001`, `DOC-FDESIGN-001`.

**Severity**: MEDIUM

#### Why Applicable

Features describe behavior; decision history belongs in decision records. Planned integration must not appear implemented.

#### Issue

Hold and Expiry retained historical design debates and an inline Rust lock invocation. UI/accessibility non-applicability was implicit. Workflow continuation and decision-register endpoint wording could imply delivered code.

#### Evidence

Historical passages are preserved in [ADR-0005](../ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md#hold-and-resume-guard-rationale) and [D-90](../DECISIONS.md#hold-and-expiry-alternative-history). Feature lock behavior now uses prose with the same keys and zero-wait/retry policy. All features state UI applicability; Preconditions and Q-20/Q-23 use planned/specification wording.

#### Why It Matters

These distinctions prevent implementation details, historical alternatives and delivery claims from obscuring the current contract.

#### Proposal

Corrections applied; retain deployment and upstream evidence as separate acceptance gates.

### 3. Product conformance and upstream delivery remain open — pre-existing

**Checklist Item**: `SEM-FDESIGN-001`, `ARCH-FDESIGN-002` — requirements alignment and architecture contracts.

**Severity**: HIGH

#### Why Applicable

A structurally complete feature cannot establish agreement with contradictory product requirements or deliver an absent integration.

#### Issue

The migration preserves unresolved product and upstream questions; it does not close them.

#### Evidence

[DECISIONS open questions](../DECISIONS.md) include Q-18 (Partner Admin hold rights), Q-31 (unconditional resumability versus resume cap), Q-20 (audit retrieval scope), Q-25 (event triggers and consumer callback reads), Q-11/Q-16 (latency boundary) and Q-26 (operational baselines). The register retains the complete outstanding list. [Release prerequisites](../DECOMPOSITION.md#31-upstream-and-release-prerequisites) and [UPSTREAM_REQS](../UPSTREAM_REQS.md) distinguish existing code from required SDK/provider capabilities.

#### Why It Matters

These features must not be called fully PRD-conformant or production-ready solely because migration and document checks pass.

#### Proposal

Keep the recorded owners and acceptance obligations. Resolve product choices through their existing questions and demonstrate upstream contracts/provider behavior before marking the corresponding features implemented. No requirement or open question was silently resolved in this review.

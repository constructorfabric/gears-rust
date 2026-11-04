# Waits

**Status:** Draft · [All proposals](README.md)

## Problem

- **Answering an input wait needs a broad permission.** Today `POST /wait-conditions/{id}/resolve` needs `update` on the task (DESIGN.md:1098), so a service that only answers a question also gets every other right `update` carries.
- **Resolve is not safe to retry.** Until event waits exist, `resolve` carries request and reply between services, so a lost response must be retryable.

When a wait can be set (create request or worker only) is covered in [Task mutability](01-task-mutability.md).

## Proposal

Timer waits come first, then input and children waits. Event waits follow as a firm p2 commitment. `resolve` gets its own permission and safe retries.

- [Proposed] **Input and children waits stay p2, and p1 is built for them.** The phases do not change (PRD.md:606, :617, and parent/child at :824), but candidate adopters lean on these features, so p1 ships everything they need and they arrive in p2 without a breaking change:
  - **Children:** account-management tenant hard delete (children before parents), orders-workflow (one child per order line), rating re-rate run (a parent run with children).
  - **Input:** orders-workflow (approval before fulfillment).
  - **Kept in the p1 contract:** the wait model with all four kinds (PRD.md:548); `parent_task_id`, `wait_for_children` and `propagate_cancel` on the task (DESIGN.md:296-298); `dependency_failed` among the cancel results; parent before child in the lock order (ADR-0008:86). p1 schemas, storage and API carry them from the start.
  - **Rejected until p2:** setting an input or children condition, and `parent_task_id` on create, return `400` naming the feature, as a type that declares `authz_scope` does today (DESIGN.md:715).
  - **Open sets:** the SDK and consumers treat wait kinds, result codes and event types as open sets, so p2 values do not break them.
- [Proposed] **Event waits after the others.** The core is designed so event waits plug in later through the same resolve path.
  - **When:** once Event Broker issues #5493 (DESIGN.md:912) and #5494 are closed and the approval service has a design (its DESIGN.md is "TODO").
- [Proposed] **`resolve`.** Answering an input wait gets its own permission and is safe to retry.
  - **Its own permission.** `resolve` gets its own action, and `update` keeps only tag changes ([Task mutability](01-task-mutability.md)). A per-wait token was the other option and was not chosen.
  - **Safe to repeat.** The same value again returns success, as completion replay does (DESIGN.md:736); a different value is rejected.
    - **How:** a guarded update that matches only a `pending` input condition. When it matches nothing, the engine reads the condition: `satisfied` with an equal value (canonical JSON) from the same caller → `200` with the stored result and `Idempotency-Replayed: true`, no write; `satisfied` otherwise → `409`; cancelled or timed out → `400` `failed_precondition`. The stored value is the dedup record, so no idempotency key is kept.
    - **Same caller:** the condition stores `resolved_by` (the resolving subject). A second subject sending the same value gets `409`, so the audit trail names the one who answered.
    - **Replay window:** a resolved condition is kept until its task is deleted at retention, so a late retry still finds it.

## Doc changes

- PRD.md:546-548 (p1 carries all four kinds; input and children are rejected until p2), :577-617.
- DESIGN.md:360-372 (`resolved_by` on the wait condition), :819 (resolve replay and retention), :912, :1098.

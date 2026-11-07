# Dedup and supersession

**Status:** Draft · [All proposals](README.md)

## Problem

- **`authz_scope` dedup hashes the wrong rights, and cannot run anyway.** It hashes the caller's right to create the type in the queue (DESIGN.md:2534, :2564), not the caller's rights on the data, so two callers who see different data can still share a result.
- **The create transaction calls the PDP.** Supersession calls `authorize(cancel)` after `BEGIN` (DESIGN.md:2578, :2601, :2378), and the parent check `authorize(update)` does too (DESIGN.md:2632). That holds row locks across a remote call, against the design's own rule (DESIGN.md:722, :2540).

## Proposal

- [Proposed] **Drop `authz_scope`.** The dedup mode is removed.
  - **Instead:** a type whose result depends on the caller uses `subject` scope or puts the relevant role or group in `key_fields`; otherwise `tenant`.
- [Proposed] **No PDP call inside the create transaction.** Authorize before `BEGIN`, re-check the plan inside, and retry from the top if it changed.

## Doc changes

- PRD.md:301, :319-335, :1493.
- DESIGN.md:715-716, :1228, :2378, :2564, :2596-2636.
- ADR-0012:56, :65.
- `schemas/types/gts.cf.core.tasks.task.v1~.schema.json`: remove `authz_scope` from the dedup scope enum.

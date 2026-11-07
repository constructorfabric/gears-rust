# Task mutability

**Status:** Draft · [All proposals](README.md)

## Problem

Today an API consumer can change a task after creating it: add or remove dependencies, park it on wait conditions, and create children under it with `update` permission on the parent. Every one of those second calls races with the claim. A worker can take the task, and even complete it, before the second call arrives (PRD.md:841, :258). A parent with no children yet has nothing to wait for.

Supporting these changes also costs the design:

- **A lock and a graph search.** Later dependency changes are the only reason for the per-tenant dependency-graph lock and the cycle search of up to 10,000 nodes (DESIGN.md:1599, :2870-2898). A task that is being created cannot close a cycle, because nothing depends on it yet.
- **Extra transitions, with a bug.** `queued → blocked` and `scheduled → blocked` exist only for later dependency additions. Adding a predecessor to an `ANY` task that already has a finished predecessor blocks it again, because `if any unmet: s → blocked` ignores the mode (DESIGN.md:2904).
- **Leases revoked mid-run.** A consumer can park a queued, claimed or running task, which ends the worker's lease while it works (DESIGN.md:532, PRD.md:548-550).

Tags are different: they only affect search, not scheduling, so changing them races with nothing that matters. They stay changeable.

## Proposal

Everything that affects how a task runs is set in the create request. Afterwards an API consumer can cancel the task, answer its input waits and change its tags, nothing else. The worker running a task can still set waits on it and create children of it.

- [Proposed] **Set only in the create request.** After creation, an API consumer cannot change a task's dependencies or waits.
  - **Dependencies.** Predecessors and the `ALL`/`ANY` mode are set on `POST /tasks` and never change afterwards.
  - **Failed predecessor.** Each predecessor carries `on_predecessor_failure: cancel | ignore`, default `cancel`. `cancel` is today's rule: a predecessor that ends `failed`, `cancelled` or `dead_lettered` cancels the successor with `dependency_failed` (PRD.md:839). `ignore` counts it as satisfied, so the successor still runs; linking a predecessor that has already failed is accepted.
  - **Instead of removing a dependency.** An operator who wants a successor to run past a hung predecessor cancels the predecessor; the successor runs if its link says `ignore`. BullMQ's `ignoreDependencyOnFailure`, also set at creation, is the closest match. BullMQ also offers `removeChildDependency()`, but its flows are trees (one parent per job), so removal there needs no cycle check; workflow engines (Temporal, Hatchet, Airflow, Argo) fix dependencies when the run starts.
  - **Waits.** A consumer can no longer park a task. The create request can carry an initial wait group, so a task can start in `waiting` (for example "run when approved"). Waits set by the worker during execution stay as they are, including arm-then-park.
  - **Tags stay changeable.** `PATCH /tasks/{id}/tags` stays (PRD.md:363), for example to rename old tag keys. A new limit on tags per task bounds them.
- [Proposed] **Only the parent's worker adds children.** A child of an existing task can only be created by the worker that holds the parent's lease, with `parent_lease_token`.
  - **Change:** the alternative of `update` permission on the parent goes (PRD.md:855, DESIGN.md:2630-2632). The PRD only describes the running task creating children (PRD.md:826, :831).
  - **Fan-out and join:** a creator creates the child tasks first, then a join task with them as predecessors, using `on_predecessor_failure: ignore` where failures are acceptable.
  - **New work for a running job** reaches it through an input wait, and its worker creates the children.
- [Proposed] **`update` covers only tags; `resolve` gets its own action.** A service that answers a question gets no other right on the task.
  - **Today:** `update` covers tags, dependency changes, consumer waits, children without the parent's lease, and answering input waits (DESIGN.md:986). After the two items above, only tags and answering input waits are left.
  - **Change:** `update` keeps only tag changes (PRD.md:925). Add a `resolve` action that allows only answering an input wait (`POST /wait-conditions/{id}/resolve`, DESIGN.md:1098). See [Waits](07-waits.md).

### API changes

- **Remove** `POST /tasks/{id}/dependencies`.
- **Remove** `DELETE /tasks/{id}/dependencies/{predecessor_id}`. The `on_predecessor_failure` setting covers the case it served.
- **Remove** the API-consumer path of `POST /tasks/{id}/wait-conditions`. The worker path, with a lease token, stays.
- **Add** an optional wait group to `POST /tasks`.
- **Add** `on_predecessor_failure` (`cancel` or `ignore`, default `cancel`) to each predecessor on `POST /tasks`.
- **Add** a limit on tags per task.
- **Change** `parent_task_id` on `POST /tasks` to require `parent_lease_token`; `update` on the parent no longer allows it.
- **Change** `POST /wait-conditions/{id}/resolve` to need the new `resolve` action instead of `update`.
- **Add** the `resolve` task action and its permission instance `gts.cf.toolkit.authz.permission.v1~cf.task_engine._.task_resolve.v1`. The `update` action and `…task_update.v1` stay, for tags only.
- **Keep** `PATCH /tasks/{id}/tags`, `POST /tasks/{id}/cancel` and every worker endpoint.

## What we give up

- Changing dependencies or waits, or adding children, after creation. A task created wrongly is cancelled and created again.
- Unblocking a successor whose link says `cancel` while its predecessor hangs. The operator cancels both and creates the successor again.

## Doc changes

- PRD.md:258 (create placement), :363 (tag limit), :548-550 (who sets waits), :608 (input wait), :837-841 (predecessor failure, dependency changes), :855-856 and :1505 (who may create a child or change dependencies), :922 (task actions: add `resolve`), :925 (`update` covers tags only).
- DESIGN.md:532 (park transition), :726-727 and :1062 (child needs the parent's lease), :986 (actions table), :1098 (resolve permission), :1599 (lock order step 0), :2356 and :2630-2632 (child under a parent), :2870-2906 (dependency addition), the `te_dependencies` table and the dependency cascade (new column for the setting).
- DESIGN §3.3 API table: the removed and added endpoints (DESIGN.md:1075-1077, :1095, :1098).
- `schemas/instances/`: add `…task_resolve.v1.json`; keep `…task_update.v1.json` and narrow its description to tags.

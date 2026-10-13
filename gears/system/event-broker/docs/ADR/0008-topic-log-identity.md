# ADR-0008: Topic Log Identity

## Status

Draft. The identity itself and how it is minted are decided and implemented by feature 0006. Everything about checking it, and reacting when a check fails, is open and is what this ADR is for.

## Context and Problem Statement

A topic's events live in one storage backend, chosen by `backend_routing` (feature 0005). Configuration names the backend; nothing proves that the backend it names actually holds the topic's log. Three failures follow from that:

1. **Misrouted after a mistake.** `topic_A`'s log lives in `pg_cluster_1`. A configuration change routes it to `pg_cluster_2` by mistake. New events go to `pg_cluster_2`, `pg_cluster_1` goes stale, and consumers see what looks like a fresh log.
2. **Ingest and delivery disagree.** Ingest routes `topic_A` to one backend and delivery to another. Producers write to one place and consumers read from another, and both look healthy.
3. **Briefly, during a rollout.** Either of the above for the seconds a configuration rollout takes, which is enough to write events to the wrong backend or to serve reads from one.

Backend addresses, host names and registry names all legitimately change, and a topic's data may be moved between backends - of the same type or not - by tools the broker knows nothing about. So a topic cannot be bound to a backend by where the backend is. It has to be bound by something the backend holds.

## Decision Drivers

- Detect a backend that does not hold a topic's log, whatever its address or registry name.
- Work across media: a database, a directory, a Kafka cluster, an object store.
- Survive data being moved by an external tool.
- Never let one topic be live on two backends.
- Keep it small: this is a misconfiguration detector, not a migration mechanism.

## Considered Options

1. **Per-topic log id, minted by the broker, stored by the backend.**
2. **Operator-assigned backend id in configuration.**
3. **Per-backend-instance identity reported by the plugin** (a marker row, a Kafka `cluster.id`).
4. **Content anchor**: the broker remembers a recent `(sequence, event id)` per partition and reads it back.

## Decision Outcome

Option 1, a **log id**: a random UUID v4 identifying one topic's log inside one backend.

Decided now (feature 0006):

- Minted by ingest at every provisioning attempt; an attempt that is abandoned and replaced gets a new one, a slow or `Pending` one keeps its own.
- Stored by the backend with the topic's data, in whatever form its medium allows.
- Recorded in the ingest database, `event_broker__topic_binding(topic_uuid, topic_id, log_id, state, attempt_at, partitions)`.
- Guarded by compare-and-set on both sides: the record admits one live attempt, and the backend accepts a new id only over nothing, over the id being replaced, or over itself (the same attempt, continued). So at most one backend holds the recorded log id.
- An abandoned attempt that reached another backend leaves an orphan log id there that no record names. Until O1 decides where the check runs, nothing refuses writes to that backend, so an instance routed to it stays able to write the topic there.
- The age after which an attempt counts as abandoned is a setting.
- The backend operations that carry it take no `SecurityContext`: backends are plugins linked into the gear, and the ones that reach a remote store do so with their own credentials.

Open - to be decided here before any check exists:

### O1. Where the check runs

| Option | Catches | Cost |
|---|---|---|
| a. At start only | cases 1, 2 at boot | misses anything that changes while running, and case 3 |
| b. Periodically | cases 1, 2 within a tick | misses case 3; N instances x T topics calls per tick |
| c. On every operation: `append`, `read`, `resolve`, `query` and `watch` carry the expected log id and the backend refuses a mismatch | 1, 2 and 3 | one comparison per call, in the same statement where the medium allows; on `append` it rides in `AppendRequest` beside the `Fence` and is checked in the same predicate (feature 0007 §2.2) |

A periodic check is not wanted. The lean is (c), possibly with (a) as an early warning.

(c) needs no new operation: the expected log id travels with each call. (a), like (b), compares against the id the backend stores, which takes a backend operation that reads a topic's log id. None exists; one is added only if the chosen option needs it.

### O2. What a failed check does

Decided elsewhere, for the MVP:

- An `append` answered `NotProvisioned` or `LogConflict` is retried endlessly; its outbox slot stalls until an operator fixes the cause (feature 0007).
- A provisioning attempt answered `LogConflict` is retried with the same log id, and `attempt_at` is refreshed, so the attempt is never treated as abandoned and replaced (feature 0006).

Reactions beyond these are open here:

- For the topic: unavailable on that instance (`503`, retryable) versus the whole instance failing.
- Signals: log level, readiness, a metric.
- Whether a mismatch seen during a known rollout is treated differently from one that persists.

### O3. Moving a topic between backends

- **Moved, never copied**: at most one backend holds a topic's log id at any moment. The moving tool copies data and log id, then removes the log id from the source.
- A whole-medium copy (a dump, a file, every topic of a cluster) must remove on the destination every log id except the moved topics'.
- A move between backend types is allowed: the log id, not the backend type, is the identity.
- How the broker notices the move: with (c), the next operation on the new route succeeds and the old route is refused.

### O4. Losing the ingest database

With no record, the next provisioning attempt meets a backend that already holds a log id and is refused. Options: adopt the backend's id when there is no record (with a warning), or require an operator action per topic.

Adopting takes a backend operation that reads a topic's stored log id. None exists; one is added only if this option is chosen.

### O5. Reprovisioning after data loss

A datacenter can lose part of a topic's log - the last events of some partitions, or all of it - and the operator accepts the loss and reprovisions the topic on the same backend. The trigger is a CLI, to be defined. It is a provisioning attempt that replaces the current log id (`replacing = Log(current)`), keeping whatever data survived.

What that exposes today, when the backend's highest sequence falls below positions clients already hold:

| Case | Behaviour today |
|---|---|
| SEEK to a held offset above the new ceiling | `400`, reason `above_high_water_mark`; the SDK consumer's `run()` fails with no fallback |
| Stream opened on a stored cursor above the ceiling | not re-validated; the session idles |
| Live reader above the ceiling | idles on heartbeats; new events numbered at or below its position are skipped silently |
| Partition cache | keeps serving pre-loss events; a refetch of already-accounted sequences is dropped |
| Sequence assignment | continues from the backend's persisted counter if it survived; restarts or regresses - reusing sequences clients have seen - if it did not |
| Producer chain | accepted by the broker (EB database unaffected); backend deduplication keys on the outbox fence (feature 0007 §2.2), not on producer chain numbers, so a backend never drops a batch for its chain values |

Candidate rule: reprovision resumes numbering above what clients may hold (`resume_above` per partition), so no sequence is ever reused and clients above the new tail simply wait across a gap. Open: whether the operator supplies it or the broker records it.

### O6. Delivery

Whether delivery checks the log id itself.

A topic that is registered and not provisioned reads as an empty partition: head 0, cursor range `[0, 0]`, SEEK resolves to 0. The backend answers `NotProvisioned`, and delivery maps it (feature 0007).

### Consequences

- Every backend plugin stores a log id per topic.
- Moving a topic becomes a data-plus-log-id operation with a precise contract for the tool that does it.
- Until O1-O6 are decided, the log id is minted and stored but nothing reads it back.

## Pros and Cons of the Options

### 1. Per-topic log id

- Good: survives address, name and type changes; travels with the data it identifies; one topic can be moved without the rest of its backend.
- Good: works on any medium that can store a small value next to a topic.
- Bad: every plugin and every moving tool must handle it.

### 2. Operator-assigned id in configuration

- Good: nothing for plugins to store.
- Bad: identifies the configuration, not the data; a restored or moved dataset carries no proof, and the mistake it should catch is a configuration mistake.

### 3. Per-backend-instance identity

- Good: one value per backend.
- Bad: identifies the machine, not the log; moving one topic, or mirroring a cluster, changes or duplicates it; a Kafka `cluster.id` changes under MirrorMaker although the data is the same.

### 4. Content anchor

- Good: plugins need nothing new.
- Bad: retention deletes the anchor; the broker learns at most partition heads on the write path, never which event holds which sequence; a faithful copy is indistinguishable from the original.

## More Information

- Feature 0005 - backend registry, backend routing, affinity
- Feature 0006 - topic provisioning, where the log id is minted and stored
- ADR-0006 - offset authority (consumer-owned progress)
- Feature 0007 - storage backend API: `backend::Backend`
- `event-broker-sdk`, `backend` module - `Backend`, the contract a backend's sequence assignment, deduplication and position resolution follow

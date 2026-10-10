# Feature: Topic Provisioning

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Log Id and the Binding Record](#2-log-id-and-the-binding-record)
  - [2.1 Log Id](#21-log-id)
  - [2.2 Binding Record](#22-binding-record)
  - [2.3 Backend Contract](#23-backend-contract)
  - [2.4 Configuration](#24-configuration)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Provisioning Attempt](#31-provisioning-attempt)
  - [3.2 Publishing to a Topic Not Yet Provisioned](#32-publishing-to-a-topic-not-yet-provisioned)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Actor Flows (CDSL)](#5-actor-flows-cdsl)
  - [5.1 A New Topic Appears](#51-a-new-topic-appears)
  - [5.2 An Attempt Is Abandoned](#52-an-attempt-is-abandoned)
  - [5.3 The Backend Holds Another Log Id](#53-the-backend-holds-another-log-id)
- [6. Out of Scope](#6-out-of-scope)
- [7. Definitions of Done](#7-definitions-of-done)
- [8. Acceptance Criteria](#8-acceptance-criteria)
- [9. Unit Test Plan](#9-unit-test-plan)
- [10. E2E Test Plan](#10-e2e-test-plan)

## 1. Feature Context

### 1.1 Overview

Before a topic takes its first event, its backend has to be told to hold it. Ingest does that once per topic - a **provisioning attempt** - and records the outcome in the ingest database.

Every attempt mints a **log id** and hands it to the backend, which stores it with the topic's log. The log id is what will later let the broker tell whether a backend really holds a topic's log (ADR-0008). This feature only mints and stores it; nothing reads it back yet.

### 1.2 Purpose

- Provision each new topic exactly once, however many ingest instances see it, and however long its backend takes.
- Keep the recorded log id on at most one backend.
- Leave every provisioned topic with a log id, stored by its backend and recorded by the broker, and with the partition count the backend actually holds.

### 1.3 Actors

- **Ingest instance**: runs provisioning attempts and keeps the binding record.
- **Storage backend plugin**: creates what it needs to hold a topic's log and stores its log id, in whatever form its medium allows.

### 1.4 References

- Feature 0005 - backend registry, backend routing, affinity
- Feature 0007 - storage backend API: `provision`, `TopicSpec`, `LogClaim`
- ADR-0008 - topic log identity: checks, reactions, moves, reprovisioning
- DESIGN.md §3.1 "Topic" - ingest as the one writer of shared state
- `event-broker/src/infra/specification.rs` - `bulk_load`; `event-broker/src/infra/workers/specification_refresh.rs`
- `gts::GtsId::to_uuid` - the deterministic topic UUID

## 2. Log Id and the Binding Record

### 2.1 Log Id

- A random UUID v4 identifying one topic's log inside one backend.
- Minted by ingest at every provisioning attempt. An attempt that is abandoned and replaced gets a new one; an attempt that is slow, `Pending` or refused keeps its own.
- Stored by the backend with the topic's log: a row in a database, a marker file in a directory, a record in a compacted topic, a marker object in a bucket.

### 2.2 Binding Record

Ingest is the only writer. Portable types only, so the table runs on SQLite, PostgreSQL and MySQL/MariaDB.

```sql
CREATE TABLE event_broker__topic_binding (
    topic_uuid  VARCHAR(36)   PRIMARY KEY,   -- GtsId::to_uuid(topic_id), deterministic
    topic_id    VARCHAR(1024) NOT NULL,      -- GTS topic instance identifier; for operators, not indexed
    log_id      VARCHAR(36)   NOT NULL,      -- UUID v4: the current attempt's, then the provisioned log's
    state       VARCHAR(16)   NOT NULL,      -- 'provisioning' | 'provisioned'
    attempt_at  VARCHAR(32)   NOT NULL,      -- RFC 3339 UTC, database time: when the current attempt last acted
    partitions  BIGINT                       -- the count the backend holds; set when provisioned
);
```

- The primary key is the topic's deterministic UUID, the same identity the head-cache keys use, so the 1024-character identifier never needs an index.
- The record says which log a topic has, not where it is: every instance resolves the backend from its own routing, and every instance carries the same routing (feature 0005 §2).
- `state` separates "not created yet" from "created". A `provisioned` topic is provisioned again only when its resolved settings change (its retention): the same log id, so the backend adopts the new settings without a new attempt (feature 0007 §2.6).
- `partitions` is the count every role uses for the topic once it is provisioned (feature 0007 §2.6).
- `attempt_at` is written and compared with the database's clock in the same statement, so clock skew between ingest hosts never decides whether an attempt is abandoned.

### 2.3 Backend Contract

One operation on the backend trait (feature 0007 §2.6):

```rust
/// Creates whatever the backend needs to hold `topic`'s log, and stores `claim.new` as its log id.
///
/// Compare-and-set: accepted when nothing is stored for the topic, when the stored id is the
/// one `claim.replacing` names (the attempt being replaced), or when it is already `claim.new`
/// (the same attempt, continued). Any other stored id is `BackendError::LogConflict`.
async fn provision(&self, topic: &TopicSpec, claim: &LogClaim) -> Result<Provisioning, BackendError>;

pub struct LogClaim { pub replacing: Replacing, pub new: LogId }
pub enum Replacing { Nothing, Log(LogId) }

pub enum Provisioning {
    /// The log exists and accepts writes; `partitions` is the count the backend holds.
    Ready { partitions: u32 },
    /// Provisioning was started and has not finished; call `provision` again with the same `new`.
    Pending,
}
```

- `Pending` exists because provisioning is not instant everywhere: creating a Kafka topic, or applying a bucket policy, completes on the backend's schedule.
- No `SecurityContext`: backends are plugins linked into the gear, and one that reaches a remote store does so with its own credentials.

### 2.4 Configuration

```yaml
modules:
  event_broker:
    provisioning:
      attempt_timeout: 5m    # an attempt that has not acted for this long may be replaced by a new one
```

`attempt_timeout` must exceed the longest a backend plausibly takes to answer one `provision` call; an attempt that keeps getting answers - `Pending` or `LogConflict` - refreshes `attempt_at` and is never replaced.

## 3. Processes / Business Logic (CDSL)

Pseudo-code below is Python-ish - not runnable; intent over syntax.

### 3.1 Provisioning Attempt

Runs on its own worker on every ingest instance, for each registered topic that is not `provisioned`: at start, after `bulk_load`, whenever the specification refresh registers a topic, and again on a short backoff for an attempt answered `Pending` or `LogConflict`. Each `provision` call carries a timeout, and topics on different registry entries are driven concurrently, so a slow backend delays only its own topics and never the specification refresh. The compare-and-set on the record decides which instance acts.

```python
def provision_topic(topic):
    backend = registry[routing(topic)]                     # feature 0005 §2.2
    spec = TopicSpec(topic, partitions=settings(topic).partitions)
    row = binding.get(topic.uuid)

    if row is None:
        log = LogId.new_v4()
        if not binding.insert_if_absent(topic, log, "provisioning", db_now()):
            return                                         # another instance's attempt is live
        return drive(topic, backend, spec, replacing=Nothing, new=log)

    if row.state == "provisioned":
        if row.partitions != spec.partitions:
            log.error("topic partition count differs from the provisioned count",
                      topic=topic)                         # the recorded count stays in force
        return

    if not binding.older_than(topic.uuid, attempt_timeout):   # compared in the database
        return drive(topic, backend, spec, replacing=Log(row.log_id), new=row.log_id)   # continue

    # the attempt has not acted for attempt_timeout: replace it
    log = LogId.new_v4()
    if not binding.cas(topic.uuid, (row.log_id, "provisioning"), (log, "provisioning", db_now())):
        return                                             # someone replaced it first
    return drive(topic, backend, spec, replacing=Log(row.log_id), new=log)

def drive(topic, backend, spec, replacing, new):
    match backend.provision(spec, LogClaim(replacing, new)) within call_timeout:
        case Ready(partitions):
            binding.cas(topic.uuid, (new, "provisioning"), (new, "provisioned", partitions))
        case Pending:
            binding.touch(topic.uuid, new, db_now())       # same `new`, retried on backoff
        case LogConflict:
            binding.touch(topic.uuid, new, db_now())       # same `new`; never treated as abandoned
            log.error("topic provisioning refused: the backend holds another log id",
                      topic=topic, log_id=new)             # reaction: ADR-0008
        case timeout | Unavailable:
            pass                                           # retried on backoff; ages toward replacement
```

- **One live attempt.** The record admits one attempt at a time: the insert for a new topic, and the compare-and-set for a replacement. Any instance may continue a live attempt, because continuing it sends the same log id.
- **At most one backend holds the recorded log id.** The backend accepts a new id only over nothing, over the attempt it replaces, or over itself. An abandoned attempt that reached another backend - under routing that changed in between - leaves an id there that no record names; that backend stays writable to anyone still routed to it until ADR-0008 O1 decides how operations are checked.
- **Pending is not a new attempt.** The backend is called again with the same id until it answers `Ready`.
- **A refused attempt is not abandoned.** `LogConflict` refreshes `attempt_at` and keeps the same id, so the record does not churn through new log ids; the topic stays `provisioning` and its publishes keep answering `503` until an operator acts (ADR-0008 O2).
- **Only silence ages an attempt.** An attempt is replaced only when no instance has acted on it for `attempt_timeout` - a crash, or a backend that never answers.

### 3.2 Publishing to a Topic Not Yet Provisioned

A publish to a topic whose record is absent or `provisioning` is answered `503 Service Unavailable`, retryable. Provisioning starts as soon as the topic is registered, so the window is the backend's own provisioning time.

Reading such a topic is not refused: delivery reads it as an empty partition (feature 0007 §3.3), so consumers may join before the first publish.

## 4. States (CDSL)

```
              insert wins                       provision -> Ready
 absent ────────────────────▶ provisioning ────────────────────────▶ provisioned
                               │    ▲
   Pending, LogConflict, or    │    │ no instance acted for attempt_timeout:
   live attempt continued      └────┘ replaced by CAS, new log id
```

## 5. Actor Flows (CDSL)

### 5.1 A New Topic Appears

```python
# Two ingest instances. A gear registers gts.cf.core.events.topic.v1~vendor.orders.v1, 4 partitions.
# Routing: instance-less key -> main.

# ingest-1, provisioning worker
#   no row -> insert (L7, provisioning) -> won
#   main.provision(orders/4, replacing=Nothing, new=L7) -> Ready(4) -> (L7, provisioned, 4)

# ingest-2, a moment later
#   row provisioned, partitions match -> nothing to do
```

### 5.2 An Attempt Is Abandoned

```python
# ingest-1 inserts (L7, provisioning) and crashes before calling the backend.
# attempt_timeout later, ingest-2's worker:
#   row (L7, provisioning), not acted on for attempt_timeout -> CAS to (L8, provisioning) -> won
#   main.provision(orders/4, replacing=Log(L7), new=L8)
#     nothing stored (ingest-1 never got there) -> accepted
#     had ingest-1 stored L7 first            -> stored == replacing -> accepted, replaced by L8
#   -> Ready(4) -> (L8, provisioned, 4)
```

### 5.3 The Backend Holds Another Log Id

```python
# main already holds orders under L3 (a restored copy).
# ingest-1: insert (L7, provisioning) -> provision(replacing=Nothing, new=L7) -> LogConflict
#   attempt_at refreshed, error logged; retried on backoff with L7, again LogConflict
# the record stays (L7, provisioning); publishes answer 503; ADR-0008 owns the reaction
```

## 6. Out of Scope

Everything that reads the log id back is ADR-0008's to decide:

- checking the log id - at start, per operation, or otherwise - and what a failed check does;
- moving a topic between backends, and the contract for the tool that moves it;
- losing the ingest database;
- reprovisioning a topic after data loss;
- whether delivery checks the log id.

Changing a provisioned topic's partition count is out of scope as well.

## 7. Definitions of Done

- `backend::Backend` carries `provision` (§2.3); every linked backend plugin implements it, compare-and-set included, and reports the partition count it holds.
- `event_broker__topic_binding` exists in the ingest database with portable types, written by ingest only.
- Provisioning attempts (§3.1) run on their own worker, with a per-call timeout and backoff, for every topic not `provisioned`; `attempt_at` uses the database's clock.
- `provisioning.attempt_timeout` is a setting.

## 8. Acceptance Criteria

- **AC-1**: A new topic seen by two ingest instances is provisioned exactly once, under one log id, with the partition count the backend reports.
- **AC-2**: An attempt abandoned by a crashed instance is replaced after `attempt_timeout` by another, under a new log id, and completes.
- **AC-3**: A backend answering `Pending` is called again with the same log id until it answers `Ready`.
- **AC-4**: A publish to a topic not yet provisioned is answered `503`, retryable, and succeeds once the topic is provisioned.
- **AC-5**: A backend holding a log id no attempt expects refuses the attempt; the topic stays `provisioning` under the same log id, however long it lasts.
- **AC-6**: A backend that never answers delays only the topics routed to it; the specification refresh and other topics' provisioning proceed.

## 9. Unit Test Plan

- **attempt**: one test per branch of §3.1 - no row, insert lost, provisioned, provisioned with a differing partition count, live attempt continued, abandoned attempt replaced, replacement CAS lost.
- **backend CAS** (per plugin): nothing stored; stored equals the id `replacing` names; stored equals `new`; any other stored id is `LogConflict`.
- **pending / conflict**: `Pending` and `LogConflict` leave the record `provisioning`, refresh `attempt_at`, and the next call uses the same id.
- **clock**: `attempt_at` is set and compared by the database, not the host.

## 10. E2E Test Plan

Suite: `testing/e2e/suites/event_broker/`.

- **S1 - provision once**: two ingest instances; register a topic; assert one record, `provisioned`, one log id, the partition count, and that both instances accept publishes.
- **S2 - restart**: restart both instances; assert the record is unchanged and nothing is provisioned again.
- **S3 - before provisioning**: publish to a topic registered a moment ago; assert `503` until it is provisioned, then success.
- **S4 - conflict**: pre-seed the backend with another log id for a topic; register it; assert the record stays `provisioning` under one log id and publishes answer `503`.

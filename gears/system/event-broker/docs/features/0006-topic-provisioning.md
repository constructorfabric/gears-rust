# Feature: Topic Provisioning

- [ ] `p1` - **ID**: `cpt-cf-evbk-featstatus-topic-provisioning`

<!-- toc -->

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
- [3. Actor Flows (CDSL)](#3-actor-flows-cdsl)
  - [3.1 A New Topic Appears](#31-a-new-topic-appears)
  - [3.2 An Attempt Is Abandoned](#32-an-attempt-is-abandoned)
  - [3.3 The Backend Holds Another Log Id](#33-the-backend-holds-another-log-id)
- [4. Processes / Business Logic (CDSL)](#4-processes--business-logic-cdsl)
  - [4.1 Provisioning Attempt](#41-provisioning-attempt)
  - [4.2 Publishing to a Topic Not Yet Provisioned](#42-publishing-to-a-topic-not-yet-provisioned)
- [5. States (CDSL)](#5-states-cdsl)
- [6. Out of Scope](#6-out-of-scope)
- [7. Definitions of Done](#7-definitions-of-done)
  - [Topic Provisioning](#topic-provisioning)
- [8. Acceptance Criteria](#8-acceptance-criteria)
- [9. Unit Test Plan](#9-unit-test-plan)
- [10. E2E Test Plan](#10-e2e-test-plan)

<!-- /toc -->

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

The specification manager, on ingest instances, is the only writer. The table runs on SQLite, PostgreSQL and MySQL/MariaDB, with each engine's UUID type: `uuid` on PostgreSQL, `binary(16)` on MySQL/MariaDB and `uuid_text` on SQLite; `topic_id` is ASCII with a binary collation on MySQL/MariaDB.

```sql
CREATE TABLE event_broker__topic_binding (
    topic_uuid  UUID          PRIMARY KEY,   -- GtsId::to_uuid(topic_id), deterministic
    topic_id    VARCHAR(1024) NOT NULL,      -- GTS topic instance identifier; for operators, not indexed
    log_id      UUID          NOT NULL,      -- UUID v4: the current attempt's, then the provisioned log's
    state       VARCHAR(12)   NOT NULL,      -- 'provisioning' | 'provisioned'
    attempt_at  VARCHAR(24)   NOT NULL,      -- YYYY-MM-DDTHH:MM:SS.sssZ, database time: when the current attempt last acted
    partitions  BIGINT,                      -- the count the backend holds; set when provisioned
    retention_max_age_ms BIGINT,             -- the retention max age last recorded, ms; NULL = unbounded
    retention_max_bytes  BIGINT,             -- the retention max bytes last recorded; NULL = unbounded
    retention_version    BIGINT  NOT NULL,   -- bumped by every write of the retention columns or of retention_confirmed
    retention_confirmed  BOOLEAN NOT NULL    -- false while the backend may hold a retention other than the recorded one
);
```

- The primary key is the topic's deterministic UUID, so the 1024-character identifier never needs an index.
- The record says which log a topic has, not where it is: every instance resolves the backend from its own routing, and every instance carries the same routing (feature 0005 §2).
- `state` separates "not created yet" from "created". A `provisioned` topic is provisioned again only when its resolved settings change (its retention) or its record is unconfirmed: the same log id, so the backend adopts the new settings without a new attempt (feature 0007 §2.6).
- `partitions` is the count every role uses for the topic once it is provisioned (feature 0007 §2.6).
- `retention_max_age_ms` and `retention_max_bytes` hold the retention last recorded; the backend holds it while `retention_confirmed` is true. They are set when the topic is provisioned and again on each re-provision that answers `Ready` and wins the compare-and-set on `retention_version`. A NULL bound is unbounded, and a bound past what `BIGINT` holds is stored as its maximum. The resolved retention is compared with them in these units to decide whether a `provisioned` topic is provisioned again.
- `retention_version` is bumped by every write of the retention columns or of `retention_confirmed`: the write that makes the topic `provisioned`, the mark before a re-provision call, the retention write after it, and every unconfirmed mark. The mark before the call compares-and-sets on the version its pass read, and the re-provision records its retention only while the version is the one that mark wrote. A counter never repeats, so a write that read before another one always loses, even when the retention went back to the value it read.
- `retention_confirmed` is false while the backend may hold a retention other than the recorded one: from the mark before a re-provision call until that call's answer wins the compare-and-set, so also after a call dropped mid-flight, a call whose answer was not seen and a record write that lost its compare-and-set; and after a first provisioning whose call answered `Ready` but whose write to `provisioned` lost to another instance driving the same attempt, or whose call answered `Pending`, timed out or failed after that instance recorded the topic. An unconfirmed `provisioned` topic is provisioned again on the next pass even when its resolved retention equals the recorded one. A new attempt inserts version `0` and unconfirmed, and the write that makes the topic `provisioned` confirms it.
- `attempt_at` is written and compared with the database's clock in the same statement, so clock skew between ingest hosts never decides whether an attempt is abandoned. It is text of one fixed width on every engine, so it compares as text.

### 2.3 Backend Contract

One operation on the backend trait (feature 0007 §2.6):

```rust
/// Creates whatever the backend needs to hold `topic`'s log, and stores `claim.new` as its log id.
///
/// Compare-and-set: accepted when the stored id is already `claim.new` (the same attempt,
/// continued), when it is the one `claim.replacing` names (the attempt being replaced), or when
/// nothing is stored and the claim is not `Held`. A `Held` claim over nothing is
/// `BackendError::NotProvisioned`, and the backend creates nothing; any other stored id is
/// `BackendError::LogConflict`.
async fn provision(&self, topic: &TopicSpec, claim: &LogClaim) -> Result<Provisioning, BackendError>;

pub struct LogClaim { pub replacing: Replacing, pub new: LogId }
pub enum Replacing {
    /// No earlier attempt is being replaced.
    Nothing,
    /// The attempt being replaced.
    Log(LogId),
    /// The log is already held under `new`: apply the settings only, never create.
    Held,
}

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

## 3. Actor Flows (CDSL)

### 3.1 A New Topic Appears

```python
# Two ingest instances. A gear registers gts.cf.core.events.topic.v1~vendor.orders.v1, 4 partitions.
# Routing: instance-less key -> main.

# ingest-1, specification refresh
#   no row -> insert (L7, provisioning) -> won
#   main.provision(orders/4, replacing=Nothing, new=L7) -> Ready(4) -> (L7, provisioned, 4)

# ingest-2, a moment later
#   row provisioned, partitions match, retention unchanged -> nothing to do
```

### 3.2 An Attempt Is Abandoned

```python
# ingest-1 inserts (L7, provisioning) and crashes before calling the backend.
# attempt_timeout later, ingest-2's specification refresh:
#   row (L7, provisioning), not acted on for attempt_timeout -> CAS to (L8, provisioning) -> won
#   main.provision(orders/4, replacing=Log(L7), new=L8)
#     nothing stored (ingest-1 never got there) -> accepted
#     had ingest-1 stored L7 first            -> stored == replacing -> accepted, replaced by L8
#   -> Ready(4) -> (L8, provisioned, 4)
```

### 3.3 The Backend Holds Another Log Id

```python
# main already holds orders under L3 (a restored copy).
# ingest-1: insert (L7, provisioning) -> provision(replacing=Nothing, new=L7) -> LogConflict
#   attempt_at refreshed, error logged; retried on backoff with L7, again LogConflict
# the record stays (L7, provisioning); publishes answer 503; ADR-0008 owns the reaction
```

## 4. Processes / Business Logic (CDSL)

Pseudo-code below is Python-ish - not runnable; intent over syntax.

### 4.1 Provisioning Attempt

The specification manager owns each topic's lifecycle: it is the only writer of the binding record and it drives the attempts. Every specification refresh pass on an ingest instance ends with a provisioning pass over every registered topic - at start, before the instance serves, and after each load, so a topic the refresh registers is provisioned in the same pass. While a pass leaves any topic unprovisioned - its backend still starting, or answering `Pending` or `LogConflict` - provisioning alone is retried on a short backoff. At start, the instance waits up to 10 s for its topics to be provisioned before it takes traffic, retrying after 250 ms, doubling up to 1 s, while another retry still fits in the 10 s; once it serves, it retries after 1 s, doubling up to the refresh interval, and starts again at 1 s after each refresh: a backend that is still starting is waited for, one that is down delays the start by that much and never fails it. A provisioned topic is provisioned again, as a `Held` claim under the same log id and with its recorded partition count, only when its resolved retention differs from the retention its record holds, or its record is unconfirmed. Before the call the record is marked unconfirmed by a compare-and-set on the `retention_version` the pass read; when that loses, another write happened since the read, and the pass leaves the topic to the next pass. On `Ready` the retention is recorded and confirmed by a compare-and-set on the version the mark wrote; a lost compare-and-set, and a call answering `Pending`, timing out or failing, mark the record unconfirmed again, because the backend may hold or still apply another retention; a refusal (`NotProvisioned`, `LogConflict`) leaves it unconfirmed from the mark. In every case but a won compare-and-set, the next pass calls again. A first provisioning call answering `Pending`, timing out or failing marks the record unconfirmed as well, since the backend may still apply it: when another instance driving the same attempt has recorded the topic meanwhile, the mark keeps that record from holding a retention the backend may not hold; while the topic is `provisioning`, the mark matches nothing. Each `provision` call carries a timeout, so a backend that never answers costs one timeout per pass and never stalls the refresh or other topics. The compare-and-set on the record decides which instance acts.

```python
def provision_topic(topic):
    backend = registry[routing(topic)]                     # feature 0005 §2.2
    spec = TopicSpec(topic, partitions=settings(topic).partitions,
                     retention=settings(topic).retention)
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
        if row.retention_confirmed and row.retention == spec.retention:   # compared in the stored units
            return                                         # nothing to do
        if not binding.cas(topic.uuid, (row.log_id, "provisioned", row.retention_version),
                           retention_version=row.retention_version + 1,
                           retention_confirmed=False):    # marked before the call
            return                                         # written since the read: next pass
        held = TopicSpec(topic, partitions=row.partitions, retention=spec.retention)
        match backend.provision(held, LogClaim(Held, row.log_id)) within call_timeout:
            case Ready(_):
                if not binding.cas(topic.uuid, (row.log_id, "provisioned", row.retention_version + 1),
                                   retention=spec.retention,
                                   retention_version=row.retention_version + 2,
                                   retention_confirmed=True):     # a concurrent write won
                    binding.mark_unconfirmed(topic.uuid, (row.log_id, "provisioned"))
            case NotProvisioned:
                log.error("the backend this topic is routed to holds no log for it",
                          topic=topic)                     # unconfirmed from the mark: retried next pass
            case LogConflict:
                log.error("the backend this topic is routed to holds another log id",
                          topic=topic)                     # unconfirmed from the mark: retried next pass
            case _:                                        # Pending, timeout, failure
                binding.mark_unconfirmed(topic.uuid, (row.log_id, "provisioned"))
                log.warn("re-provisioning failed", topic=topic)   # the backend may still apply it
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
            if not binding.cas(topic.uuid, (new, "provisioning"),
                               (new, "provisioned", partitions, spec.retention)):   # another driver won
                binding.mark_unconfirmed(topic.uuid, (new, "provisioned"))   # no match if replaced
        case Pending:
            binding.touch(topic.uuid, new, db_now())       # same `new`, retried on backoff
            binding.mark_unconfirmed(topic.uuid, (new, "provisioned"))   # matches once another driver recorded it
        case LogConflict:
            binding.touch(topic.uuid, new, db_now())       # same `new`; never treated as abandoned
            log.error("topic provisioning refused: the backend holds another log id",
                      topic=topic, log_id=new)             # reaction: ADR-0008
        case timeout | Unavailable:
            binding.mark_unconfirmed(topic.uuid, (new, "provisioned"))   # matches once another driver recorded it
            # retried on backoff; ages toward replacement
```

- **One live attempt.** The record admits one attempt at a time: the insert for a new topic, and the compare-and-set for a replacement. Any instance may continue a live attempt, because continuing it sends the same log id.
- **At most one backend holds the recorded log id.** The backend accepts a new id only over nothing, over the attempt it replaces, or over itself. An abandoned attempt that reached another backend - under routing that changed in between - leaves an id there that no record names; that backend stays writable to anyone still routed to it until ADR-0008 O1 decides how operations are checked.
- **A re-provision never creates.** It is a `Held` claim: a backend holding the log applies the new retention, one holding nothing for the topic - a routing change moved it there - answers `NotProvisioned` and creates nothing, and one holding another id answers `LogConflict`. So a routing change and a retention change together never plant the log on a second backend; the record keeps its retention, and every pass calls again while the retention differs.
- **Pending is not a new attempt.** The backend is called again with the same id until it answers `Ready`.
- **A refused attempt is not abandoned.** `LogConflict` refreshes `attempt_at` and keeps the same id, so the record does not churn through new log ids; the topic stays `provisioning` and its publishes keep answering `503` until an operator acts (ADR-0008 O2).
- **Only silence ages an attempt.** An attempt is replaced only when no instance has acted on it for `attempt_timeout` - a crash, or a backend that never answers.
- **A rollout converges.** Instances that resolve different retention during a rollout interleave their calls and record writes. The record of a `provisioned` topic is unconfirmed from the mark before each re-provision call until that call's answer wins its compare-and-set, so a re-provision that interleaves with another, is answered late, is not answered, or is dropped mid-flight (a shutdown, the start-up wait cut, a crash) leaves it unconfirmed; so does a first provisioning whose write to `provisioned` loses to another instance driving the same attempt, or whose call answers `Pending`, times out or fails after that instance records the topic. An unconfirmed record is provisioned again on the next pass, so once every instance resolves the same retention, the backend and the record hold it. Not covered: a call whose answer is never seen and that the backend applies after every later write. The record cannot fence a write that lands after all of them; the mark keeps that window to the call's own lifetime. One such call is a first-provisioning call dropped mid-flight (a shutdown, the start-up wait cut, a crash) after another instance records the topic: the record is still `provisioning` when the call starts, so there is nothing to mark before it, and if the backend applies the call after that record write, the record stays confirmed at the other instance's retention. So does a first-provisioning call that answers `Pending`, times out or fails before the other instance records the topic, when the backend applies it after that record write: its mark comes while the record is still `provisioning` and matches nothing.
- **A mark outlives every earlier read.** `mark_unconfirmed` (`retention_confirmed = false`, `retention_version + 1`) matches the record whatever its version, and the bump makes any compare-and-set read before it lose, so a write that read before the mark cannot clear the doubt the mark records.

### 4.2 Publishing to a Topic Not Yet Provisioned

A publish to a topic whose record is absent or `provisioning` is answered `503 Service Unavailable`, retryable. Provisioning starts as soon as the topic is registered, so the window is the backend's own provisioning time.

Reading such a topic is not refused: delivery reads it as an empty partition (feature 0007 §4.3), so consumers may join before the first publish.

## 5. States (CDSL)

```
              insert wins                       provision -> Ready
 absent ────────────────────▶ provisioning ────────────────────────▶ provisioned
                               │    ▲
   Pending, LogConflict, or    │    │ no instance acted for attempt_timeout:
   live attempt continued      └────┘ replaced by CAS, new log id
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

### Topic Provisioning

- [ ] `p1` - **ID**: `cpt-cf-evbk-dod-topic-provisioning`

- `backend::Backend` carries `provision` (§2.3); every linked backend plugin implements it, compare-and-set included, and reports the partition count it holds.
- `event_broker__topic_binding` exists in the ingest database with the columns of §2.2, written by the specification manager only.
- The specification manager runs provisioning attempts (§4.1) at the end of every specification refresh pass, with a per-call timeout, for every registered topic; `attempt_at` uses the database's clock.
- `provisioning.attempt_timeout` is a setting.

## 8. Acceptance Criteria

- **AC-1**: A new topic seen by two ingest instances is provisioned exactly once, under one log id, with the partition count the backend reports.
- **AC-2**: An attempt abandoned by a crashed instance is replaced after `attempt_timeout` by another, under a new log id, and completes.
- **AC-3**: A backend answering `Pending` is called again with the same log id until it answers `Ready`.
- **AC-4**: A publish to a topic not yet provisioned is answered `503`, retryable, and succeeds once the topic is provisioned.
- **AC-5**: A backend holding a log id no attempt expects refuses the attempt; the topic stays `provisioning` under the same log id, however long it lasts.
- **AC-6**: A backend that never answers delays only the topics routed to it; the specification refresh and other topics' provisioning proceed.

## 9. Unit Test Plan

- **attempt**: one test per branch of §4.1 - no row, insert lost, provisioned with unchanged retention (no call), provisioned with changed retention (one call under the same log id, recorded on `Ready`, retried after a failure), re-provision compare-and-set lost to another instance's record write (marked unconfirmed; one more call once the configurations agree, including when the losing call was applied last), a mark after a lost record that a re-provision read before it cannot clear, re-provision answer not seen (marked unconfirmed; called again even when the configuration returns to the recorded retention), re-provision dropped mid-call (unconfirmed from the mark before the call; called again once the configuration returns), mark before the call lost (the pass skips the topic; not covered: needs a pause seam between the binding read and the mark), first provisioning whose write to `provisioned` lost to another driver of the same attempt (marked unconfirmed; one more call once the configurations agree), first provisioning call answered `Pending` or failing after another driver of the same attempt recorded the topic (marked unconfirmed; one more call once the configurations agree), provisioned with a differing partition count (logged on every pass), live attempt continued, abandoned attempt replaced, replacement CAS lost.
- **backend CAS** (per plugin): nothing stored; stored equals the id `replacing` names; stored equals `new`; any other stored id is `LogConflict`.
- **pending / conflict**: `Pending` and `LogConflict` leave the record `provisioning`, refresh `attempt_at`, and the next call uses the same id.
- **clock**: `attempt_at` is set and compared by the database, not the host.

## 10. E2E Test Plan

Suite: `testing/e2e/suites/event_broker/`.

- **S1 - provision once**: two ingest instances; register a topic; assert one record, `provisioned`, one log id, the partition count, and that both instances accept publishes.
- **S2 - restart**: restart both instances; assert the record is unchanged and nothing is provisioned again.
- **S3 - before provisioning**: publish to a topic registered a moment ago; assert `503` until it is provisioned, then success.
- **S4 - conflict**: pre-seed the backend with another log id for a topic; register it; assert the record stays `provisioning` under one log id and publishes answer `503`.

# BSS approval units

`cf-gears-bss-approval` (library `bss_approval`) shares approval models, rules,
content hashing, a DDL template and the engine across BSS gears.

## What a unit is

An approval unit groups proposed business changes into one review and decision.
It records its tenant, kind, reference, items, reviewer snapshot, business-content
fingerprint and the quorum copied from policy at submission.
It starts pending, and ends approved, rejected or withdrawn; quorum zero applies
at submission and still records an approved unit without votes.
The submitter and every item's author are excluded from approving, and each actor
may vote only once per generation.
Content drift refreshes the items, snapshot and fingerprint, increments the
generation and marks earlier decisions stale so reviewers must vote again; an
approve and a reject both refresh a stale unit instead of voting.
An optimistic unit version detects competing writes without row locks, while
approve and reject require the generation the reviewer actually saw.

## What a gear implements

Implement `Store<R>` for the gear's four approval tables and `ApprovalSubject<R>`
for each kind of business change, using `#[async_trait::async_trait]` on both
implementations. The real transaction runner instantiations are
`impl<'a> Store<DbTx<'a>> for MyStore` and
`impl<'a> ApprovalSubject<DbTx<'a>> for MySubject`, importing
`toolkit_db::secure::DbTx`.

Both traits require `R: toolkit_db::secure::DBRunner + Sync` and implementors
that are `Send + Sync`. `DbTx<'a>` satisfies that bound; the toolkit's sealed
runner already requires `Send + Sync`. Use the `&DbTx<'a>` provided by the
transaction closure directly; it cannot be constructed outside toolkit-db.

`Store` supplies `insert_unit`, `unit`, `bump_version`, `items`, `decisions`,
`insert_decision`, `refresh` and `set_state`. Every operation uses the supplied
runner and the gear's tenant-scoped SecureORM entities. Items and decisions have
`tenant_id` columns; the store supplies the decision's tenant from its scope
because `Decision` has no tenant field. `bump_version` conditionally increments
`version` using the expected value and returns `false` if no row matched.
`refresh` replaces items and snapshot, updates the hash and generation, and
marks earlier decisions stale within the same transaction. Preserve driver
errors as `ApprovalError::Db` so `db_err()` can support retry classification.

`ApprovalSubject` supplies `kind`, `ref_type`, `collect`, `validate_submit`,
`lock`, `snapshot`, `apply` and `unlock`. Collection must return the requested
business items; validation rejects invalid submissions. Acquire item locks
conditionally: zero affected rows means `ApprovalError::Locked`. Application
revalidates the environment and returns `ApplyRefused { code, detail }` when
publication is no longer allowed. Unlocking clears pending locks and, on
approval, records `approved_by_unit_id`. The gear owns authorization, policy
loading, audit, outbox events and idempotency; those effects belong in the same
transaction as the approval transition.

## What the gear's door does

Check the gear's replay/idempotency contract before fences or unit work. Open
`Db::transaction_with_retry` with `TxConfig` and the gear's typed database-error
extractor. Its callback has this exact shape:

```text
for<'a> FnMut(&'a DbTx<'a>)
    -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>> + Send
```

Build the scoped store and subject for each attempt, then call `Engine::submit`,
`Engine::approve`, `Engine::reject` or `Engine::withdraw` with that runner. The
four take an `InTransaction` runner (`DbTx` or `SecureTx`): their writes are one
unit only inside a transaction, so a connection does not compile.
`submit` takes `SubmitRequest` and returns `Submitted { unit, applied }`.
`approve` returns `ApproveOutcome::{Pending { have, need }, Applied,
Refreshed { generation }}`, and `reject` returns `RejectOutcome::{Rejected,
Refreshed { generation }}`. Reject requires a nonblank note; a vote's note is at
most `NOTE_MAX_CHARS` (2000) characters (`NoteTooLong`, before any write);
withdrawal is restricted to the submitter. A unit the store does not hold is
`UnitNotFound`. Pass the client's `seen_generation` to approve and reject; do not
replace it with a newly loaded generation.

Propagate every `Err` out of the callback so the transaction rolls back, including
any preceding version bump, vote or lock. **Return `Ok(Refreshed { generation })`
(either outcome's) from the callback and map it to wire `UNIT_STALE` only after
`transaction_with_retry` has committed.** Mapping refresh to an error inside the
callback would undo the new generation and its stale-vote marks.

Map every `ApprovalError` through `code()` in the gear's error adapter, preserving
its details. In particular, `Contended` becomes HTTP 409 `UNIT_CONTENDED` and the
client retries; `GenerationMismatch` becomes HTTP 400 `GENERATION_MISMATCH` with
the current generation. The engine does not open connections, emit HTTP responses
or retry domain errors itself. The fake spike's `no_retry` extractor is specific
to that test and is not the production retry policy.

## Migrations

Call the DDL helpers from the gear's own migration and `SchemaManager`:

```rust
async fn approval_tables(
    manager: &sea_orm_migration::SchemaManager<'_>,
) -> Result<(), sea_orm::DbErr> {
    bss_approval::ddl::apply_up(manager, "products_", Some("bss")).await
}
```

This creates `products_approval_policy`, `products_approval_unit`,
`products_approval_unit_item` and `products_approval_decision`, plus the queue
index. The decision primary key is `(unit_id, actor, generation)`.
`apply_up` is idempotent; `apply_down(manager, "products_", Some("bss"))` drops
child tables before parents. PostgreSQL qualifies the tables with the existing
schema; SQLite ignores it in both helpers. Prefix and schema are trusted SQL
identifiers fixed by migration code, never request input. The library registers
no migration or connection of its own, and unit rows carry no idempotency key.

## Business content only

`ItemRef.after` contains only the proposed business content. Exclude
`pending_unit_id`, `approved_by_unit_id`, row versions and other lock or storage
metadata: otherwise acquiring the submit lock would immediately make its own
unit stale. `hash::snapshot_hash` includes item type/id, canonical `after` JSON
and the common effective date; item order and JSON object-key order do not
matter. Authorship, `before` and the reviewer-facing snapshot are not fingerprint
inputs. Keep informational impact or computation timestamps in the snapshot,
not in `after`.

The scenarios in `tests/engine_fake.rs` drive the engine through real
SQLite toolkit-db transactions with fakes typed over `DbTx<'a>`. They prove the
runner/lifetime integration and state-machine decisions. The maps do not roll
back, and simulated contention is not a two-writer test: phase 1c must prove
rollback and real contention with its SQL-backed store on SQLite and PostgreSQL.

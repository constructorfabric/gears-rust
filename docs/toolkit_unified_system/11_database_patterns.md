# Database Execution Patterns

This document covers database execution mechanics in Gears: the `DBRunner` abstraction, transactions, the repository pattern, and database migrations.

For security-scoped database access (`AccessScope`, `.secure().scope_with(..)`, `PolicyEnforcer` PEP pattern), see [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md). **Design references:** [secure-orm ADR-0001 CTE policy](../arch/secure-orm/ADR/0001-secure-cte-policy.md) · [secure-orm ADR-0002 property-graph policy](../arch/secure-orm/ADR/0002-secure-property-graph-policy.md) · [database ADR-0001 object namespacing](../arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md) · [database TRADEOFFS](../arch/database/TRADEOFFS.md) · [Architecture Manifest § 3.2.4](../ARCHITECTURE_MANIFEST.md#324-gear-owned-schema-runtime-owned-privilege)

## Core invariants

- **Rule**: No plain SQL in handlers/services/repos. Raw SQL is allowed only in migration infrastructure.
- **Rule**: Repository methods accept `runner: &impl DBRunner`, so they run on a connection or inside a transaction.
- **Rule**: Run transactional work through `DBProvider::transaction` (or `transaction_with_config`); use `Db::transaction_with_retry` when the transaction can hit serialization or lock contention.
- **Rule**: Each gear gets its own isolated migration history table.
- **Rule**: CTEs go through `with_ctes()` / `cte()` / `recursive_cte()`. A raw `WITH`
  string — recursive or not — is forbidden in gear code, as is reaching a raw-SQL sink
  via `into_inner()` / `into_query()`.
- **Rule**: Recursive traversal must state a depth bound. There is no safe default.

## Common Table Expressions (`WITH`)

`with_ctes()` is reachable only from a scoped select, and every CTE body inherits that query's
`AccessScope`, so a CTE cannot leak another tenant's rows and there is no scope check to write.
Why scope is embedded in each body: [ADR-0001 § Decision Outcome](../arch/secure-orm/ADR/0001-secure-cte-policy.md#decision-outcome).

```rust
let rows = order::Entity::find()
    .secure()
    .scope_with(&scope)          // required before with_ctes()
    .with_ctes()
    .cte::<line_item::Entity>("scoped_items", |q| {
        q.filter(line_item::Column::Quantity.gt(0))
    })
    .join_cte("scoped_items", on_condition)   // define != use
    .all(runner)
    .await?;
```

Rules (API contract and reasons: [ADR-0001 § API shape](../arch/secure-orm/ADR/0001-secure-cte-policy.md#api-shape)):

- **Reference every CTE.** `cte()` only defines; without `join_cte()` the `WITH` computes nothing.
- **Get the join predicate right.** It is not compiler-verified; a wrong one returns wrong rows but
  cannot cross a tenant boundary.
- **`join_cte` is an inner join.** Add `.distinct()` when the CTE is a membership set, after
  narrowing the projection with `.select_only()` (PostgreSQL `json` has no equality operator).
- **Order by `.order_by(E::Column, ..)` for entity columns and `.order_by_cte(..)` for CTE columns.**
  `.distinct()` with `.order_by_cte()` returns `ScopeError::Invalid` on every backend. "Distinct rows,
  shallowest first" (`GROUP BY … ORDER BY MIN(depth)`) is not expressible yet; dedup in the caller.
- **Narrow the outer projection** with `.select_only()` plus `.column()` / `.column_from_cte()` /
  `.expr_as()`, paired with `all_as::<T>()`. `all_as::<T>()` alone narrows only deserialization;
  on PostgreSQL the wide select turns an index-only scan into a heap visit per row (measured 0.371 ms
  against 0.079 ms).
- **Don't `OR` two columns in a `join_cte` predicate.** `node.id = cte.src OR node.id = cte.dst`
  drops the index (15.2 ms against 0.30 ms on 199k nodes). Put the alternation inside the CTE body
  and project one column holding both endpoints.
- **Give every recursive walk a depth cap** (`RecursiveCte::new`); it is the only termination
  guarantee. Keep the default `RecursiveDedup::Union` unless the graph is a sparse parent-pointer
  tree. A recursive walk spans one self-referencing table; a hop through a separate table needs one
  scoped query per hop. See [ADR-0001 § Recursive CTE](../arch/secure-orm/ADR/0001-secure-cte-policy.md#recursive-cte-with-recursive).
- **Prefer a closure table for a hot hierarchy** (as `tenant_closure` does); use `recursive_cte` for
  rarely walked or frequently changing trees. See
  [ADR-0001 § Closure tables](../arch/secure-orm/ADR/0001-secure-cte-policy.md#closure-tables-still-preferred-for-hot-hierarchies-no-longer-required).

## Executors: `DBRunner`

A gear obtains a `DBProvider<DbError>` from `GearCtx::db()` / `db_required()` in `init()` (`toolkit_db::DBProvider`, [`db_provider.rs`](../../libs/toolkit-db/src/db_provider.rs)). It hands out two runners, both implementing the sealed `toolkit_db::secure::DBRunner` trait:

- `DBProvider::conn()` returns a `DbConn` for non-transactional work.
- Inside `DBProvider::transaction(..)` the closure receives a `&DbTx`.

Repository methods accept **`runner: &impl DBRunner`** (or a generic `C: DBRunner`), so the same method works on a connection and inside a transaction. `DbConn` refuses to be created while a transaction is open on the same task (the transaction-bypass guard in `Db::conn`), so use the `tx` runner inside the closure.

Example signature:

```rust
use toolkit_db::secure::{DBRunner, ScopeError};
use toolkit_security::AccessScope;

pub async fn create_user(
    runner: &impl DBRunner,
    scope: &AccessScope,
    user: user::ActiveModel,
) -> Result<user::Model, ScopeError> {
    // ...
}
```

## Transactions

### `DBProvider::transaction`

The closure receives `&DbTx` and returns a boxed future. The transaction commits when the closure returns `Ok` and rolls back on `Err`. The error type is the provider's `E` (which must implement `From<DbError>`). From the `users-info` smoke test ([`tests_tenant_scoping.rs`](../../examples/toolkit/users-info/users-info/src/domain/service/tests_tenant_scoping.rs), `dbprovider_transaction_smoke`):

```rust
let provider: DBProvider<toolkit_db::DbError> = DBProvider::new(db.clone());
let scope = AccessScope::for_tenants(vec![tenant_id]);

provider
    .transaction(|tx| {
        Box::pin(async move {
            let user = ActiveModel { /* ... */ };
            let _ = secure_insert::<UserEntity>(user, &scope, tx).await?;
            Ok(())
        })
    })
    .await?;
```

`DBProvider::transaction_with_config(TxConfig, ..)` takes an isolation level and access mode, for example `TxConfig::serializable()` for hierarchy mutations (`chat-engine`'s `session_repo.rs` and `variant_repo.rs`) or `TxConfig::read_only()` for pure reads (`prefetch` in `gears/bss/orders/orders/src/infra/storage/scoped.rs`, via `Db::transaction_ref_mapped_with_config`).

### Retrying on contention: `Db::transaction_with_retry`

Under `SERIALIZABLE`, or on any backend with lock contention, a transaction can fail with an error that succeeds on retry. `Db::transaction_with_retry(tx_config, extract_db_err, body)` ([`secure/db.rs`](../../libs/toolkit-db/src/secure/db.rs)) reruns `body` in a fresh transaction when `extract_db_err` returns a `DbErr` that `toolkit_db::contention::is_retryable_contention` classifies as retryable, up to `DEFAULT_TX_RETRY_ATTEMPTS`. The body is called once per attempt, so it must be idempotent and clone what it captures ([R1](14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again), [R2](14_db_behavior_testing.md#r2-the-retry-can-fire)); retry versus reporting the abort is a choice, see [TRADEOFFS § Retrying aborted transactions](../arch/database/TRADEOFFS.md#retrying-aborted-transactions). Get the `Db` from the provider with `DBProvider::db()`. Abridged from `file-storage` ([`store/metadata.rs`](../../gears/file-storage/file-storage/src/infra/storage/store/metadata.rs)):

```rust
self.db
    .db()
    .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
        let files = files.clone();
        let patch_scope = patch_scope.clone();
        Box::pin(async move {
            let bumped = files
                .touch_meta(tx, &patch_scope, file_id, expected_meta_version, now)
                .await?;
            if !bumped {
                return Ok(false);
            }
            // ... upsert / delete the patched metadata keys, insert the audit record
            Ok::<bool, DomainError>(true)
        })
    })
    .await
```

`db_err` here is `fn(&DomainError) -> Option<&sea_orm::DbErr>`; returning `None` stops retrying. `transaction_with_retry_max` takes an explicit attempt budget.

### SQLite: `transaction_mode`

`SQLite` begins a plain `BEGIN` as a *deferred* transaction (dialect background: [TRADEOFFS § Row locks and dialects](../arch/database/TRADEOFFS.md#row-locks-and-dialects)). One that reads and then writes cannot wait for the write lock: while another connection holds it, `SQLite` fails the write at once with `SQLITE_BUSY` and ignores `busy_timeout`. Under write contention (a request beside the gear's outbox worker, two requests on one row) such a transaction fails on every retry while the other writer is busy, and the caller sees a conflict that never happened.

A gear whose writes contend can set `transaction_mode: "immediate"` in its database `params`. (A DSN passed to `connect_db` can carry `transaction_mode=immediate` too, but a gear config's `dsn` cannot: the config path ignores a SQLite DSN's query parameters.) `Db`'s transactions that may write then begin with `BEGIN IMMEDIATE`, which takes the write lock up front and waits for it under `busy_timeout`, so writers queue instead of failing. So do the outbox's queue registration, sequencer, vacuum and processing transactions on that database; its dead-letter replay and discard, and the migration runner, still begin deferred. The default is `deferred`.

An immediate transaction holds the write lock from `BEGIN`, so declare pure reads read-only (`TxConfig` access mode `ReadOnly`): they keep the deferred `BEGIN` and never queue behind writers. A read in a transaction with no access mode queues for the write lock like a writer.

```yaml
gears:
  bss-pricing:
    database:
      server: "sqlite_users"
      file: "bss_pricing.db"
      params:
        transaction_mode: "immediate"
```

With `immediate`, only one transaction that may write is open at a time. Don't hold one open while waiting for another connection's transaction (for example, two writers meeting at a barrier inside their transactions): the second one waits in its `BEGIN` until `busy_timeout` expires. The same goes for slow work inside one: a transactional outbox handler holds the write lock for the whole handler, so use a leased handler for slow work. sqlx refuses the option, so a raw `sea_orm::Database::connect` must get the DSN without it.

In tests, close a pool before reopening its file (`Db::close`, behind toolkit-db's `test-support` feature). Dropping a pool closes its connections in the background, and on Windows a closing connection still locks the file.

## Repository pattern

### Repository with `DBRunner` (works with both `DbConn` and `DbTx`)

```rust
use sea_orm::{EntityTrait, Set};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt};
use toolkit_security::AccessScope;

pub struct UserRepository;

impl UserRepository {
    pub async fn find_by_id(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<user::Model>, ScopeError> {
        Ok(user::Entity::find_by_id(id)
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?)
    }

    pub async fn create(
        &self,
        runner: &impl DBRunner,
        scope: &AccessScope,
        new_user: users_info_sdk::NewUser,
    ) -> Result<user::Model, ScopeError> {
        let am = user::ActiveModel {
            id: Set(new_user.id.unwrap_or_else(Uuid::new_v4)),
            tenant_id: Set(new_user.tenant_id),
            email: Set(new_user.email),
            display_name: Set(new_user.display_name),
            ..Default::default()
        };

        toolkit_db::secure::secure_insert::<user::Entity>(am, scope, runner).await
    }
}
```

## Database migrations

Gears provide migration definitions that the runtime executes with a privileged connection:

```rust
impl DatabaseCapability for MyGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}
```

Each gear gets its own migration history table (`toolkit_migrations__<prefix>__<hash8>`), ensuring isolation between gears.

### Migrations use raw SQL

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Users::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Users::TenantId).uuid().not_null())
                    .col(ColumnDef::new(Users::Email).string().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    TenantId,
    Email,
}
```

Raw SQL is **allowed only in migration infrastructure** (migration runner + migration definitions). Gear code (handlers/services/repos) must use the Secure ORM.

## Quick checklist

- [ ] Use `runner: &impl DBRunner` in repository method signatures.
- [ ] Use `DBProvider::transaction` for multi-step mutations, and `Db::transaction_with_retry` where contention is expected.
- [ ] Use raw SQL only in migration infrastructure (migration runner + migration
      definitions) — including raw `WITH`.
- [ ] Build CTEs with `with_ctes()`/`cte()`/`recursive_cte()`, and reference them with
      `join_cte()`.
- [ ] Give every `recursive_cte` an explicit `max_depth`.
- [ ] Add indexes on security columns (`tenant_id`, `resource_id`).
- [ ] Provide `DatabaseCapability::migrations()` returning SeaORM migrations.

## Related docs

- Security data path (AuthN/AuthZ, Secure ORM, AccessScope): [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md)
- OData pagination / filtering: [`07_odata_pagination_select_filter.md`](./07_odata_pagination_select_filter.md)
- Canonical example: `examples/toolkit/users-info/`
- DB-behavior testing & audit (rules and audit method): [`14_db_behavior_testing.md`](./14_db_behavior_testing.md)

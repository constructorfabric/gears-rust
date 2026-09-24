# Database Execution Patterns

This document covers database execution mechanics in Gears: the `DBRunner` abstraction, transactions, the repository pattern, and database migrations.

For security-scoped database access (`SecureConn`, `AccessScope`, `PolicyEnforcer` PEP pattern), see [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md).

## Core invariants

- **Rule**: No plain SQL in handlers/services/repos. Raw SQL is allowed only in migration infrastructure.
- **Rule**: Repository methods accept `runner: &impl DBRunner`, not `&SecureConn`.
- **Rule**: Use `in_transaction_mapped` for transactional work.
- **Rule**: Each gear gets its own isolated migration history table.
- **Rule**: CTEs go through `with_ctes()` / `cte()` / `recursive_cte()`. A raw `WITH`
  string — recursive or not — is forbidden in gear code, as is reaching a raw-SQL sink
  via `into_inner()` / `into_query()`.
- **Rule**: Recursive traversal must state a depth bound. There is no safe default.

## Common Table Expressions (`WITH`)

The full rationale is [ADR-0001](../arch/secure-orm/ADR/0001-secure-cte-policy.md); the
rules that matter day to day:

A CTE body is an independent `SELECT`, so the outer query's scope `WHERE` does **not**
reach inside it. The Secure ORM therefore embeds scope into *every* CTE body rather than
relying on the outer query. `with_ctes()` is only reachable from a scoped select, and each
body inherits that query's `AccessScope` — a differently-scoped CTE cannot be built, so
there is no check to remember and no error to handle.

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

Things to keep in mind:

- **Attaching a CTE is not using it.** `cte()` only defines; without `join_cte()` the
  `WITH` clause is valid SQL that computes nothing.
- **The join predicate is yours to get right.** It is not compiler-verified. Getting it
  wrong changes which rows return, but cannot cross a tenant boundary — the body never
  held another tenant's rows.
- **`join_cte` is an inner join**, so an outer row repeats once per matching CTE row. Use
  `.distinct()` when the CTE is a membership set rather than a 1:1 join — and narrow the
  projection first with `.select_only()`, because `SELECT DISTINCT` compares every selected
  column and PostgreSQL's `json` has no equality operator (`jsonb` does).
- **Ordering is split by where the column comes from.** `.order_by(E::Column, ..)` for an
  entity column, `.order_by_cte("cte", "col", ..)` for a CTE's own column (e.g. a recursive
  walk's depth). Only the first is combinable with `.distinct()`: PostgreSQL and MySQL both
  reject `ORDER BY` on an expression that is not in a `SELECT DISTINCT` list, so pairing
  `.distinct()` with `.order_by_cte()` returns `ScopeError::Invalid` on every backend rather
  than working on SQLite and failing in production. "Distinct rows, shallowest first" needs
  `GROUP BY … ORDER BY MIN(depth)`, which this API cannot express yet — dedup in the caller.
- **The outer query selects every column of `E` by default.** `all_as::<T>()` narrows
  *deserialization*, not the SQL — a hop needing only ids still transfers the wide columns,
  and on PostgreSQL that turns an index-only scan into a heap visit per row (measured 0.371
  ms against 0.079 ms). Use `.select_only()` plus `.column()` / `.column_from_cte()` /
  `.expr_as()` to narrow it, and pair that with `all_as::<T>()` rather than `all()`.
- **Don't `OR` two columns in a `join_cte` predicate.** `node.id = cte.src OR node.id =
  cte.dst` drops the index and sequentially scans (15.2 ms against 0.30 ms on 199k nodes).
  Put the alternation inside the CTE body instead, projecting one column that already holds
  both endpoints.
- **Recursive walks need a depth cap**, passed to `RecursiveCte::new`. PostgreSQL's `CYCLE`
  clause is not portable (sea_query no-ops it on MySQL and SQLite), so the cap is the only
  termination guarantee. Separately, the dedup mode decides how much work happens inside
  that bound: the default `RecursiveDedup::Union` discards rows duplicating ones already
  produced, bounding re-expansion by *(rows × depth)*; `UnionAll` keeps everything and
  enumerates *paths*, which multiplies on hub-shaped graphs. Neither is a visited set.
- **A recursive walk spans one self-referencing table.** The recursive member joins `J` to
  the CTE on `J.link_col = cte.anchor_col`, so both endpoints must be columns of the same
  entity — an edge table (`src`, `dst`) or a parent-pointer tree (`parent_id`, `id`). A hop
  *through* a separate table (`node -> edge -> node`) needs a three-way join and is not
  expressible; use one scoped query per hop instead.

For a hierarchy that is traversed often, prefer a closure table (as `tenant_closure` does)
over recursing on every read — `recursive_cte` is the right tool for rarely-walked or
frequently-changing trees, where materializing is not worth the write-path cost.

## Executors: `DBRunner` and `SecureTx`

- Repository methods should accept **`runner: &impl DBRunner`**, not `&SecureConn`.
- Inside a transaction callback, you get **`&SecureTx`**. It also implements `DBRunner`, so the same repository methods work both inside and outside a transaction.

Example signature:

```rust
use toolkit_db::secure::{AccessScope, DBRunner};

pub async fn create_user(
    runner: &impl DBRunner,
    scope: &AccessScope,
    user: user::ActiveModel,
) -> Result<user::Model, ScopeError> {
    // ...
}
```

## Transactions

### Transaction with SecureConn

`in_transaction_mapped` consumes the `SecureConn` and returns `(SecureConn, Result<T, E>)`, preventing accidental use of the outer connection inside the transaction:

```rust
pub async fn transfer_user(
    &self,
    ctx: &SecurityContext,
    from_tenant: Uuid,
    to_tenant: Uuid,
    user_id: Uuid,
) -> Result<(), DomainError> {
    let secure_conn = self.db.sea_secure();
    let scope = enforcer.access_scope(ctx, &resources::USER, actions::UPDATE, None).await?;

    let (_conn, result) = secure_conn
        .in_transaction_mapped(DomainError::database_infra, move |tx| {
            Box::pin(async move {
                // tx is &SecureTx — use it as the runner for repository calls
                // repo.transfer_user(tx, &scope, from_tenant, to_tenant, user_id).await?;
                Ok(())
            })
        })
        .await;
    result
}
```

## Row locks (`SELECT … FOR UPDATE`)

Secure ORM has no row-lock API of its own, and `DBRunner` is sealed — but a lock is a
property of the sea-orm query built *before* the Secure ORM wrapper, so it composes
normally: call `.lock(LockType::Update)` on the `Select` before `.secure().scope_with(..)`.
The scope/tenant filter still applies to the locked rows.

```rust
use sea_orm::QuerySelect;

ChatEntity::find()
    .filter(chat::Column::Id.eq(id))
    .lock(sea_orm::sea_query::LockType::Update)
    .secure()
    .scope_with(&scope)
    .one(txn) // txn: &SecureTx, inside in_transaction_mapped
    .await?
```

See `find_model_by_id_for_update` in
`resource-group/src/infra/storage/group_repo.rs` and `get_for_update` in
`mini-chat/src/infra/db/repo/chat_repo.rs` — both are called only with a transaction's
runner, never with the outer `SecureConn`.

**When you need it**: check-then-act inside a transaction, where the decision depends on
rows a concurrent transaction could insert or change between the check and the act — "delete
the parent only if it has no children", "last child of a container ⇒ delete the container
too", or a CAS that must read several rows as a stable set. A plain `DELETE … WHERE NOT
EXISTS (SELECT 1 FROM children …)` does **not** give this guarantee under PostgreSQL's
default `READ COMMITTED`: the `NOT EXISTS` subquery sees the snapshot from the start of the
`DELETE` statement, the concurrent `INSERT` of a child only takes `FOR KEY SHARE` on the
parent row, and once that insert commits the `DELETE` proceeds without re-evaluating the
subquery (no EvalPlanQual recheck — that machinery only reruns the statement's own `WHERE`,
not a subquery inside it). Locking the parent with `FOR UPDATE` first closes the gap: it
conflicts with the child insert's `FOR KEY SHARE` on the same row, so the two transactions
serialize instead of interleaving.

Rules:

- Take the lock as the transaction's **first** statement, and always in the same order
  (parent before children) — locking out of order across call sites is the standard way to
  deadlock.
- A lock only holds **inside a transaction**; outside one it is released as soon as the
  `SELECT` completes, so it protects nothing.
- Keep the transaction short: no external I/O (HTTP calls, other services) while a row lock
  is held.
- `NOWAIT` / `SKIP LOCKED` are for queue-style dequeue, not this pattern — see how the
  outbox picks its next batch with raw `FOR UPDATE SKIP LOCKED` (`libs/toolkit-db/src/outbox/dialect.rs`,
  `statements.rs`). Ordinary check-then-act should block and wait, not skip.
- `LockType::Share` (`FOR SHARE`) is for "let others also read, but block writers" —
  reading a row you depend on without modifying it. `NoKeyUpdate` locks a row you are about
  to update without touching its key, so it does not conflict with a `FOR KEY SHARE` taken by
  someone inserting a *referencing* row — used when you want your update to proceed
  alongside child inserts rather than serialize with them.

**Dialects**: on PostgreSQL and MySQL, `.lock(..)` renders `FOR UPDATE`/`FOR SHARE`/etc. and
takes a real row lock (checked in `sea-query`'s `backend/{postgres,mysql}/query.rs`). On
SQLite it renders **nothing** — `backend/sqlite/query.rs`'s `prepare_select_lock` is a
literal no-op ("SQLite doesn't supports row locking"), so `.lock(..)` silently compiles to a
plain `SELECT` there. Correctness on SQLite instead comes from having a single writer: any
write takes a database-level `RESERVED` lock, serializing all writers regardless of what any
`SELECT` asked for. Practically this means a race test for this pattern only proves anything
against PostgreSQL (`testcontainers`) — running it against the SQLite backend passes for the
wrong reason.

## Repository pattern

### Repository with `DBRunner` (works with both `SecureConn` and `SecureTx`)

```rust
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt};
use sea_orm::Set;

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
        new_user: user_info_sdk::NewUser,
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
- [ ] Use `in_transaction_mapped` for multi-step mutations.
- [ ] Use raw SQL only in migration infrastructure (migration runner + migration
      definitions) — including raw `WITH`.
- [ ] Build CTEs with `with_ctes()`/`cte()`/`recursive_cte()`, and reference them with
      `join_cte()`.
- [ ] Give every `recursive_cte` an explicit `max_depth`.
- [ ] Add indexes on security columns (`tenant_id`, `resource_id`).
- [ ] Provide `DatabaseCapability::migrations()` returning SeaORM migrations.

## Related docs

- Security data path (AuthN/AuthZ, SecureConn, AccessScope): [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md)
- OData pagination / filtering: [`07_odata_pagination_select_filter.md`](./07_odata_pagination_select_filter.md)
- Canonical example: `examples/toolkit/users-info/`

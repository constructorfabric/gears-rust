---
title: Database patterns
description: Secure, tenant-scoped persistence with SecureConn, Scopable entities, transactions, and migrations.
sidebar:
  label: Database
  order: 2
---

Gears never touch a raw database connection. All access flows through the secure ORM layer
(`toolkit-db`), which applies an [`AccessScope`](../../concepts/security-and-tenancy/) to every
query as automatic `WHERE` clauses. This guide shows the patterns you'll use day to day.

## Make an entity scopable

A SeaORM entity opts into row-level security with `#[derive(Scopable)]`, declaring which
columns map to the security dimensions (tenant / resource / owner / type):

```rust title="infra/storage/entity/user.rs"
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    // …
}
```

Use `no_owner` / `no_type` when a dimension doesn't apply. An entity owned by a user would
instead set an owner column, e.g. `#[secure(tenant_col = "tenant_id", owner_col = "user_id")]`.

## Read and write through SecureConn

Repository methods are generic over `DBRunner` (so the same code works for a connection or a
transaction). Reads chain `.secure().scope_with(scope)`; writes use the `secure_insert` /
`secure_update_with_scope` helpers:

```rust title="infra/storage/users_sea_repo.rs"
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureDeleteExt, secure_insert, secure_update_with_scope};

async fn get<C: DBRunner>(&self, conn: &C, scope: &AccessScope, id: Uuid)
    -> Result<Option<User>, DomainError>
{
    let found = UserEntity::find()
        .filter(Expr::col(Column::Id).eq(id))
        .secure().scope_with(scope)
        .one(conn).await.map_err(db_err)?;
    Ok(found.map(Into::into))
}

async fn create<C: DBRunner>(&self, conn: &C, scope: &AccessScope, user: User)
    -> Result<User, DomainError>
{
    let m = UserAM { id: Set(user.id), tenant_id: Set(user.tenant_id), /* … */ };
    secure_insert::<UserEntity>(m, scope, conn).await.map_err(db_err)?;
    Ok(user)
}

async fn delete<C: DBRunner>(&self, conn: &C, scope: &AccessScope, id: Uuid)
    -> Result<bool, DomainError>
{
    let res = UserEntity::delete_many()
        .filter(Expr::col(Column::Id).eq(id))
        .secure().scope_with(scope)
        .exec(conn).await.map_err(db_err)?;
    Ok(res.rows_affected > 0)
}
```

The scope is obtained from the `PolicyEnforcer` in the domain service and passed down — see
[Authorization](../authorization/). An empty scope matches **no rows**
(deny-by-default), and `tenant_id` is immutable on update.

## Acquire a connection

The domain service gets a connection from the injected `DBProvider`; the gear acquires the
provider in `init` via the `db` capability:

```rust
// in the gear's init():
let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

// in a service method:
let conn = self.db.conn().map_err(DomainError::from)?;
```

## Transactions

Run multiple writes atomically with the transaction helper; the closure receives a
`SecureTx` that carries the same scope, so writes stay scoped inside the transaction:

```rust
let (_conn, result) = secure_conn
    .in_transaction_mapped(
        DomainError::database_infra,
        move |tx| Box::pin(async move {
            // tx: &SecureTx — use secure_insert/scope_with against `tx`
            Ok(())
        }),
    )
    .await;
```

Domain events can be enqueued in the same transaction via the **transactional outbox**, so
they commit atomically with the data and are delivered reliably afterward.

## Register migrations

A gear owns its schema. Expose migrations from the `db` capability and the runtime runs them
in the **DB migration** lifecycle phase:

```rust title="infra/storage/migrations/mod.rs"
pub struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260111_000001_initial::Migration), /* … */]
    }
}
```

```rust title="gear.rs"
impl DatabaseCapability for MyGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        crate::infra::storage::migrations::Migrator::migrations()
    }
}
```

Individual migrations are ordinary SeaORM `MigrationTrait` impls (the example writes
backend-aware SQL via `manager.get_connection().execute_unprepared(...)`).

## See also

- [Security & multi-tenancy](../../concepts/security-and-tenancy/) — where the scope comes from.
- [Authorization](../authorization/) — obtaining an `AccessScope`.
- [Pagination & filtering](../odata/) — `paginate_odata` over a scoped query.
- Full code: `examples/toolkit/users-info/users-info/src/infra/storage/`.

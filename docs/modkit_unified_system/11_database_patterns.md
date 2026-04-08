# Database Execution Patterns

This document covers database execution mechanics in Cyber Fabric: the `DBRunner` abstraction, transactions, the repository pattern, and database migrations.

For security-scoped database access (`SecureConn`, `AccessScope`, `PolicyEnforcer` PEP pattern), see [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md).

## Core invariants

- **Rule**: No plain SQL in handlers/services/repos. Raw SQL is allowed only in migration infrastructure.
- **Rule**: Repository methods accept `runner: &impl DBRunner`, not `&SecureConn`.
- **Rule**: Use `in_transaction_mapped` for transactional work.
- **Rule**: Each module gets its own isolated migration history table.

## Executors: `DBRunner` and `SecureTx`

- Repository methods should accept **`runner: &impl DBRunner`**, not `&SecureConn`.
- Inside a transaction callback, you get **`&SecureTx`**. It also implements `DBRunner`, so the same repository methods work both inside and outside a transaction.

Example signature:

```rust
use modkit_db::secure::{AccessScope, DBRunner};

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

## Repository pattern

### Repository with `DBRunner` (works with both `SecureConn` and `SecureTx`)

```rust
use modkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt};
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

        modkit_db::secure::secure_insert::<user::Entity>(am, scope, runner).await
    }
}
```

## Database migrations

Modules provide migration definitions that the runtime executes with a privileged connection:

```rust
impl DatabaseCapability for MyModule {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}
```

Each module gets its own migration history table (`modkit_migrations__<prefix>__<hash8>`), ensuring isolation between modules.

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

Raw SQL is **allowed only in migration infrastructure** (migration runner + migration definitions). Module code (handlers/services/repos) must use the Secure ORM.

## Quick checklist

- [ ] Use `runner: &impl DBRunner` in repository method signatures.
- [ ] Use `in_transaction_mapped` for multi-step mutations.
- [ ] Use raw SQL only in `migrations/*.rs`.
- [ ] Add indexes on security columns (`tenant_id`, `resource_id`).
- [ ] Provide `DatabaseCapability::migrations()` returning SeaORM migrations.

## Related docs

- Security data path (AuthN/AuthZ, SecureConn, AccessScope): [`06_authn_authz_secure_orm.md`](./06_authn_authz_secure_orm.md)
- OData pagination / filtering: [`07_odata_pagination_select_filter.md`](./07_odata_pagination_select_filter.md)
- Canonical example: `examples/modkit/users-info/`

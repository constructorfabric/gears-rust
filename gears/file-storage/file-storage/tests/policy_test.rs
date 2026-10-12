//! `Store::upsert_policy` upsert-race tests on a temp-file `SQLite` DB (`sqlite::memory:` would
//! give each pooled connection its own DB).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm::{EntityTrait, PaginatorTrait};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::entity::policy::Entity as PolicyEntity;
use file_storage::infra::storage::migrations::Migrator;

const TENANT: &str = "00000000-0000-0000-0000-0000000000a1";
const OWNER: &str = "00000000-0000-0000-0000-0000000000b1";

/// `Store` over a fresh temp-file DB with migrations, plus the raw DSN for a second connection.
async fn build_store() -> (Store, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-policy-{}.db", Uuid::now_v7().simple()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));
    (Store::new(db), dsn)
}

fn body_with_max_bytes(max_bytes: u64) -> PolicyBody {
    PolicyBody {
        size_limits: SizeLimits {
            max_bytes: Some(max_bytes),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Two sequential upserts for one tenant scope leave exactly one row carrying the second body.
#[tokio::test]
async fn policy_upsert_on_conflict_updates_existing_row_not_duplicates() {
    let (store, dsn) = build_store().await;
    let scope = AccessScope::allow_all();
    let tenant_id: Uuid = TENANT.parse().expect("valid uuid");
    let now = time::OffsetDateTime::now_utc();

    let first_body = body_with_max_bytes(100);
    let second_body = body_with_max_bytes(200);

    store
        .upsert_policy(
            &scope,
            tenant_id,
            &PolicyScope::Tenant,
            None,
            &first_body,
            now,
        )
        .await
        .expect("first upsert must succeed");
    store
        .upsert_policy(
            &scope,
            tenant_id,
            &PolicyScope::Tenant,
            None,
            &second_body,
            now,
        )
        .await
        .expect("second upsert must succeed");

    // Independent raw connection, purely for the row count.
    let raw = sea_orm::Database::connect(&dsn)
        .await
        .expect("second raw connection");
    let count = PolicyEntity::find()
        .count(&raw)
        .await
        .expect("count policies");
    assert_eq!(
        count, 1,
        "two upserts for the same scope must leave exactly one row, not {count}"
    );

    let stored = store
        .get_policy(&scope, tenant_id, &PolicyScope::Tenant, None)
        .await
        .expect("get_policy must succeed")
        .expect("policy row must exist");
    assert_eq!(
        stored.body, second_body,
        "the surviving row must carry the second call's body, not the first's"
    );
}

/// Same for a user-scope row (`scope_owner_id = Some(..)`).
#[tokio::test]
async fn policy_upsert_on_conflict_updates_existing_user_scope_row() {
    let (store, dsn) = build_store().await;
    let scope = AccessScope::allow_all();
    let tenant_id: Uuid = TENANT.parse().expect("valid uuid");
    let owner_id: Uuid = OWNER.parse().expect("valid uuid");
    let now = time::OffsetDateTime::now_utc();

    let first_body = body_with_max_bytes(10);
    let second_body = body_with_max_bytes(20);

    store
        .upsert_policy(
            &scope,
            tenant_id,
            &PolicyScope::User,
            Some(owner_id),
            &first_body,
            now,
        )
        .await
        .expect("first upsert must succeed");
    store
        .upsert_policy(
            &scope,
            tenant_id,
            &PolicyScope::User,
            Some(owner_id),
            &second_body,
            now,
        )
        .await
        .expect("second upsert must succeed");

    let raw = sea_orm::Database::connect(&dsn)
        .await
        .expect("second raw connection");
    let count = PolicyEntity::find()
        .count(&raw)
        .await
        .expect("count policies");
    assert_eq!(
        count, 1,
        "two upserts for the same user scope must leave exactly one row, not {count}"
    );

    let stored = store
        .get_policy(&scope, tenant_id, &PolicyScope::User, Some(owner_id))
        .await
        .expect("get_policy must succeed")
        .expect("policy row must exist");
    assert_eq!(
        stored.body, second_body,
        "the surviving row must carry the second call's body, not the first's"
    );
}

//! Repo-level test for `IdempotencyRepo::delete_expired`'s batch cap.
//!
//! Runs against a real SQLite DB with the full migration applied, mirroring
//! `tests/version_repo_test.rs`/`tests/metadata_repo_test.rs`'s own setup.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm::Set;
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::secure_insert;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::infra::storage::entity::file::{
    ActiveModel as FileActiveModel, Entity as FileEntity,
};
use file_storage::infra::storage::entity::idempotency_key::ActiveModel as IdempotencyActiveModel;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::IdempotencyRepo;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (mirrors `version_repo_test.rs::db()`).
async fn db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-idempotency-repo-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let conn = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&conn, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(conn))
}

/// `IdempotencyRepo::delete_expired` must delete at most `limit` rows, even
/// when strictly more than `limit` rows are eligible (`expires_at <= now`) --
/// the unbounded single-`DELETE` behavior this replaces would have removed
/// all of them in one statement.
#[tokio::test]
async fn delete_expired_caps_at_limit_when_more_rows_are_eligible() {
    const N: usize = 5;
    const LIMIT: u64 = 3;

    let db = db().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let idem = IdempotencyRepo::new();

    let tenant_id = Uuid::now_v7();
    let owner_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let expired_at = now - time::Duration::hours(1);

    // Parent `files` row -- `idempotency_keys.file_id` carries `ON DELETE
    // CASCADE` back to it (m20260701_000001_p2_initial).
    let file_am = FileActiveModel {
        file_id: Set(file_id),
        tenant_id: Set(tenant_id),
        owner_kind: Set("user".to_owned()),
        owner_id: Set(owner_id),
        name: Set("f.bin".to_owned()),
        gts_file_type: Set(GTS.to_owned()),
        content_id: Set(None),
        meta_version: Set(0),
        created_at: Set(now),
        last_modified_at: Set(now),
    };
    secure_insert::<FileEntity>(file_am, &scope, &conn)
        .await
        .expect("seed parent file");

    for i in 0..N {
        let am = IdempotencyActiveModel {
            tenant_id: Set(tenant_id),
            owner_kind: Set("user".to_owned()),
            owner_id: Set(owner_id),
            idempotency_key: Set(format!("key-{i}")),
            subject_id: Set(owner_id),
            file_id: Set(file_id),
            response_status: Set(201),
            response_body: Set("{}".to_owned()),
            response_etag: Set(String::new()),
            request_hash: Set(vec![0u8; 32]),
            created_at: Set(now - time::Duration::hours(2)),
            expires_at: Set(expired_at),
        };
        secure_insert::<file_storage::infra::storage::entity::idempotency_key::Entity>(
            am, &scope, &conn,
        )
        .await
        .expect("seed idempotency key");
    }

    let deleted = idem
        .delete_expired(&conn, now, LIMIT)
        .await
        .expect("delete_expired must not error");
    assert_eq!(
        deleted, LIMIT,
        "N + 1 (well, N=5) eligible rows with limit=3 must delete exactly 3"
    );

    let remaining = idem
        .delete_expired(&conn, now, 100)
        .await
        .expect("second delete_expired must not error");
    assert_eq!(
        remaining,
        (N as u64) - LIMIT,
        "the remainder must still be deletable on a later sweep pass"
    );
}

#![cfg(feature = "integration")]
// The fixture and the seed are plain helpers, which `allow-expect-in-tests`
// does not reach; a failed expectation there is the test failing, as in the
// sibling PostgreSQL suites.
#![allow(clippy::expect_used)]
//! A refused category delete on a real `PostgreSQL`.
//!
//! `PostgreSQL` aborts a transaction on any failed statement, so a recount on
//! the refused transaction would answer `25P02` and the race — a declaration
//! created between the advisory check and the delete — would turn from a `409`
//! into a `500`. `SQLite` keeps the transaction usable, which is why only this
//! suite can tell. The advisory check is made to answer 0 so that the real
//! `ON DELETE RESTRICT` is what refuses.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_category_delete_test
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use cf_gears_settings_service::api::rest::handlers::delete_reporting_count;
use cf_gears_settings_service::audit::{AuditRecord, AuditSink};
use cf_gears_settings_service::domain::category::{
    Category, CategoryDraft, CategoryKey, CategoryPatch, CategoryRepository, CategoryService,
    DomainVisibility,
};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::domain::odata::Listing;
use cf_gears_settings_service::infra::storage::category_repo::CategoryRepo;
use cf_gears_settings_service::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

mod common;
use common::pg_fixture;

/// The audit sink a refused delete never reaches.
struct NoAudit;

#[async_trait]
impl AuditSink for NoAudit {
    async fn append<C: DBRunner>(
        &self,
        _conn: &C,
        _scope: &AccessScope,
        _record: AuditRecord,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

/// The real repository, except that the first declaration count answers 0: the
/// advisory check lets the delete through, and the foreign key is what refuses.
struct RacedRepo {
    inner: CategoryRepo,
    counts: AtomicUsize,
}

#[async_trait]
impl CategoryRepository for RacedRepo {
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Category>, DomainError> {
        self.inner.find(conn, scope, id).await
    }

    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &CategoryKey,
    ) -> Result<Option<Category>, DomainError> {
        self.inner.find_by_key(conn, scope, key).await
    }

    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: CategoryDraft,
    ) -> Result<Category, DomainError> {
        self.inner.insert(conn, scope, draft).await
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: CategoryPatch,
        expected: time::OffsetDateTime,
    ) -> Result<Category, DomainError> {
        self.inner.update(conn, scope, id, patch, expected).await
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.inner.delete(conn, scope, id, expected).await
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        query: &ODataQuery,
    ) -> Result<Listing<Category>, DomainError> {
        self.inner.list(conn, scope, visibility, query).await
    }

    async fn count_referencing_declarations<C: DBRunner>(
        &self,
        conn: &C,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        if self.counts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(0);
        }
        self.inner.count_referencing_declarations(conn, id).await
    }
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql).await.expect("statement runs");
}

/// A category with one declaration filed under it.
async fn seed_a_category_holding_a_declaration(conn: &DatabaseConnection) -> Uuid {
    let category = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO categories (id, key, name)
             VALUES ('{category}', 'network', 'Network')"
        ),
    )
    .await;
    let id = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO setting_declarations
               (id, key, leaf_slug, value_type_id, category_id, default_value,
                scope_class, created_by)
             VALUES ('{id}',
                     'gts.cf.core.settings.setting_type.v1~acme.settings.network.one.v1~',
                     'one', 'gts.cf.core.settings.type_bool_flag.v1~', '{category}', 'true',
                     'local', 'fixture')"
        ),
    )
    .await;
    category
}

async fn categories_named(conn: &DatabaseConnection, id: Uuid) -> usize {
    conn.query_all_raw(sea_orm::Statement::from_string(
        conn.get_database_backend(),
        format!("SELECT id FROM categories WHERE id = '{id}'"),
    ))
    .await
    .expect("readable")
    .len()
}

/// The foreign key refuses a delete the advisory check let through; the refusal
/// stays a `409` and names the declaration, rather than becoming a `500` over a
/// recount on the aborted transaction.
#[tokio::test]
async fn a_delete_the_foreign_key_refuses_is_a_conflict_naming_the_count_not_an_internal_error() {
    let Some(fixture) = pg_fixture("category delete").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let id = seed_a_category_holding_a_declaration(&fixture.conn).await;

    let db = DBProvider::<DbError>::new(fixture.db.clone());
    let svc = std::sync::Arc::new(CategoryService::new(
        RacedRepo {
            inner: CategoryRepo,
            counts: AtomicUsize::new(0),
        },
        NoAudit,
    ));
    let scope = AccessScope::allow_all();
    let current = {
        let conn = db.conn().expect("connection");
        CategoryRepo
            .find(&conn, &scope, id)
            .await
            .expect("lookup")
            .expect("the seeded category")
    };
    let ctx = SecurityContext::anonymous();

    let refused = delete_reporting_count(
        &db,
        &svc,
        &scope,
        id,
        Some(current.etag.as_str()),
        "req-1",
        &ctx,
    )
    .await;

    match refused {
        Err(DomainError::Conflict { detail }) => {
            assert!(detail.contains("1 declaration"), "{detail}");
        }
        other => panic!("expected a conflict naming the count, got {other:?}"),
    }
    assert_eq!(categories_named(&fixture.conn, id).await, 1);
}

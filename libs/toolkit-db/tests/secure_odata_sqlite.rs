#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "sqlite")]

//! `SQLite` integration tests for `OData` + Secure ORM execution.
//!
//! Security contract:
//! - Do not use any raw SeaORM/SQLx executors from test code.
//! - Execute queries only through `SecureConn` / `SecureTx` + secure wrappers.

use anyhow::anyhow;
use sea_orm::Set;
use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude as mig;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::odata::FieldMap;
use toolkit_db::odata::pager::OPager;
use toolkit_db::secure::{
    Db, DbConn, ScopableEntity, SecureEntityExt, secure_insert, secure_insert_many,
};
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_odata::ODataQuery;
use toolkit_odata::filter::FieldKind;
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

mod ent {
    use sea_orm::entity::prelude::*;
    use uuid::Uuid;

    #[derive(Debug, Clone, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "secure_odata_test")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub tenant_id: Uuid,
        pub name: String,
        pub score: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

impl ScopableEntity for ent::Entity {
    fn tenant_col() -> Option<<Self as EntityTrait>::Column> {
        Some(ent::Column::TenantId)
    }
    fn resource_col() -> Option<<Self as EntityTrait>::Column> {
        None
    }
    fn owner_col() -> Option<<Self as EntityTrait>::Column> {
        None
    }
    fn type_col() -> Option<<Self as EntityTrait>::Column> {
        None
    }
    fn resolve_property(property: &str) -> Option<<Self as EntityTrait>::Column> {
        match property {
            p if p == pep_properties::OWNER_TENANT_ID => Self::tenant_col(),
            _ => None,
        }
    }
}

struct CreateSecureOdataTest;

impl mig::MigrationName for CreateSecureOdataTest {
    fn name(&self) -> &'static str {
        "m001_create_secure_odata_test"
    }
}

#[async_trait::async_trait]
impl mig::MigrationTrait for CreateSecureOdataTest {
    async fn up(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .create_table(
                mig::Table::create()
                    .table(mig::Alias::new("secure_odata_test"))
                    .if_not_exists()
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("tenant_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("name"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("score"))
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .drop_table(
                mig::Table::drop()
                    .table(mig::Alias::new("secure_odata_test"))
                    .to_owned(),
            )
            .await
    }
}

// Helper struct to manage test database lifecycle
struct TestDb {
    db: Db,
    tenant_id: Uuid,
    scope: AccessScope,
}

impl TestDb {
    async fn new() -> Self {
        let db = connect_db("sqlite::memory:", ConnectOpts::default())
            .await
            .expect("db connect");

        run_migrations_for_testing(&db, vec![Box::new(CreateSecureOdataTest)])
            .await
            .map_err(|e| anyhow!(e.to_string()))
            .expect("migrate");

        let tenant_id = Uuid::new_v4();
        let scope = AccessScope::for_tenants(vec![tenant_id]);

        Self {
            db,
            tenant_id,
            scope,
        }
    }

    fn conn(&self) -> DbConn<'_> {
        self.db.conn().expect("conn")
    }
}

async fn seed<R: toolkit_db::secure::DBRunner>(runner: &R, tenant_id: Uuid, scope: &AccessScope) {
    let rows = [("alice", 10), ("bob", 20), ("charlie", 30), ("dave", 40)];

    for (name, score) in rows {
        let am = ent::ActiveModel {
            tenant_id: Set(tenant_id),
            name: Set(name.to_owned()),
            score: Set(score),
            ..Default::default()
        };
        secure_insert::<ent::Entity>(am, scope, runner)
            .await
            .expect("insert");
    }
}

#[tokio::test]
async fn paginate_odata_works_with_secure_conn() {
    let test_db = TestDb::new().await;
    let conn = test_db.conn();
    seed(&conn, test_db.tenant_id, &test_db.scope).await;

    let fmap: FieldMap<ent::Entity> = FieldMap::new()
        .insert_with_extractor("id", ent::Column::Id, FieldKind::I64, |m: &ent::Model| {
            m.id.to_string()
        })
        .insert("name", ent::Column::Name, FieldKind::String)
        .insert("score", ent::Column::Score, FieldKind::I64);

    let q = ODataQuery {
        limit: Some(2),
        ..Default::default()
    };

    let page = OPager::<ent::Entity, _>::new(&test_db.scope, &conn, &fmap)
        .fetch(&q, |m| (m.name, m.score))
        .await
        .expect("fetch");

    assert_eq!(page.items.len(), 2, "page size");
}

#[tokio::test]
async fn secure_insert_many_inserts_all_rows_in_one_call() {
    let test_db = TestDb::new().await;
    let conn = test_db.conn();

    let rows = [("alice", 10), ("bob", 20), ("charlie", 30)];
    let models: Vec<ent::ActiveModel> = rows
        .iter()
        .map(|(name, score)| ent::ActiveModel {
            tenant_id: Set(test_db.tenant_id),
            name: Set((*name).to_owned()),
            score: Set(*score),
            ..Default::default()
        })
        .collect();

    secure_insert_many::<ent::Entity>(models, &test_db.scope, &conn)
        .await
        .expect("batch insert");

    let count = ent::Entity::find()
        .secure()
        .scope_with(&test_db.scope)
        .count(&conn)
        .await
        .expect("count rows");
    assert_eq!(count, 3, "all three rows must land in a single call");
}

/// The row-count assertion above would also pass for a loop of per-row
/// inserts, which is exactly what `secure_insert_many` exists to replace.
/// This pins the contract that matters: one statement for a batch that fits.
///
/// Needs the recorder, hence the feature gate.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn secure_insert_many_issues_exactly_one_statement() {
    use toolkit_db::test_support::{QueryKind, connect_with_recorder};

    let (db, rec) = connect_with_recorder("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("db connect with recorder");
    run_migrations_for_testing(&db, vec![Box::new(CreateSecureOdataTest)])
        .await
        .map_err(|e| anyhow!(e.to_string()))
        .expect("migrate");
    let tenant_id = Uuid::new_v4();
    let scope = AccessScope::for_tenants(vec![tenant_id]);
    // Migrations ran through the same callback.
    rec.clear();

    let models: Vec<ent::ActiveModel> = (0..8)
        .map(|i| ent::ActiveModel {
            tenant_id: Set(tenant_id),
            name: Set(format!("user{i}")),
            score: Set(i),
            ..Default::default()
        })
        .collect();

    let conn = db.conn().expect("conn");
    secure_insert_many::<ent::Entity>(models, &scope, &conn)
        .await
        .expect("batch insert");

    let inserts = rec
        .events()
        .into_iter()
        .filter(|e| e.kind == QueryKind::Insert)
        .count();
    assert_eq!(
        inserts,
        1,
        "eight rows must go out as one INSERT, not a loop:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn secure_insert_many_empty_vec_is_a_noop() {
    let test_db = TestDb::new().await;
    let conn = test_db.conn();

    secure_insert_many::<ent::Entity>(Vec::new(), &test_db.scope, &conn)
        .await
        .expect("empty batch must be Ok(())");

    let count = ent::Entity::find()
        .secure()
        .scope_with(&test_db.scope)
        .count(&conn)
        .await
        .expect("count rows");
    assert_eq!(count, 0, "an empty batch must not touch the database");
}

#[tokio::test]
async fn secure_insert_many_rejects_whole_batch_on_one_scope_violation() {
    let test_db = TestDb::new().await;
    let conn = test_db.conn();

    // One row's tenant is outside the scope, so the whole call must fail --
    // scope validation runs in memory over the whole batch before any row
    // reaches the database.
    let other_tenant = Uuid::new_v4();
    let models = vec![
        ent::ActiveModel {
            tenant_id: Set(test_db.tenant_id),
            name: Set("alice".to_owned()),
            score: Set(1),
            ..Default::default()
        },
        ent::ActiveModel {
            tenant_id: Set(other_tenant),
            name: Set("mallory".to_owned()),
            score: Set(2),
            ..Default::default()
        },
        ent::ActiveModel {
            tenant_id: Set(test_db.tenant_id),
            name: Set("bob".to_owned()),
            score: Set(3),
            ..Default::default()
        },
    ];

    let err = secure_insert_many::<ent::Entity>(models, &test_db.scope, &conn)
        .await
        .expect_err("a batch with an out-of-scope row must be rejected");
    assert!(
        matches!(err, toolkit_db::secure::ScopeError::Denied(_)),
        "expected a Denied scope error, got: {err}"
    );

    let count = ent::Entity::find()
        .secure()
        .scope_with(&test_db.scope)
        .count(&conn)
        .await
        .expect("count rows");
    assert_eq!(
        count, 0,
        "no row from the rejected batch may be inserted, not even the in-scope ones"
    );
}

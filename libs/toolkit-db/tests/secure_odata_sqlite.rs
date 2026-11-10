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
use toolkit_db::secure::{Db, DbConn, ScopableEntity, secure_insert};
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
    fn scope_columns() -> Vec<<Self as EntityTrait>::Column> {
        vec![ent::Column::TenantId]
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

// ---------------------------------------------------------------------------
// A cursor continues only the listing whose `$filter` it was minted under.
//
// Driven through the three pagers a gear actually calls, so each one's check
// and each one's cursor minting are under test, not the rule in isolation.
// The first page is built in-process, without a stamped `filter_hash`: the
// cursor it mints must still record the filter, or a replay without one could
// not be told apart from a continuation.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum Field {
    Id,
    Name,
    Score,
}

impl toolkit_odata::filter::FilterField for Field {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Name, Self::Score];

    fn name(&self) -> &'static str {
        self.into()
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::Score => FieldKind::I64,
            Self::Name => FieldKind::String,
        }
    }
}

struct Mapper;

impl toolkit_db::odata::FieldToColumn<Field> for Mapper {
    type Column = ent::Column;

    fn map_field(field: Field) -> ent::Column {
        match field {
            Field::Id => ent::Column::Id,
            Field::Name => ent::Column::Name,
            Field::Score => ent::Column::Score,
        }
    }
}

impl toolkit_db::odata::ODataFieldMapping<Field> for Mapper {
    type Entity = ent::Entity;

    fn extract_cursor_value(model: &ent::Model, field: Field) -> sea_orm::Value {
        match field {
            Field::Id => sea_orm::Value::BigInt(Some(model.id)),
            Field::Name => sea_orm::Value::String(Some(model.name.clone())),
            Field::Score => sea_orm::Value::BigInt(Some(model.score)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Pager {
    OPagerFetch,
    PaginateOdata,
    PaginateOdataTry,
}

/// One page of names, through `pager`.
async fn page_of_names(
    pager: Pager,
    test_db: &TestDb,
    q: &ODataQuery,
) -> Result<toolkit_odata::Page<String>, toolkit_odata::Error> {
    use toolkit_db::odata::LimitCfg;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_odata::SortDir;

    let conn = test_db.conn();
    let limits = LimitCfg {
        default: 1,
        max: 10,
    };
    match pager {
        Pager::OPagerFetch => {
            let fmap: FieldMap<ent::Entity> = FieldMap::new()
                .insert_with_extractor("id", ent::Column::Id, FieldKind::I64, |m: &ent::Model| {
                    m.id.to_string()
                })
                .insert("name", ent::Column::Name, FieldKind::String)
                .insert("score", ent::Column::Score, FieldKind::I64);
            OPager::<ent::Entity, _>::new(&test_db.scope, &conn, &fmap)
                .tiebreaker("id", SortDir::Asc)
                .limits(limits.default, limits.max)
                .fetch(q, |m| m.name)
                .await
        }
        Pager::PaginateOdata => {
            toolkit_db::odata::paginate_odata::<Field, Mapper, ent::Entity, String, _, _>(
                ent::Entity::find().secure().scope_with(&test_db.scope),
                &conn,
                q,
                ("id", SortDir::Asc),
                limits,
                |m| m.name,
            )
            .await
        }
        Pager::PaginateOdataTry => toolkit_db::odata::sea_orm_filter::paginate_odata_try::<
            Field,
            Mapper,
            ent::Entity,
            String,
            _,
            std::convert::Infallible,
            _,
        >(
            ent::Entity::find().secure().scope_with(&test_db.scope),
            &conn,
            q,
            ("id", SortDir::Asc),
            limits,
            |m| Ok(m.name),
        )
        .await
        .map_err(|e| match e {
            toolkit_db::odata::sea_orm_filter::PaginateOdataTryError::OData(e) => e,
            toolkit_db::odata::sea_orm_filter::PaginateOdataTryError::MapError(never) => {
                match never {}
            }
        }),
    }
}

#[tokio::test]
async fn a_cursor_is_refused_under_an_omitted_or_different_filter() {
    let test_db = TestDb::new().await;
    seed(&test_db.conn(), test_db.tenant_id, &test_db.scope).await;

    let filter = |raw: &str| {
        toolkit_odata::parse_filter_string(raw)
            .expect("filter parses")
            .into_expr()
    };

    for pager in [
        Pager::OPagerFetch,
        Pager::PaginateOdata,
        Pager::PaginateOdataTry,
    ] {
        // Page 1 under `score gt 15`, built in-process: no stamped hash.
        let first = ODataQuery::new()
            .with_filter(filter("score gt 15"))
            .with_limit(1);
        let page = page_of_names(pager, &test_db, &first)
            .await
            .unwrap_or_else(|e| panic!("{pager:?}: page 1: {e}"));
        assert_eq!(page.items, ["bob"], "{pager:?}: page 1");
        let token = page
            .page_info
            .next_cursor
            .unwrap_or_else(|| panic!("{pager:?}: page 1 has more rows"));
        let cursor = toolkit_odata::CursorV1::decode(&token).expect("cursor decodes");
        assert!(
            cursor.f.is_some(),
            "{pager:?}: a cursor minted under a filter records it"
        );
        let next = || ODataQuery::new().with_cursor(cursor.clone()).with_limit(1);

        // The same filter continues the listing.
        let page = page_of_names(pager, &test_db, &next().with_filter(filter("score gt 15")))
            .await
            .unwrap_or_else(|e| panic!("{pager:?}: same filter: {e}"));
        assert_eq!(page.items, ["charlie"], "{pager:?}: page 2");

        // Without it, or under another one, it is a different listing.
        for (case, q) in [
            ("no filter", next()),
            ("another filter", next().with_filter(filter("score gt 25"))),
        ] {
            assert!(
                matches!(
                    page_of_names(pager, &test_db, &q).await,
                    Err(toolkit_odata::Error::FilterMismatch)
                ),
                "{pager:?}: {case} must be refused"
            );
        }
    }
}

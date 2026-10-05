#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Two `$filter` shapes, compiled and executed on both dialects.
//!
//! **Null equality.** On a field that declares itself nullable (`FilterField::nullable`),
//! `field eq null` / `field ne null` become `IS NULL` / `IS NOT NULL` on the typed path
//! (`paginate_odata`), as they always did on the legacy `FieldMap` path. The typed parser refuses
//! `null` on every other field.
//!
//! **String functions.** `contains`, `startswith` and `endswith` escape `%`, `_` and `\` in the
//! caller's text and say so: the `LIKE` carries `ESCAPE '\'`. `SQLite` has no default escape
//! character, so without the clause the escaped pattern matched a literal backslash followed by
//! a wildcard — `contains(name,'%')` found names holding a backslash, and `startswith(name,'a_')`
//! found nothing. Postgres defaults to the backslash, which is why only one dialect showed it.
//!
//! The rendering tests run in every build; the execution suite runs under the `integration`
//! feature on each backend compiled in (`make test-sqlite`, `make test-pg`).

use sea_orm::entity::prelude::*;
use sea_orm::{DbBackend, QueryFilter, QueryTrait};
use toolkit_db::odata::{
    FieldMap, FieldToColumn, ODataFieldMapping, expr_to_condition, filter_node_to_condition,
};
use toolkit_db::secure::ScopableEntity;
use toolkit_odata::filter::{FieldKind, FilterField, FilterNode, parse_odata_filter};
use toolkit_security::pep_properties;

#[cfg(all(feature = "integration", any(feature = "sqlite", feature = "pg")))]
mod common;

mod ent {
    use sea_orm::entity::prelude::*;
    use uuid::Uuid;

    #[derive(Debug, Clone, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "odata_null_like")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub tenant_id: Uuid,
        pub name: String,
        pub parent: Option<Uuid>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum Field {
    Id,
    Name,
    Parent,
}

impl FilterField for Field {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Name, Self::Parent];

    fn name(&self) -> &'static str {
        self.into()
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::I64,
            Self::Name => FieldKind::String,
            Self::Parent => FieldKind::Uuid,
        }
    }

    /// Only `parent` can be absent.
    fn nullable(&self) -> bool {
        matches!(self, Self::Parent)
    }
}

struct Mapper;

impl FieldToColumn<Field> for Mapper {
    type Column = ent::Column;

    /// Maps a filter field to its column.
    fn map_field(field: Field) -> ent::Column {
        match field {
            Field::Id => ent::Column::Id,
            Field::Name => ent::Column::Name,
            Field::Parent => ent::Column::Parent,
        }
    }
}

impl ODataFieldMapping<Field> for Mapper {
    type Entity = ent::Entity;

    /// Reads a row's value for a cursor field.
    fn extract_cursor_value(model: &ent::Model, field: Field) -> sea_orm::Value {
        match field {
            Field::Id => sea_orm::Value::BigInt(Some(model.id)),
            Field::Name => sea_orm::Value::String(Some(model.name.clone())),
            Field::Parent => sea_orm::Value::Uuid(model.parent),
        }
    }
}

/// Parses a `$filter` string into a typed filter node, panicking on a parse error.
fn node(raw: &str) -> FilterNode<Field> {
    parse_odata_filter::<Field>(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
}

/// The typed path's SQL for `raw`, values inlined.
fn typed_sql(raw: &str, backend: DbBackend) -> String {
    let condition = filter_node_to_condition::<Field, Mapper>(&node(raw)).unwrap();
    ent::Entity::find()
        .filter(condition)
        .build(backend)
        .to_string()
}

/// The legacy `FieldMap` over the same columns, for the untyped path.
fn field_map() -> FieldMap<ent::Entity> {
    FieldMap::new()
        .insert_with_extractor("id", ent::Column::Id, FieldKind::I64, |m: &ent::Model| {
            m.id.to_string()
        })
        .insert("name", ent::Column::Name, FieldKind::String)
        .insert("parent", ent::Column::Parent, FieldKind::Uuid)
}

/// The legacy `FieldMap` path's SQL for `raw`, values inlined.
fn legacy_sql(raw: &str, backend: DbBackend) -> String {
    let parsed = toolkit_odata::parse_filter_string(raw).unwrap();
    let condition = expr_to_condition::<ent::Entity>(parsed.as_expr(), &field_map()).unwrap();
    ent::Entity::find()
        .filter(condition)
        .build(backend)
        .to_string()
}

const DIALECTS: [DbBackend; 2] = [DbBackend::Postgres, DbBackend::Sqlite];

/// `eq null` / `ne null` render as `IS NULL` / `IS NOT NULL`.
#[test]
fn null_equality_renders_is_null_and_is_not_null() {
    for backend in DIALECTS {
        let eq = typed_sql("parent eq null", backend);
        assert!(eq.contains(r#""parent" IS NULL"#), "{backend:?}: {eq}");
        let ne = typed_sql("parent ne null", backend);
        assert!(ne.contains(r#""parent" IS NOT NULL"#), "{backend:?}: {ne}");
        let both = typed_sql("startswith(name,'a') and parent eq null", backend);
        assert!(
            both.contains(r#""name" LIKE"#) && both.contains(r#""parent" IS NULL"#),
            "{backend:?}: {both}"
        );
    }
    // `name` and `id` always have a value: the typed parser refuses `null` on them.
    for raw in [
        "name ne null",
        "id eq null",
        "parent eq null or name eq null",
    ] {
        assert!(parse_odata_filter::<Field>(raw).is_err(), "{raw}");
    }
}

/// `contains`, `startswith` and `endswith` render a `LIKE` with `ESCAPE '\'` on both paths.
#[test]
fn every_string_function_carries_an_escape_clause() {
    for backend in DIALECTS {
        for raw in [
            "contains(name,'50%_off')",
            "startswith(name,'a_')",
            "endswith(name,'%')",
            "contains(name,'plain')",
        ] {
            for sql in [typed_sql(raw, backend), legacy_sql(raw, backend)] {
                assert!(sql.contains("LIKE"), "{backend:?} {raw}: {sql}");
                assert!(sql.contains(" ESCAPE "), "{backend:?} {raw}: {sql}");
            }
        }
    }
}

#[cfg(all(feature = "integration", any(feature = "sqlite", feature = "pg")))]
mod execution {
    use super::*;
    use crate::common;
    use anyhow::{Result, anyhow};
    use sea_orm::Set;
    use sea_orm_migration::prelude as mig;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::odata::LimitCfg;
    use toolkit_db::odata::pager::OPager;
    use toolkit_db::odata::paginate_odata;
    use toolkit_db::secure::{AccessScope, SecureEntityExt, secure_insert};
    use toolkit_odata::{ODataQuery, SortDir};
    use uuid::Uuid;

    struct CreateTable;

    impl mig::MigrationName for CreateTable {
        fn name(&self) -> &'static str {
            "m001_create_odata_null_like"
        }
    }

    #[async_trait::async_trait]
    impl mig::MigrationTrait for CreateTable {
        async fn up(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
            manager
                .create_table(
                    mig::Table::create()
                        .table(mig::Alias::new("odata_null_like"))
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
                        .col(mig::ColumnDef::new(mig::Alias::new("parent")).uuid().null())
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
            manager
                .drop_table(
                    mig::Table::drop()
                        .table(mig::Alias::new("odata_null_like"))
                        .to_owned(),
                )
                .await
        }
    }

    /// Names seeded with `(has a parent)`: every wildcard and the escape character appear
    /// literally in one name and are matched by another name only as a wildcard would be.
    const ROWS: [(&str, bool); 7] = [
        ("100%", true),
        ("100x", false),
        ("a_b", true),
        ("axb", false),
        (r"back\slash", true),
        ("backXslash", false),
        ("plain", false),
    ];

    /// Returns the names sorted, for order-insensitive comparison.
    fn sorted(mut names: Vec<String>) -> Vec<String> {
        names.sort();
        names
    }

    /// Runs the null-equality and literal-LIKE queries against the database at `url`.
    async fn run_suite(url: &str) -> Result<()> {
        let db = toolkit_db::connect_db(url, toolkit_db::ConnectOpts::default()).await?;
        run_migrations_for_testing(&db, vec![Box::new(CreateTable)])
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenants(vec![tenant]);
        let conn = db.conn()?;
        for (name, has_parent) in ROWS {
            let row = ent::ActiveModel {
                tenant_id: Set(tenant),
                name: Set(name.to_owned()),
                parent: Set(has_parent.then(Uuid::new_v4)),
                ..Default::default()
            };
            secure_insert::<ent::Entity>(row, &scope, &conn).await?;
        }

        let typed = |raw: &'static str| {
            let conn = db.conn().unwrap();
            let scope = scope.clone();
            async move {
                let parsed = toolkit_odata::parse_filter_string(raw).unwrap();
                let query = ODataQuery::default().with_filter(parsed.into_expr());
                let page = paginate_odata::<Field, Mapper, ent::Entity, String, _, _>(
                    ent::Entity::find().secure().scope_with(&scope),
                    &conn,
                    &query,
                    ("id", SortDir::Asc),
                    LimitCfg {
                        default: 50,
                        max: 50,
                    },
                    |m| m.name,
                )
                .await
                .unwrap_or_else(|e| panic!("{raw}: {e}"));
                sorted(page.items)
            }
        };
        let legacy = |raw: &'static str| {
            let conn = db.conn().unwrap();
            let scope = scope.clone();
            async move {
                let parsed = toolkit_odata::parse_filter_string(raw).unwrap();
                let query = ODataQuery::default().with_filter(parsed.into_expr());
                let page = OPager::<ent::Entity, _>::new(&scope, &conn, &field_map())
                    .fetch(&query, |m| m.name)
                    .await
                    .unwrap_or_else(|e| panic!("{raw}: {e}"));
                sorted(page.items)
            }
        };

        for (raw, expected) in [
            ("contains(name,'%')", vec!["100%"]),
            ("endswith(name,'%')", vec!["100%"]),
            ("startswith(name,'a_')", vec!["a_b"]),
            ("contains(name,'_')", vec!["a_b"]),
            (r"contains(name,'\')", vec![r"back\slash"]),
            (r"contains(name,'k\s')", vec![r"back\slash"]),
            ("startswith(name,'100')", vec!["100%", "100x"]),
            ("contains(name,'lai')", vec!["plain"]),
        ] {
            let expected: Vec<String> = sorted(expected.into_iter().map(str::to_owned).collect());
            assert_eq!(typed(raw).await, expected, "typed path: {raw}");
            assert_eq!(legacy(raw).await, expected, "legacy path: {raw}");
        }

        let with_parent = sorted(
            ROWS.iter()
                .filter(|(_, p)| *p)
                .map(|(n, _)| (*n).to_owned())
                .collect(),
        );
        let without_parent = sorted(
            ROWS.iter()
                .filter(|(_, p)| !*p)
                .map(|(n, _)| (*n).to_owned())
                .collect(),
        );
        assert_eq!(typed("parent ne null").await, with_parent);
        assert_eq!(typed("parent eq null").await, without_parent);
        assert_eq!(legacy("parent ne null").await, with_parent);
        assert_eq!(legacy("parent eq null").await, without_parent);
        assert_eq!(
            typed("parent eq null and startswith(name,'100')").await,
            vec!["100x".to_owned()]
        );
        assert_eq!(
            typed("not (parent eq null) and contains(name,'_')").await,
            vec!["a_b".to_owned()]
        );
        Ok(())
    }

    /// The suite on an in-memory `SQLite` database.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite() -> Result<()> {
        let dut = common::bring_up_sqlite();
        run_suite(&dut.url).await
    }

    /// The suite on Postgres (needs Docker).
    #[cfg(feature = "pg")]
    #[tokio::test]
    async fn postgres() -> Result<()> {
        let dut = common::bring_up_postgres().await?;
        run_suite(&dut.url).await
    }
}

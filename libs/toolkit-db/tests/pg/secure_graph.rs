#![allow(clippy::unwrap_used, clippy::expect_used)]

//! What a secure graph query *returns* on a real `PostgreSQL` 19 server.
//!
//! The unit tests assert what SQL is built. These assert what comes back, which
//! is the only way to show the isolation claim holds rather than merely renders.
//!
//! # The fixture is a trap
//!
//! Two tenants, and one edge that crosses between them. Every isolation test
//! here asserts its own precondition — that the crossing edge is actually in the
//! table — because a trap fixture that quietly disappears turns the test it
//! guards into a vacuous pass, and nothing about the test's output says so.
//!
//! # Observing the hole
//!
//! A test that only checks "the scoped query returns the right rows" cannot tell
//! a working guard from an absent one, because the outer query re-filters on its
//! own scope and masks a leaking element. So the suite also runs the *unscoped*
//! pattern — built straight from the syntax crate, which embeds no scope — over
//! a plain connection, and asserts it does return the foreign row. If that
//! control ever stops seeing the crossing edge, the guard tests below are no
//! longer evidence of anything.

use sea_orm::entity::prelude::*;
use sea_orm::{FromQueryResult as _, Set};
use sea_orm_migration::prelude as mig;
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::pgq::{Endpoint, GraphDeclaration, PropertyGraph, VertexOf};
use toolkit_db::secure::{Db, DbConn, ScopeError, SecureEntityExt, secure_insert};
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(0xA);
const TENANT_B: Uuid = Uuid::from_u128(0xB);

// ════════════════════════════════════════════════════════════════════
// Entities
// ════════════════════════════════════════════════════════════════════

mod node {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "pgq_node")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl toolkit_db::secure::ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            Some(Column::TenantId)
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            None
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            match property {
                p if p == toolkit_security::pep_properties::OWNER_TENANT_ID => {
                    Some(Column::TenantId)
                }
                p if p == toolkit_security::pep_properties::RESOURCE_ID => Some(Column::Id),
                _ => None,
            }
        }
    }
}

mod edge {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "pgq_edge")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub src_id: i64,
        pub dst_id: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl toolkit_db::secure::ScopableEntity for Entity {
        fn tenant_col() -> Option<Column> {
            Some(Column::TenantId)
        }
        fn resource_col() -> Option<Column> {
            Some(Column::Id)
        }
        fn owner_col() -> Option<Column> {
            None
        }
        fn type_col() -> Option<Column> {
            None
        }
        fn resolve_property(property: &str) -> Option<Column> {
            match property {
                p if p == toolkit_security::pep_properties::OWNER_TENANT_ID => {
                    Some(Column::TenantId)
                }
                p if p == toolkit_security::pep_properties::RESOURCE_ID => Some(Column::Id),
                _ => None,
            }
        }
    }
}

struct Kb;

impl PropertyGraph for Kb {
    const GRAPH_NAME: &'static str = "pgq_kb";

    fn declaration() -> Result<GraphDeclaration, ScopeError> {
        GraphDeclaration::new::<Self>()
            .vertex::<Self, node::Entity>(&["tenant_id", "id"])?
            .edge::<Self, edge::Entity>(
                &["tenant_id", "id"],
                Endpoint {
                    key: vec!["tenant_id".to_owned(), "src_id".to_owned()],
                    table: "pgq_node".to_owned(),
                    references: vec!["tenant_id".to_owned(), "id".to_owned()],
                },
                Endpoint {
                    key: vec!["tenant_id".to_owned(), "dst_id".to_owned()],
                    table: "pgq_node".to_owned(),
                    references: vec!["tenant_id".to_owned(), "id".to_owned()],
                },
            )
    }
}

impl VertexOf<Kb> for node::Entity {
    const LABEL: &'static str = "node";
}

impl toolkit_db::secure::pgq::EdgeOf<Kb> for edge::Entity {
    const LABEL: &'static str = "edge";
}

// ════════════════════════════════════════════════════════════════════
// Schema
// ════════════════════════════════════════════════════════════════════

struct CreateTables;

impl mig::MigrationName for CreateTables {
    fn name(&self) -> &'static str {
        "pgq_create_tables"
    }
}

#[async_trait::async_trait]
impl mig::MigrationTrait for CreateTables {
    async fn up(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .create_table(
                mig::Table::create()
                    .table(mig::Alias::new("pgq_node"))
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("tenant_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("name"))
                            .string()
                            .not_null(),
                    )
                    .primary_key(
                        mig::Index::create()
                            .col(mig::Alias::new("tenant_id"))
                            .col(mig::Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                mig::Table::create()
                    .table(mig::Alias::new("pgq_edge"))
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("tenant_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("src_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("dst_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        mig::Index::create()
                            .col(mig::Alias::new("tenant_id"))
                            .col(mig::Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;

        // The property graph is a schema object, and the DDL is the *generated*
        // one — so this statement is also the assertion that what the declaration
        // emits is what PostgreSQL accepts. Raw SQL is the established idiom for
        // statements sea-orm-migration cannot model.
        let ddl = Kb::declaration()
            .map_err(|e| mig::DbErr::Custom(format!("declaration: {e}")))?
            .create_statement()
            .map_err(|e| mig::DbErr::Custom(format!("ddl: {e}")))?;
        manager.get_connection().execute_unprepared(&ddl).await?;
        Ok(())
    }

    async fn down(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        let drop = Kb::declaration()
            .map_err(|e| mig::DbErr::Custom(format!("declaration: {e}")))?
            .drop_statement()
            .map_err(|e| mig::DbErr::Custom(format!("ddl: {e}")))?;
        manager.get_connection().execute_unprepared(&drop).await?;
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════
// Harness
// ════════════════════════════════════════════════════════════════════

struct Stand {
    db: Db,
    dsn: String,
    _container: ContainerAsync<Postgres>,
}

/// Bring up `PostgreSQL` 19, or explain why not.
///
/// `None` means skip. While the image is pre-GA that is the default;
/// `GEARS_TEST_PG_GRAPH_REQUIRED=1` makes an unavailable image a failure, so a
/// lane that is supposed to cover PG19 cannot pass by skipping.
async fn stand() -> Option<Stand> {
    let request = cf_gears_test_containers::postgres_graph()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");

    let container = match request.start().await {
        Ok(container) => container,
        Err(error) => {
            assert!(
                !cf_gears_test_containers::graph_lane_required(),
                "GEARS_TEST_PG_GRAPH_REQUIRED is set but PostgreSQL 19 \
                 ({}) could not start: {error}",
                cf_gears_test_containers::postgres_graph_tag()
            );
            eprintln!("PostgreSQL 19 unavailable - skipping the SQL/PGQ lane: {error}");
            return None;
        }
    };

    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    run_migrations_for_testing(&db, vec![Box::new(CreateTables)])
        .await
        .expect("migrate");

    Some(Stand {
        db,
        dsn,
        _container: container,
    })
}

impl Stand {
    fn conn(&self) -> DbConn<'_> {
        self.db.conn().expect("conn")
    }

    /// Two tenants, and one edge that crosses between them.
    ///
    /// Tenant A: 1 -> 2. Tenant B: 10 -> 11. The trap is an edge row owned by A
    /// whose destination id exists only in B.
    async fn seed(&self) {
        let conn = self.conn();
        let unrestricted = AccessScope::allow_all();

        for (tenant, id, name) in [
            (TENANT_A, 1_i64, "a1"),
            (TENANT_A, 2, "a2"),
            (TENANT_B, 10, "b10"),
            (TENANT_B, 11, "b11"),
        ] {
            secure_insert::<node::Entity>(
                node::ActiveModel {
                    tenant_id: Set(tenant),
                    id: Set(id),
                    name: Set(name.to_owned()),
                },
                &unrestricted,
                &conn,
            )
            .await
            .expect("insert node");
        }

        for (tenant, id, src, dst) in [
            (TENANT_A, 100_i64, 1_i64, 2_i64),
            (TENANT_B, 200, 10, 11),
            // The trap: an edge in A pointing at an id that only B has. The
            // composite endpoint key makes it unresolvable inside the graph,
            // which is itself part of what these tests check.
            (TENANT_A, 300, 1, 11),
        ] {
            secure_insert::<edge::Entity>(
                edge::ActiveModel {
                    tenant_id: Set(tenant),
                    id: Set(id),
                    src_id: Set(src),
                    dst_id: Set(dst),
                },
                &unrestricted,
                &conn,
            )
            .await
            .expect("insert edge");
        }
    }

    /// A plain connection, for the deliberately unscoped control query only.
    async fn raw(&self) -> sea_orm::DatabaseConnection {
        sea_orm::Database::connect(&self.dsn)
            .await
            .expect("raw connect")
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Neighbour {
    neighbour: i64,
}

// ════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════

/// The precondition every isolation test below rests on: the crossing edge is
/// really in the table. Asserted separately so that a fixture which loses it
/// fails here, loudly, instead of turning the guards into vacuous passes.
#[tokio::test]
async fn the_trap_edge_exists() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    let conn = stand.conn();
    let crossing = edge::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            sea_orm::Condition::all()
                .add(edge::Column::TenantId.eq(TENANT_A))
                .add(edge::Column::DstId.eq(11_i64)),
        )
        .all(&conn)
        .await
        .expect("query");

    assert_eq!(
        crossing.len(),
        1,
        "the cross-tenant trap edge is missing; the isolation tests below would \
         pass without proving anything"
    );
}

/// A scoped hop stays inside the caller's tenant.
#[tokio::test]
async fn a_scoped_hop_does_not_leave_the_tenant() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    let conn = stand.conn();
    let rows: Vec<Neighbour> = node::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(TENANT_A))
        .with_graph::<Kb>()
        .match_path(|p| {
            p.vertex::<node::Entity>("a")
                .edge_to::<edge::Entity>("e")
                .to::<node::Entity>("b")
        })
        .column("b", "id", "neighbour")
        .all_as(&conn)
        .await
        .expect("graph query");

    let mut ids: Vec<i64> = rows.into_iter().map(|r| r.neighbour).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids, vec![2], "tenant A's only reachable neighbour is 2");
}

/// Tenant B sees its own graph and nothing of A's.
#[tokio::test]
async fn each_tenant_sees_only_its_own_graph() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    let conn = stand.conn();
    let rows: Vec<Neighbour> = node::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(TENANT_B))
        .with_graph::<Kb>()
        .match_path(|p| {
            p.vertex::<node::Entity>("a")
                .edge_to::<edge::Entity>("e")
                .to::<node::Entity>("b")
        })
        .column("b", "id", "neighbour")
        .all_as(&conn)
        .await
        .expect("graph query");

    let mut ids: Vec<i64> = rows.into_iter().map(|r| r.neighbour).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids, vec![11]);
}

/// A deny-all scope returns nothing rather than everything.
#[tokio::test]
async fn deny_all_returns_no_rows() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    let conn = stand.conn();
    let rows: Vec<Neighbour> = node::Entity::find()
        .secure()
        .scope_with(&AccessScope::deny_all())
        .with_graph::<Kb>()
        .match_path(|p| {
            p.vertex::<node::Entity>("a")
                .edge_to::<edge::Entity>("e")
                .to::<node::Entity>("b")
        })
        .column("b", "id", "neighbour")
        .all_as(&conn)
        .await
        .expect("graph query");

    assert!(rows.is_empty(), "deny-all must return nothing: {rows:?}");
}

/// The control: **can this suite see a hole at all?**
///
/// The same pattern with no scope anywhere, built from the syntax crate (which
/// embeds none) and run over a plain connection. Unscoped, tenant A's crossing
/// edge is resolvable, so the walk reaches a node A does not own — and the row
/// count says so. If this ever returns the same rows as the scoped query, the
/// tests above have stopped being evidence.
#[tokio::test]
async fn the_unscoped_pattern_does_reach_across_tenants() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    let unscoped = toolkit_sea_orm_pgq::GraphTable::new(
        Kb::GRAPH_NAME,
        toolkit_sea_orm_pgq::GraphPattern::new(toolkit_sea_orm_pgq::Element::new("a", "node")).hop(
            toolkit_sea_orm_pgq::Element::new("e", "edge"),
            toolkit_sea_orm_pgq::Direction::Outgoing,
            toolkit_sea_orm_pgq::Element::new("b", "node"),
        ),
    )
    .column(toolkit_sea_orm_pgq::ProjectedColumn::new(
        "b",
        "id",
        "neighbour",
    ));

    let statement = sea_orm::sea_query::Query::select()
        .expr_as(
            sea_orm::sea_query::Expr::col((
                sea_orm::sea_query::Alias::new("g"),
                sea_orm::sea_query::Alias::new("neighbour"),
            )),
            sea_orm::sea_query::Alias::new("neighbour"),
        )
        .from(unscoped.into_table_ref("g").expect("renders"))
        .to_owned();
    let stmt = sea_orm::StatementBuilder::build(&statement, &sea_orm::DbBackend::Postgres);

    let raw = stand.raw().await;
    let rows = Neighbour::find_by_statement(stmt)
        .all(&raw)
        .await
        .expect("unscoped query");

    let mut ids: Vec<i64> = rows.into_iter().map(|r| r.neighbour).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids,
        vec![2, 11],
        "the unscoped pattern must reach both tenants' nodes; if it does not, \
         the isolation tests above prove nothing"
    );
}

/// A column absent from the declaration's `PROPERTIES` list is invisible to
/// `MATCH` — the quiet failure Policy 3 exists to prevent. Here it is made
/// loud: the server refuses the pattern rather than silently ignoring it.
#[tokio::test]
async fn a_column_outside_properties_cannot_be_matched_on() {
    let Some(stand) = stand().await else { return };
    stand.seed().await;

    // `name` is a real column of pgq_node and is deliberately not exposed: the
    // declaration lists only the key and the scope columns.
    let declaration = Kb::declaration().expect("declares");
    let exposed = declaration.properties_of("node").expect("node");
    assert!(
        !exposed.contains(&"name".to_owned()),
        "this test needs an unexposed column: {exposed:?}"
    );

    let table = toolkit_sea_orm_pgq::GraphTable::new(
        Kb::GRAPH_NAME,
        toolkit_sea_orm_pgq::GraphPattern::new(toolkit_sea_orm_pgq::Element::new("a", "node")),
    )
    .column(toolkit_sea_orm_pgq::ProjectedColumn::new("a", "name", "n"));

    let statement = sea_orm::sea_query::Query::select()
        .expr(sea_orm::sea_query::Expr::val(1))
        .from(table.into_table_ref("g").expect("renders"))
        .to_owned();
    let raw = stand.raw().await;
    let outcome = raw.query_all(&statement).await;
    assert!(
        outcome.is_err(),
        "projecting an unexposed column must be refused, not silently answered"
    );
}

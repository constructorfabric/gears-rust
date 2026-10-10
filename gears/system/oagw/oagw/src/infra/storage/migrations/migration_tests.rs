//! Schema tests for the OAGW migration, on `SQLite` and (behind the
//! `integration` feature, Docker required) `PostgreSQL` and `MySQL`.
//!
//! Rows are written and read through the secure ORM only; errors are
//! classified with the backend-neutral `ScopeError` helpers, never by message.
//! Every case draws fresh tenant and row IDs, so the `PostgreSQL` and `MySQL`
//! runs execute all cases against one migrated database.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::Db;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{
    AccessScope, ScopableEntity, ScopeError, SecureDeleteExt, SecureEntityExt, secure_insert,
};
use uuid::Uuid;

use super::Migrator;
use crate::infra::storage::entity::{
    route, route_grpc_match, route_http_match, route_method, route_plugin, route_tag, upstream,
    upstream_plugin, upstream_tag,
};
use crate::infra::storage::testing::migrated;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Private in-memory `SQLite`, one connection (a second would see an empty
/// database).
async fn sqlite() -> Db {
    migrated("sqlite::memory:", 1).await
}

async fn insert<E>(db: &Db, tenant_id: Uuid, am: E::ActiveModel) -> Result<(), ScopeError>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
    E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    E::Model: sea_orm::IntoActiveModel<E::ActiveModel>,
{
    let conn = db.conn().expect("conn");
    secure_insert::<E>(am, &AccessScope::for_tenant(tenant_id), &conn)
        .await
        .map(|_| ())
}

/// The tenant's rows of `E`.
async fn rows<E>(db: &Db, tenant_id: Uuid) -> Vec<E::Model>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    let conn = db.conn().expect("conn");
    E::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant_id))
        .all(&conn)
        .await
        .expect("scoped select")
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn upstream_row(id: Uuid, tenant_id: Uuid, alias: &str) -> upstream::ActiveModel {
    let now = OffsetDateTime::now_utc();
    upstream::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        alias: Set(alias.to_owned()),
        protocol: Set("gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1".to_owned()),
        enabled: Set(true),
        schema_version: Set(1),
        server: Set(serde_json::json!({
            "endpoints": [{"scheme": "https", "host": "api.example.com", "port": 443}]
        })),
        auth_plugin_ref: Set(None),
        auth_plugin_uuid: Set(None),
        auth_config: Set(None),
        auth_sharing: Set("private".to_owned()),
        headers: Set(None),
        cors: Set(None),
        cors_sharing: Set("private".to_owned()),
        rate_limit: Set(None),
        rate_limit_sharing: Set("private".to_owned()),
        plugins_sharing: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        managed_by: Set("api".into()),
    }
}

fn route_row(id: Uuid, tenant_id: Uuid, upstream_id: Uuid, match_type: &str) -> route::ActiveModel {
    let now = OffsetDateTime::now_utc();
    route::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        upstream_id: Set(upstream_id),
        enabled: Set(true),
        priority: Set(0),
        match_type: Set(match_type.to_owned()),
        schema_version: Set(1),
        match_config: Set(None),
        cors: Set(None),
        rate_limit: Set(None),
        rate_limit_sharing: Set("private".to_owned()),
        plugins_sharing: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        managed_by: Set("api".into()),
    }
}

fn upstream_tag_row(upstream_id: Uuid, tenant_id: Uuid) -> upstream_tag::ActiveModel {
    upstream_tag::ActiveModel {
        upstream_id: Set(upstream_id),
        tenant_id: Set(tenant_id),
        tag: Set("payments".to_owned()),
    }
}

fn upstream_plugin_row(upstream_id: Uuid, tenant_id: Uuid) -> upstream_plugin::ActiveModel {
    upstream_plugin::ActiveModel {
        upstream_id: Set(upstream_id),
        tenant_id: Set(tenant_id),
        position: Set(0),
        plugin_ref: Set("gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1".to_owned()),
        plugin_uuid: Set(None),
        schema_version: Set(1),
        config: Set(Some(serde_json::json!({}))),
    }
}

fn route_http_match_row(route_id: Uuid, tenant_id: Uuid) -> route_http_match::ActiveModel {
    route_http_match::ActiveModel {
        route_id: Set(route_id),
        tenant_id: Set(tenant_id),
        path_prefix: Set("/v1/charges".to_owned()),
    }
}

fn route_method_row(route_id: Uuid, tenant_id: Uuid, method: &str) -> route_method::ActiveModel {
    route_method::ActiveModel {
        route_id: Set(route_id),
        tenant_id: Set(tenant_id),
        method: Set(method.to_owned()),
    }
}

fn route_grpc_match_row(route_id: Uuid, tenant_id: Uuid) -> route_grpc_match::ActiveModel {
    route_grpc_match::ActiveModel {
        route_id: Set(route_id),
        tenant_id: Set(tenant_id),
        service: Set("payments.v1.Charges".to_owned()),
        method: Set("Create".to_owned()),
    }
}

fn route_tag_row(route_id: Uuid, tenant_id: Uuid) -> route_tag::ActiveModel {
    route_tag::ActiveModel {
        route_id: Set(route_id),
        tenant_id: Set(tenant_id),
        tag: Set("charges".to_owned()),
    }
}

fn route_plugin_row(route_id: Uuid, tenant_id: Uuid) -> route_plugin::ActiveModel {
    route_plugin::ActiveModel {
        route_id: Set(route_id),
        tenant_id: Set(tenant_id),
        position: Set(0),
        plugin_ref: Set(
            "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1".to_owned(),
        ),
        plugin_uuid: Set(None),
        schema_version: Set(1),
        config: Set(Some(serde_json::json!({}))),
    }
}

/// An upstream with one tag and one plugin, an HTTP route (match, two methods,
/// tag, plugin) and a gRPC route: at least one row in each of the nine tables.
async fn seed_upstream_tree(db: &Db, tenant_id: Uuid, alias: &str) -> Uuid {
    let upstream_id = Uuid::new_v4();
    let http_route = Uuid::new_v4();
    let grpc_route = Uuid::new_v4();
    insert::<upstream::Entity>(db, tenant_id, upstream_row(upstream_id, tenant_id, alias))
        .await
        .expect("upstream");
    insert::<upstream_tag::Entity>(db, tenant_id, upstream_tag_row(upstream_id, tenant_id))
        .await
        .expect("upstream tag");
    insert::<upstream_plugin::Entity>(db, tenant_id, upstream_plugin_row(upstream_id, tenant_id))
        .await
        .expect("upstream plugin");
    insert::<route::Entity>(
        db,
        tenant_id,
        route_row(http_route, tenant_id, upstream_id, "http"),
    )
    .await
    .expect("http route");
    insert::<route_http_match::Entity>(db, tenant_id, route_http_match_row(http_route, tenant_id))
        .await
        .expect("http match");
    for method in ["GET", "POST"] {
        insert::<route_method::Entity>(
            db,
            tenant_id,
            route_method_row(http_route, tenant_id, method),
        )
        .await
        .expect("route method");
    }
    insert::<route_tag::Entity>(db, tenant_id, route_tag_row(http_route, tenant_id))
        .await
        .expect("route tag");
    insert::<route_plugin::Entity>(db, tenant_id, route_plugin_row(http_route, tenant_id))
        .await
        .expect("route plugin");
    insert::<route::Entity>(
        db,
        tenant_id,
        route_row(grpc_route, tenant_id, upstream_id, "grpc"),
    )
    .await
    .expect("grpc route");
    insert::<route_grpc_match::Entity>(db, tenant_id, route_grpc_match_row(grpc_route, tenant_id))
        .await
        .expect("grpc match");
    upstream_id
}

/// Row counts of the tenant in all nine tables, in schema order.
async fn table_counts(db: &Db, tenant_id: Uuid) -> [usize; 9] {
    [
        rows::<upstream::Entity>(db, tenant_id).await.len(),
        rows::<upstream_tag::Entity>(db, tenant_id).await.len(),
        rows::<upstream_plugin::Entity>(db, tenant_id).await.len(),
        rows::<route::Entity>(db, tenant_id).await.len(),
        rows::<route_http_match::Entity>(db, tenant_id).await.len(),
        rows::<route_method::Entity>(db, tenant_id).await.len(),
        rows::<route_grpc_match::Entity>(db, tenant_id).await.len(),
        rows::<route_tag::Entity>(db, tenant_id).await.len(),
        rows::<route_plugin::Entity>(db, tenant_id).await.len(),
    ]
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

/// Invoke `$m!` with every entity module, one per migrated table.
macro_rules! with_entities {
    ($m:ident) => {
        $m!(
            upstream,
            upstream_tag,
            upstream_plugin,
            route,
            route_http_match,
            route_method,
            route_grpc_match,
            route_tag,
            route_plugin,
        )
    };
}

/// Each entity is tenant-scoped and its table answers a tenant-filtered select
/// of every mapped column, which proves the table and its `tenant_id` exist.
async fn case_all_tables_exist_with_tenant_id(db: &Db) {
    macro_rules! assert_tenant_table {
        ($($entity:ident),+ $(,)?) => {{
            $(
                assert!(
                    matches!(
                        <$entity::Entity as ScopableEntity>::tenant_col(),
                        Some($entity::Column::TenantId)
                    ),
                    "{} is tenant-scoped on tenant_id",
                    stringify!($entity),
                );
                assert!(
                    rows::<$entity::Entity>(db, Uuid::new_v4()).await.is_empty(),
                    "{} starts empty for a new tenant",
                    stringify!($entity),
                );
            )+
        }};
    }
    with_entities!(assert_tenant_table);
}

/// Values at the stored field limits (`management::validation`) fit their
/// sized columns, and read back unchanged. ASCII is the worst case for a
/// column sized in characters.
async fn case_values_at_the_field_limits_fit(db: &Db) {
    let tenant_id = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let http_route = Uuid::new_v4();
    let grpc_route = Uuid::new_v4();
    let text = |c: char, n: usize| c.to_string().repeat(n);
    let mut path = text('p', 2048);
    path.replace_range(..1, "/");

    let mut up = upstream_row(upstream_id, tenant_id, &text('a', 253));
    up.protocol = Set(text('t', 256));
    up.auth_plugin_ref = Set(Some(text('u', 256)));
    insert::<upstream::Entity>(db, tenant_id, up)
        .await
        .expect("upstream");
    let mut tag = upstream_tag_row(upstream_id, tenant_id);
    tag.tag = Set(text('g', 128));
    insert::<upstream_tag::Entity>(db, tenant_id, tag)
        .await
        .expect("upstream tag");
    let mut plugin = upstream_plugin_row(upstream_id, tenant_id);
    plugin.plugin_ref = Set(text('r', 256));
    insert::<upstream_plugin::Entity>(db, tenant_id, plugin)
        .await
        .expect("upstream plugin");

    insert::<route::Entity>(
        db,
        tenant_id,
        route_row(http_route, tenant_id, upstream_id, "http"),
    )
    .await
    .expect("http route");
    let mut http = route_http_match_row(http_route, tenant_id);
    http.path_prefix = Set(path.clone());
    insert::<route_http_match::Entity>(db, tenant_id, http)
        .await
        .expect("http match");
    let mut tag = route_tag_row(http_route, tenant_id);
    tag.tag = Set(text('h', 128));
    insert::<route_tag::Entity>(db, tenant_id, tag)
        .await
        .expect("route tag");
    let mut plugin = route_plugin_row(http_route, tenant_id);
    plugin.plugin_ref = Set(text('q', 256));
    insert::<route_plugin::Entity>(db, tenant_id, plugin)
        .await
        .expect("route plugin");
    insert::<route::Entity>(
        db,
        tenant_id,
        route_row(grpc_route, tenant_id, upstream_id, "grpc"),
    )
    .await
    .expect("grpc route");
    let mut grpc = route_grpc_match_row(grpc_route, tenant_id);
    grpc.service = Set(text('s', 256));
    grpc.method = Set(text('m', 256));
    insert::<route_grpc_match::Entity>(db, tenant_id, grpc)
        .await
        .expect("grpc match");

    let up = &rows::<upstream::Entity>(db, tenant_id).await[0];
    assert_eq!((up.alias.len(), up.protocol.len()), (253, 256));
    assert_eq!(up.auth_plugin_ref.as_deref().map(str::len), Some(256));
    assert_eq!(
        rows::<upstream_tag::Entity>(db, tenant_id).await[0]
            .tag
            .len(),
        128
    );
    assert_eq!(
        rows::<upstream_plugin::Entity>(db, tenant_id).await[0]
            .plugin_ref
            .len(),
        256
    );
    assert_eq!(
        rows::<route_http_match::Entity>(db, tenant_id).await[0].path_prefix,
        path
    );
    assert_eq!(
        rows::<route_tag::Entity>(db, tenant_id).await[0].tag.len(),
        128
    );
    assert_eq!(
        rows::<route_plugin::Entity>(db, tenant_id).await[0]
            .plugin_ref
            .len(),
        256
    );
    let grpc = &rows::<route_grpc_match::Entity>(db, tenant_id).await[0];
    assert_eq!((grpc.service.len(), grpc.method.len()), (256, 256));
}

/// A JSON document over 64 KiB fits every JSON column and reads back
/// unchanged. Nothing bounds these documents below the request body limit,
/// so no backend's JSON type may cap them there (`MySQL`'s `text` would).
async fn case_json_documents_over_64_kib_fit(db: &Db) {
    let tenant_id = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let route_id = Uuid::new_v4();
    let doc = serde_json::json!({ "k": "x".repeat(70 * 1024) });

    let mut up = upstream_row(upstream_id, tenant_id, "large-json");
    up.server = Set(doc.clone());
    up.auth_config = Set(Some(doc.clone()));
    up.headers = Set(Some(doc.clone()));
    up.cors = Set(Some(doc.clone()));
    up.rate_limit = Set(Some(doc.clone()));
    insert::<upstream::Entity>(db, tenant_id, up)
        .await
        .expect("upstream");
    let mut plugin = upstream_plugin_row(upstream_id, tenant_id);
    plugin.config = Set(Some(doc.clone()));
    insert::<upstream_plugin::Entity>(db, tenant_id, plugin)
        .await
        .expect("upstream plugin");
    let mut route = route_row(route_id, tenant_id, upstream_id, "http");
    route.match_config = Set(Some(doc.clone()));
    route.cors = Set(Some(doc.clone()));
    route.rate_limit = Set(Some(doc.clone()));
    insert::<route::Entity>(db, tenant_id, route)
        .await
        .expect("route");
    let mut plugin = route_plugin_row(route_id, tenant_id);
    plugin.config = Set(Some(doc.clone()));
    insert::<route_plugin::Entity>(db, tenant_id, plugin)
        .await
        .expect("route plugin");

    let up = &rows::<upstream::Entity>(db, tenant_id).await[0];
    assert_eq!(up.server, doc);
    for (column, value) in [
        ("auth_config", &up.auth_config),
        ("headers", &up.headers),
        ("cors", &up.cors),
        ("rate_limit", &up.rate_limit),
    ] {
        assert_eq!(value.as_ref(), Some(&doc), "oagw_upstream.{column}");
    }
    let route = &rows::<route::Entity>(db, tenant_id).await[0];
    for (column, value) in [
        ("match_config", &route.match_config),
        ("cors", &route.cors),
        ("rate_limit", &route.rate_limit),
    ] {
        assert_eq!(value.as_ref(), Some(&doc), "oagw_route.{column}");
    }
    assert_eq!(
        rows::<upstream_plugin::Entity>(db, tenant_id).await[0].config,
        Some(doc.clone()),
        "oagw_upstream_plugin.config"
    );
    assert_eq!(
        rows::<route_plugin::Entity>(db, tenant_id).await[0].config,
        Some(doc),
        "oagw_route_plugin.config"
    );
}

/// Timestamps read back to the microsecond, past 2038. `MySQL`'s `timestamp`
/// type would keep whole seconds and reject the year 2100.
async fn case_timestamps_keep_microseconds_past_2038(db: &Db) {
    let tenant_id = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let route_id = Uuid::new_v4();
    // 2100-01-01T00:00:00.123456Z, and an update 1.654321 s later.
    let created = OffsetDateTime::from_unix_timestamp(4_102_444_800).expect("valid timestamp")
        + time::Duration::microseconds(123_456);
    let updated = created + time::Duration::microseconds(1_654_321);

    let mut up = upstream_row(upstream_id, tenant_id, "dates");
    up.created_at = Set(created);
    up.updated_at = Set(updated);
    insert::<upstream::Entity>(db, tenant_id, up)
        .await
        .expect("upstream");
    let mut route = route_row(route_id, tenant_id, upstream_id, "http");
    route.created_at = Set(created);
    route.updated_at = Set(updated);
    insert::<route::Entity>(db, tenant_id, route)
        .await
        .expect("route");

    let up = &rows::<upstream::Entity>(db, tenant_id).await[0];
    assert_eq!((up.created_at, up.updated_at), (created, updated));
    let route = &rows::<route::Entity>(db, tenant_id).await[0];
    assert_eq!((route.created_at, route.updated_at), (created, updated));
}

async fn case_duplicate_tenant_alias_is_unique_violation(db: &Db) {
    let tenant = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    let first = Uuid::new_v4();
    insert::<upstream::Entity>(db, tenant, upstream_row(first, tenant, "billing"))
        .await
        .expect("first alias");

    let err =
        insert::<upstream::Entity>(db, tenant, upstream_row(Uuid::new_v4(), tenant, "billing"))
            .await
            .expect_err("same alias in the same tenant");
    assert!(err.is_unique_violation(), "unique violation, got: {err}");

    insert::<upstream::Entity>(
        db,
        other_tenant,
        upstream_row(Uuid::new_v4(), other_tenant, "billing"),
    )
    .await
    .expect("same alias in another tenant");

    let stored = rows::<upstream::Entity>(db, tenant).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, first);
}

/// For each of the eight child foreign keys: a row naming tenant B under a
/// parent of tenant A is rejected by the database (the scope check passes,
/// since the row's own tenant is in scope), and the same row with tenant A is
/// accepted, so the rejection is the tenant mismatch and nothing else.
async fn case_cross_tenant_child_rows_are_fk_violations(db: &Db) {
    let owner = Uuid::new_v4();
    let intruder = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let route_id = Uuid::new_v4();
    insert::<upstream::Entity>(db, owner, upstream_row(upstream_id, owner, "ledger"))
        .await
        .expect("parent upstream");
    insert::<route::Entity>(db, owner, route_row(route_id, owner, upstream_id, "http"))
        .await
        .expect("parent route");

    async fn check<E>(
        db: &Db,
        fk: &str,
        owner: Uuid,
        intruder: Uuid,
        row: impl Fn(Uuid) -> E::ActiveModel,
    ) where
        E: ScopableEntity + EntityTrait,
        E::Column: ColumnTrait + Copy,
        E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
        E::Model: sea_orm::IntoActiveModel<E::ActiveModel>,
    {
        let err = insert::<E>(db, intruder, row(intruder))
            .await
            .expect_err(&format!("{fk}: cross-tenant row must be rejected"));
        assert!(
            err.is_foreign_key_violation(),
            "{fk}: foreign key violation, got: {err}"
        );
        assert!(
            rows::<E>(db, intruder).await.is_empty(),
            "{fk}: nothing stored"
        );
        insert::<E>(db, owner, row(owner))
            .await
            .unwrap_or_else(|e| panic!("{fk}: same-tenant row is accepted: {e}"));
    }

    check::<upstream_tag::Entity>(db, "upstream_tag → upstream", owner, intruder, |t| {
        upstream_tag_row(upstream_id, t)
    })
    .await;
    check::<upstream_plugin::Entity>(db, "upstream_plugin → upstream", owner, intruder, |t| {
        upstream_plugin_row(upstream_id, t)
    })
    .await;
    check::<route::Entity>(db, "route → upstream", owner, intruder, |t| {
        route_row(Uuid::new_v4(), t, upstream_id, "grpc")
    })
    .await;
    check::<route_http_match::Entity>(db, "route_http_match → route", owner, intruder, |t| {
        route_http_match_row(route_id, t)
    })
    .await;
    check::<route_method::Entity>(db, "route_method → route", owner, intruder, |t| {
        route_method_row(route_id, t, "GET")
    })
    .await;
    check::<route_grpc_match::Entity>(db, "route_grpc_match → route", owner, intruder, |t| {
        route_grpc_match_row(route_id, t)
    })
    .await;
    check::<route_tag::Entity>(db, "route_tag → route", owner, intruder, |t| {
        route_tag_row(route_id, t)
    })
    .await;
    check::<route_plugin::Entity>(db, "route_plugin → route", owner, intruder, |t| {
        route_plugin_row(route_id, t)
    })
    .await;
}

/// Deleting an upstream removes its routes and the rows of all seven child
/// tables (two levels of cascade); a sibling upstream's tree is untouched. A
/// delete scoped to another tenant affects nothing.
async fn case_upstream_delete_cascades(db: &Db) {
    let tenant = Uuid::new_v4();
    let doomed = seed_upstream_tree(db, tenant, "doomed").await;
    let survivor = seed_upstream_tree(db, tenant, "survivor").await;
    let one_tree = [1, 1, 1, 2, 1, 2, 1, 1, 1];
    let two_trees = one_tree.map(|n| n * 2);
    assert_eq!(table_counts(db, tenant).await, two_trees, "seeded");

    let conn = db.conn().expect("conn");
    let foreign = upstream::Entity::delete_many()
        .filter(upstream::Column::Id.eq(doomed))
        .secure()
        .scope_with(&AccessScope::for_tenant(Uuid::new_v4()))
        .exec(&conn)
        .await
        .expect("delete under a foreign scope");
    assert_eq!(foreign.rows_affected, 0);
    assert_eq!(
        table_counts(db, tenant).await,
        two_trees,
        "foreign delete is a no-op"
    );

    let deleted = upstream::Entity::delete_many()
        .filter(upstream::Column::Id.eq(doomed))
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .exec(&conn)
        .await
        .expect("delete upstream");
    assert_eq!(deleted.rows_affected, 1);
    assert_eq!(
        table_counts(db, tenant).await,
        one_tree,
        "doomed tree removed"
    );

    let routes = rows::<route::Entity>(db, tenant).await;
    assert!(routes.iter().all(|r| r.upstream_id == survivor));
    let route_ids: Vec<Uuid> = routes.iter().map(|r| r.id).collect();
    assert!(
        rows::<upstream_tag::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| r.upstream_id == survivor)
    );
    assert!(
        rows::<upstream_plugin::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| r.upstream_id == survivor)
    );
    assert!(
        rows::<route_http_match::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| route_ids.contains(&r.route_id))
    );
    assert!(
        rows::<route_method::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| route_ids.contains(&r.route_id))
    );
    assert!(
        rows::<route_grpc_match::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| route_ids.contains(&r.route_id))
    );
    assert!(
        rows::<route_tag::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| route_ids.contains(&r.route_id))
    );
    assert!(
        rows::<route_plugin::Entity>(db, tenant)
            .await
            .iter()
            .all(|r| route_ids.contains(&r.route_id))
    );
}

/// A restart re-runs the migrations: nothing is applied and stored rows stay.
async fn case_migrating_twice_is_a_no_op(db: &Db) {
    let tenant = Uuid::new_v4();
    seed_upstream_tree(db, tenant, "kept").await;
    let before = table_counts(db, tenant).await;

    let result = run_migrations_for_testing(db, Migrator::migrations())
        .await
        .expect("second run");
    assert_eq!(result.applied, 0);
    assert_eq!(result.skipped, Migrator::migrations().len());
    assert_eq!(table_counts(db, tenant).await, before);
}

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

mod sqlite {
    use super::*;

    #[tokio::test]
    async fn all_tables_exist_with_tenant_id() {
        case_all_tables_exist_with_tenant_id(&sqlite().await).await;
    }

    /// The migration creates a table for every entity and no `oagw_` table
    /// without one. Read from the recorded DDL, so it holds for any count.
    #[tokio::test]
    async fn migration_creates_exactly_the_entity_tables() {
        use std::collections::BTreeSet;

        use sea_orm::EntityName;

        macro_rules! table_names {
            ($($entity:ident),+ $(,)?) => {
                BTreeSet::from([$($entity::Entity.table_name().to_owned()),+])
            };
        }
        let created: BTreeSet<String> = crate::infra::storage::testing::migration_statements()
            .await
            .events()
            .iter()
            .filter(|e| {
                e.raw_sql
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE")
            })
            .filter_map(|e| {
                let name = &e.raw_sql[e.raw_sql.find("oagw_")?..];
                let end = name
                    .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
                    .unwrap_or(name.len());
                Some(name[..end].to_owned())
            })
            .collect();
        assert_eq!(created, with_entities!(table_names));
    }

    #[tokio::test]
    async fn values_at_the_field_limits_fit() {
        case_values_at_the_field_limits_fit(&sqlite().await).await;
    }

    #[tokio::test]
    async fn json_documents_over_64_kib_fit() {
        case_json_documents_over_64_kib_fit(&sqlite().await).await;
    }

    #[tokio::test]
    async fn timestamps_keep_microseconds_past_2038() {
        case_timestamps_keep_microseconds_past_2038(&sqlite().await).await;
    }

    #[tokio::test]
    async fn duplicate_tenant_alias_is_unique_violation() {
        case_duplicate_tenant_alias_is_unique_violation(&sqlite().await).await;
    }

    #[tokio::test]
    async fn cross_tenant_child_rows_are_fk_violations() {
        case_cross_tenant_child_rows_are_fk_violations(&sqlite().await).await;
    }

    #[tokio::test]
    async fn upstream_delete_cascades() {
        case_upstream_delete_cascades(&sqlite().await).await;
    }

    #[tokio::test]
    async fn migrating_twice_is_a_no_op() {
        case_migrating_twice_is_a_no_op(&sqlite().await).await;
    }
}

#[cfg(feature = "integration")]
mod postgres {
    use super::*;
    use crate::infra::storage::testing;

    /// One container for all cases; each case uses its own tenants and rows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schema_behaves_on_postgres() {
        let (_container, db) = testing::postgres().await;
        case_all_tables_exist_with_tenant_id(&db).await;
        case_values_at_the_field_limits_fit(&db).await;
        case_json_documents_over_64_kib_fit(&db).await;
        case_timestamps_keep_microseconds_past_2038(&db).await;
        case_duplicate_tenant_alias_is_unique_violation(&db).await;
        case_cross_tenant_child_rows_are_fk_violations(&db).await;
        case_upstream_delete_cascades(&db).await;
        case_migrating_twice_is_a_no_op(&db).await;
    }
}

#[cfg(feature = "integration")]
mod mysql {
    use super::*;
    use crate::infra::storage::testing;

    /// One container for all cases; each case uses its own tenants and rows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schema_behaves_on_mysql() {
        let (_container, db) = testing::mysql().await;
        case_all_tables_exist_with_tenant_id(&db).await;
        case_values_at_the_field_limits_fit(&db).await;
        case_json_documents_over_64_kib_fit(&db).await;
        case_timestamps_keep_microseconds_past_2038(&db).await;
        case_duplicate_tenant_alias_is_unique_violation(&db).await;
        case_cross_tenant_child_rows_are_fk_violations(&db).await;
        case_upstream_delete_cascades(&db).await;
        case_migrating_twice_is_a_no_op(&db).await;
    }
}

/// The migration keeps literal column sizes, since a released migration never
/// changes: every field limit and every stored enum text must still fit.
/// `SQLite` ignores `varchar` sizes, so only this catches a limit raised past
/// its column before `PostgreSQL` and `MySQL` reject the write.
#[test]
fn column_sizes_cover_the_field_limits_and_stored_texts() {
    use super::m20261005_000001_create_upstreams_and_routes::{
        ALIAS_LEN, ENUM_LEN, GRPC_NAME_LEN, PATH_LEN, REF_LEN, TAG_LEN,
    };
    use crate::domain::model::{HttpMethod, ManagedBy, SharingMode};
    use crate::domain::services::management::{
        MAX_ALIAS_LENGTH, MAX_GRPC_NAME_BYTES, MAX_PATH_BYTES, MAX_REF_BYTES, MAX_TAG_BYTES,
    };
    use crate::infra::storage::mapper::{
        MATCH_TYPE_GRPC, MATCH_TYPE_HTTP, managed_by_text, method_text, sharing_text,
    };

    let size = |column: u32| usize::try_from(column).expect("column size fits usize");
    for (field, limit, column) in [
        ("alias", MAX_ALIAS_LENGTH, ALIAS_LEN),
        ("tag", MAX_TAG_BYTES, TAG_LEN),
        ("path prefix", MAX_PATH_BYTES, PATH_LEN),
        ("gRPC name", MAX_GRPC_NAME_BYTES, GRPC_NAME_LEN),
        ("reference", MAX_REF_BYTES, REF_LEN),
    ] {
        assert!(
            limit <= size(column),
            "{field}: limit {limit} > column {column}"
        );
    }

    let sharing = [
        SharingMode::Private,
        SharingMode::Inherit,
        SharingMode::Enforce,
    ];
    let owners = [ManagedBy::Api, ManagedBy::Registry];
    let methods = [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Delete,
        HttpMethod::Patch,
    ];
    let texts = sharing
        .into_iter()
        .map(sharing_text)
        .chain(owners.into_iter().map(managed_by_text))
        .chain(methods.into_iter().map(method_text))
        .chain([MATCH_TYPE_HTTP, MATCH_TYPE_GRPC]);
    for text in texts {
        assert!(text.len() <= size(ENUM_LEN), "{text:?} exceeds {ENUM_LEN}");
    }
}

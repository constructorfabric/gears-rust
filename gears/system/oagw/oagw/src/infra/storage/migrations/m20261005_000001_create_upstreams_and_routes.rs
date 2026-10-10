//! Upstreams, routes and their child tables (ADR-0009, all except `oagw_plugin`).
//!
//! Schema builders only, so one code path serves every backend. Each parent
//! table is created first, then its unique `(tenant_id, id)` index, then its
//! child tables with **inline** composite foreign keys
//! `(tenant_id, parent_id) → parent (tenant_id, id) ON DELETE CASCADE`.
//! `PostgreSQL` requires the referenced unique index to exist when the foreign
//! key is declared, `MySQL` requires it to lead with the referenced columns in
//! the same order, and `SQLite` only accepts foreign keys inside
//! `CREATE TABLE`. The composite key is what keeps a child row in its parent's
//! tenant.
//!
//! Every index serves a query the repositories issue (ADR-0009, Appendix B);
//! `docs/migration.sql` spells the resulting `PostgreSQL` DDL out.
//!
//! JSON columns use the backend's JSON type ([`json`]); the database never
//! queries into them.
//!
//! Every string column is a sized `varchar`, never `text`. A column holding a
//! validated field (`management::validation`, `management::alias`) is sized
//! to that field's limit; a column holding a closed set of values (managed
//! by, sharing mode, match type, HTTP method) is [`ENUM_LEN`]. The sizes
//! count characters and the limits bytes, so every accepted value fits.
//!
//! `MySQL` gets two backend-specific choices, so that it stores and compares
//! what `PostgreSQL` and `SQLite` do: tables compare strings byte-wise
//! ([`new_table`]), and timestamps are `datetime(6)` ([`timestamp`]).

use sea_orm_migration::prelude::*;

/// `MAX_ALIAS_LENGTH` (`management::alias`).
pub(super) const ALIAS_LEN: u32 = 253;
/// `MAX_TAG_BYTES`.
pub(super) const TAG_LEN: u32 = 128;
/// `MAX_PATH_BYTES`.
pub(super) const PATH_LEN: u32 = 2048;
/// `MAX_GRPC_NAME_BYTES`.
pub(super) const GRPC_NAME_LEN: u32 = 256;
/// `MAX_REF_BYTES`: protocol and plugin references.
pub(super) const REF_LEN: u32 = 256;
/// A closed set of short values; the longest stored today is `registry`.
pub(super) const ENUM_LEN: u32 = 16;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_upstream(manager).await?;
        create_upstream_tag(manager).await?;
        create_upstream_plugin(manager).await?;
        create_route(manager).await?;
        create_route_http_match(manager).await?;
        create_route_method(manager).await?;
        create_route_grpc_match(manager).await?;
        create_route_tag(manager).await?;
        create_route_plugin(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let tables: [DynIden; 9] = [
            OagwRoutePlugin::Table.into_iden(),
            OagwRouteTag::Table.into_iden(),
            OagwRouteGrpcMatch::Table.into_iden(),
            OagwRouteMethod::Table.into_iden(),
            OagwRouteHttpMatch::Table.into_iden(),
            OagwRoute::Table.into_iden(),
            OagwUpstreamPlugin::Table.into_iden(),
            OagwUpstreamTag::Table.into_iden(),
            OagwUpstream::Table.into_iden(),
        ];
        for table in tables {
            manager
                .drop_table(Table::drop().table(table).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}

/// Composite FK `(tenant_id, child_id) → oagw_upstream (tenant_id, id)`.
fn fk_to_upstream<T: IntoIden + Copy + 'static>(
    name: &str,
    table: T,
    upstream_id: T,
    tenant_id: T,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(name)
        .from(table, (tenant_id, upstream_id))
        .to(
            OagwUpstream::Table,
            (OagwUpstream::TenantId, OagwUpstream::Id),
        )
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

/// Composite FK `(tenant_id, route_id) → oagw_route (tenant_id, id)`.
fn fk_to_route<T: IntoIden + Copy + 'static>(
    name: &str,
    table: T,
    route_id: T,
    tenant_id: T,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .name(name)
        .from(table, (tenant_id, route_id))
        .to(OagwRoute::Table, (OagwRoute::TenantId, OagwRoute::Id))
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

fn is_mysql(manager: &SchemaManager<'_>) -> bool {
    manager.get_database_backend() == sea_orm::DbBackend::MySql
}

/// `CREATE TABLE`, comparing strings byte-wise on `MySQL` too.
///
/// `MySQL`'s default collation (`utf8mb4_0900_ai_ci`) ignores case, accents
/// and width, so its keys would reject tags that differ only that way and an
/// alias lookup would match another alias. `utf8mb4_0900_bin` compares code
/// points without padding, as `PostgreSQL` and `SQLite` compare for equality.
fn new_table(manager: &SchemaManager<'_>) -> TableCreateStatement {
    let mut stmt = Table::create();
    if is_mysql(manager) {
        stmt.collate("utf8mb4_0900_bin");
    }
    stmt
}

/// `CREATE [UNIQUE] INDEX name ON table (cols…)`.
async fn index<T: IntoIden + Copy + 'static>(
    manager: &SchemaManager<'_>,
    name: &str,
    table: T,
    cols: &[T],
    unique: bool,
) -> Result<(), DbErr> {
    let mut stmt = Index::create();
    stmt.name(name).table(table).if_not_exists();
    for col in cols {
        stmt.col(*col);
    }
    if unique {
        stmt.unique();
    }
    manager.create_index(stmt.to_owned()).await
}

async fn create_upstream(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwUpstream as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::Id).uuid().not_null().primary_key())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Alias).string_len(ALIAS_LEN).not_null())
                .col(ColumnDef::new(T::Protocol).string_len(REF_LEN).not_null())
                .col(ColumnDef::new(T::Enabled).boolean().not_null())
                .col(ColumnDef::new(T::ManagedBy).string_len(ENUM_LEN).not_null())
                .col(schema_version(T::SchemaVersion))
                .col(json(T::Server).not_null())
                .col(ColumnDef::new(T::AuthPluginRef).string_len(REF_LEN).null())
                .col(ColumnDef::new(T::AuthPluginUuid).uuid().null())
                .col(json(T::AuthConfig).null())
                .col(
                    ColumnDef::new(T::AuthSharing)
                        .string_len(ENUM_LEN)
                        .not_null(),
                )
                .col(json(T::Headers).null())
                .col(json(T::Cors).null())
                .col(
                    ColumnDef::new(T::CorsSharing)
                        .string_len(ENUM_LEN)
                        .not_null(),
                )
                .col(json(T::RateLimit).null())
                .col(
                    ColumnDef::new(T::RateLimitSharing)
                        .string_len(ENUM_LEN)
                        .not_null(),
                )
                .col(
                    ColumnDef::new(T::PluginsSharing)
                        .string_len(ENUM_LEN)
                        .null(),
                )
                .col(timestamp(manager, T::CreatedAt))
                .col(timestamp(manager, T::UpdatedAt))
                .to_owned(),
        )
        .await?;
    index(
        manager,
        "uq_oagw_upstream_tenant_alias",
        T::Table,
        &[T::TenantId, T::Alias],
        true,
    )
    .await?;
    index(
        manager,
        "uq_oagw_upstream_tenant_id",
        T::Table,
        &[T::TenantId, T::Id],
        true,
    )
    .await?;
    index(
        manager,
        "idx_oagw_upstream_managed_by",
        T::Table,
        &[T::ManagedBy],
        false,
    )
    .await
}

async fn create_upstream_tag(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwUpstreamTag as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::UpstreamId).uuid().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Tag).string_len(TAG_LEN).not_null())
                .primary_key(Index::create().col(T::UpstreamId).col(T::Tag))
                .foreign_key(&mut fk_to_upstream(
                    "fk_oagw_upstream_tag_upstream",
                    T::Table,
                    T::UpstreamId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_upstream_plugin(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwUpstreamPlugin as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::UpstreamId).uuid().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Position).integer().not_null())
                .col(ColumnDef::new(T::PluginRef).string_len(REF_LEN).not_null())
                .col(ColumnDef::new(T::PluginUuid).uuid().null())
                .col(schema_version(T::SchemaVersion))
                .col(json(T::Config).null())
                .primary_key(Index::create().col(T::UpstreamId).col(T::Position))
                .foreign_key(&mut fk_to_upstream(
                    "fk_oagw_upstream_plugin_upstream",
                    T::Table,
                    T::UpstreamId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_route(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRoute as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::Id).uuid().not_null().primary_key())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::UpstreamId).uuid().not_null())
                .col(ColumnDef::new(T::Enabled).boolean().not_null())
                .col(ColumnDef::new(T::Priority).integer().not_null())
                .col(ColumnDef::new(T::MatchType).string_len(ENUM_LEN).not_null())
                .col(ColumnDef::new(T::ManagedBy).string_len(ENUM_LEN).not_null())
                .col(schema_version(T::SchemaVersion))
                .col(json(T::MatchConfig).null())
                .col(json(T::Cors).null())
                .col(json(T::RateLimit).null())
                .col(
                    ColumnDef::new(T::RateLimitSharing)
                        .string_len(ENUM_LEN)
                        .not_null(),
                )
                .col(
                    ColumnDef::new(T::PluginsSharing)
                        .string_len(ENUM_LEN)
                        .null(),
                )
                .col(timestamp(manager, T::CreatedAt))
                .col(timestamp(manager, T::UpdatedAt))
                .foreign_key(&mut fk_to_upstream(
                    "fk_oagw_route_upstream",
                    T::Table,
                    T::UpstreamId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await?;
    index(
        manager,
        "uq_oagw_route_tenant_id",
        T::Table,
        &[T::TenantId, T::Id],
        true,
    )
    .await?;
    // Route listing, the match-conflict check, the proxy's candidate lookup and
    // the cascade from `oagw_upstream`.
    index(
        manager,
        "idx_oagw_route_tenant_upstream",
        T::Table,
        &[T::TenantId, T::UpstreamId],
        false,
    )
    .await?;
    index(
        manager,
        "idx_oagw_route_managed_by",
        T::Table,
        &[T::ManagedBy],
        false,
    )
    .await
}

async fn create_route_http_match(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRouteHttpMatch as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::RouteId).uuid().not_null().primary_key())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(
                    ColumnDef::new(T::PathPrefix)
                        .string_len(PATH_LEN)
                        .not_null(),
                )
                .foreign_key(&mut fk_to_route(
                    "fk_oagw_route_http_match_route",
                    T::Table,
                    T::RouteId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_route_method(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRouteMethod as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::RouteId).uuid().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Method).string_len(ENUM_LEN).not_null())
                .primary_key(Index::create().col(T::RouteId).col(T::Method))
                .foreign_key(&mut fk_to_route(
                    "fk_oagw_route_method_route",
                    T::Table,
                    T::RouteId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_route_grpc_match(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRouteGrpcMatch as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::RouteId).uuid().not_null().primary_key())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(
                    ColumnDef::new(T::Service)
                        .string_len(GRPC_NAME_LEN)
                        .not_null(),
                )
                .col(
                    ColumnDef::new(T::Method)
                        .string_len(GRPC_NAME_LEN)
                        .not_null(),
                )
                .foreign_key(&mut fk_to_route(
                    "fk_oagw_route_grpc_match_route",
                    T::Table,
                    T::RouteId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_route_tag(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRouteTag as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::RouteId).uuid().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Tag).string_len(TAG_LEN).not_null())
                .primary_key(Index::create().col(T::RouteId).col(T::Tag))
                .foreign_key(&mut fk_to_route(
                    "fk_oagw_route_tag_route",
                    T::Table,
                    T::RouteId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

async fn create_route_plugin(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    use OagwRoutePlugin as T;
    manager
        .create_table(
            new_table(manager)
                .table(T::Table)
                .if_not_exists()
                .col(ColumnDef::new(T::RouteId).uuid().not_null())
                .col(ColumnDef::new(T::TenantId).uuid().not_null())
                .col(ColumnDef::new(T::Position).integer().not_null())
                .col(ColumnDef::new(T::PluginRef).string_len(REF_LEN).not_null())
                .col(ColumnDef::new(T::PluginUuid).uuid().null())
                .col(schema_version(T::SchemaVersion))
                .col(json(T::Config).null())
                .primary_key(Index::create().col(T::RouteId).col(T::Position))
                .foreign_key(&mut fk_to_route(
                    "fk_oagw_route_plugin_route",
                    T::Table,
                    T::RouteId,
                    T::TenantId,
                ))
                .to_owned(),
        )
        .await
}

fn schema_version<T: IntoIden>(col: T) -> ColumnDef {
    ColumnDef::new(col)
        .integer()
        .not_null()
        .default(1)
        .to_owned()
}

/// A JSON document column: `jsonb` on `PostgreSQL`, `json` on `MySQL`, and
/// text on `SQLite`. The database validates the document but never queries
/// into it.
fn json<T: IntoIden>(col: T) -> ColumnDef {
    ColumnDef::new(col).json_binary().to_owned()
}

/// A UTC timestamp with microseconds. On `MySQL` a timestamp with time zone
/// becomes `timestamp`, which keeps whole seconds and ends in 2038, so it is
/// `datetime(6)` there; the driver writes and reads that as UTC.
fn timestamp<T: IntoIden>(manager: &SchemaManager<'_>, col: T) -> ColumnDef {
    let mut def = ColumnDef::new(col);
    if is_mysql(manager) {
        def.custom(Alias::new("datetime(6)"));
    } else {
        def.timestamp_with_time_zone();
    }
    def.not_null().to_owned()
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwUpstream {
    Table,
    Id,
    TenantId,
    Alias,
    Protocol,
    Enabled,
    ManagedBy,
    SchemaVersion,
    Server,
    AuthPluginRef,
    AuthPluginUuid,
    AuthConfig,
    AuthSharing,
    Headers,
    Cors,
    CorsSharing,
    RateLimit,
    RateLimitSharing,
    PluginsSharing,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwUpstreamTag {
    Table,
    UpstreamId,
    TenantId,
    Tag,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwUpstreamPlugin {
    Table,
    UpstreamId,
    TenantId,
    Position,
    PluginRef,
    PluginUuid,
    SchemaVersion,
    Config,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRoute {
    Table,
    Id,
    TenantId,
    UpstreamId,
    Enabled,
    Priority,
    MatchType,
    ManagedBy,
    SchemaVersion,
    MatchConfig,
    Cors,
    RateLimit,
    RateLimitSharing,
    PluginsSharing,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRouteHttpMatch {
    Table,
    RouteId,
    TenantId,
    PathPrefix,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRouteMethod {
    Table,
    RouteId,
    TenantId,
    Method,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRouteGrpcMatch {
    Table,
    RouteId,
    TenantId,
    Service,
    Method,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRouteTag {
    Table,
    RouteId,
    TenantId,
    Tag,
}

#[derive(DeriveIden, Clone, Copy)]
enum OagwRoutePlugin {
    Table,
    RouteId,
    TenantId,
    Position,
    PluginRef,
    PluginUuid,
    SchemaVersion,
    Config,
}

//! Rows ↔ domain aggregates (ADR-0018 *Domain-to-Table Mapping*).
//!
//! Column enum texts are fixed: sharing modes `private|inherit|enforce`, HTTP
//! methods `GET|POST|PUT|DELETE|PATCH`, match type `http|grpc`, owner
//! `api|registry`. JSON columns hold the storage-local shapes of
//! [`super::json`].
//!
//! - A `*_sharing` column is the source of truth for its section; the JSON
//!   omits `sharing`. Route CORS has no column and keeps `sharing` in its JSON.
//!   Sharing columns of absent sections store `private`.
//! - `plugins_sharing` NULL means no plugins configuration; otherwise the
//!   binding rows, ordered by `position`, form the (possibly empty) list.
//! - Tags come back in byte order and HTTP methods in `HttpMethod` declaration
//!   order, the canonical orders the Control Plane writes. Both are sorted
//!   here rather than in SQL: a `PostgreSQL` `ORDER BY tag` follows the
//!   database collation, not byte order.
//! - `created_at` is written on create only; `updated_at` on every write.
//!
//! Reads never panic: undecodable stored data is `RepositoryError::Internal`.

use sea_orm::ActiveValue::Set;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::entity::{
    route, route_grpc_match, route_http_match, route_method, route_plugin, route_tag, upstream,
    upstream_plugin, upstream_tag,
};
use super::error::invalid_stored;
use super::json::{StoredCors, StoredHeaders, StoredMatchConfig, StoredRateLimit, StoredServer};
use crate::domain::model::{
    AuthConfig, GrpcMatch, HttpMatch, HttpMethod, ManagedBy, MatchRules, PathSuffixMode,
    PluginBinding, PluginsConfig, Route, SharingMode, Upstream,
};
use crate::domain::repo::RepositoryError;

/// Layout version of the JSON written by this code, for every table.
const SCHEMA_VERSION: i32 = 1;

pub(super) const MATCH_TYPE_HTTP: &str = "http";
pub(super) const MATCH_TYPE_GRPC: &str = "grpc";

const UPSTREAM: &str = "upstream";
const ROUTE: &str = "route";

// ---------------------------------------------------------------------------
// Enum texts
// ---------------------------------------------------------------------------

pub(super) fn sharing_text(mode: SharingMode) -> &'static str {
    match mode {
        SharingMode::Private => "private",
        SharingMode::Inherit => "inherit",
        SharingMode::Enforce => "enforce",
    }
}

pub(super) fn parse_sharing(text: &str) -> Result<SharingMode, String> {
    match text {
        "private" => Ok(SharingMode::Private),
        "inherit" => Ok(SharingMode::Inherit),
        "enforce" => Ok(SharingMode::Enforce),
        other => Err(format!("unknown sharing mode '{other}'")),
    }
}

pub(super) fn managed_by_text(owner: ManagedBy) -> &'static str {
    match owner {
        ManagedBy::Api => "api",
        ManagedBy::Registry => "registry",
    }
}

pub(super) fn parse_managed_by(text: &str) -> Result<ManagedBy, String> {
    match text {
        "api" => Ok(ManagedBy::Api),
        "registry" => Ok(ManagedBy::Registry),
        other => Err(format!("unknown owner '{other}'")),
    }
}

pub(super) fn method_text(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Patch => "PATCH",
    }
}

/// Exact, upper-case only: stored methods are always written by [`method_text`].
pub(super) fn parse_stored_method(text: &str) -> Result<HttpMethod, String> {
    match text {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "DELETE" => Ok(HttpMethod::Delete),
        "PATCH" => Ok(HttpMethod::Patch),
        other => Err(format!("unknown HTTP method '{other}'")),
    }
}

/// The UUID instance of a custom plugin reference (`<schema>~<uuid>`);
/// `None` for named (built-in) plugins.
pub(super) fn plugin_uuid(plugin_ref: &str) -> Option<Uuid> {
    let (_, instance) = plugin_ref.rsplit_once('~')?;
    Uuid::try_parse(instance).ok()
}

// ---------------------------------------------------------------------------
// JSON and column helpers
// ---------------------------------------------------------------------------

fn encode<T: Serialize>(entity: &str, id: Uuid, value: &T) -> Result<Json, RepositoryError> {
    serde_json::to_value(value)
        .map_err(|e| RepositoryError::Internal(format!("cannot encode {entity} {id}: {e}")))
}

fn decode<T: DeserializeOwned>(
    entity: &str,
    id: Uuid,
    column: &str,
    json: &Json,
) -> Result<T, RepositoryError> {
    T::deserialize(json).map_err(|e| invalid_stored(entity, id, format!("{column}: {e}")))
}

fn decode_opt<T: DeserializeOwned>(
    entity: &str,
    id: Uuid,
    column: &str,
    json: Option<&Json>,
) -> Result<Option<T>, RepositoryError> {
    json.map(|j| decode(entity, id, column, j)).transpose()
}

fn sharing_column(
    entity: &str,
    id: Uuid,
    column: &str,
    text: &str,
) -> Result<SharingMode, RepositoryError> {
    parse_sharing(text).map_err(|e| invalid_stored(entity, id, format!("{column}: {e}")))
}

/// Sharing mode of an optional section; absent sections store `private`.
fn section_sharing<T>(section: Option<&T>, sharing: impl Fn(&T) -> SharingMode) -> String {
    sharing_text(section.map_or(SharingMode::Private, sharing)).to_owned()
}

fn sorted_tags(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut tags: Vec<String> = tags.into_iter().collect();
    // `String`'s `Ord` is byte order.
    tags.sort();
    tags
}

// ---------------------------------------------------------------------------
// Plugin bindings (shared by both parents)
// ---------------------------------------------------------------------------

/// One binding row, independent of the parent table.
struct PluginRow {
    position: i32,
    plugin_ref: String,
    plugin_uuid: Option<Uuid>,
    config: Json,
}

fn plugin_rows(
    entity: &str,
    id: Uuid,
    plugins: Option<&PluginsConfig>,
) -> Result<Vec<PluginRow>, RepositoryError> {
    let Some(plugins) = plugins else {
        return Ok(Vec::new());
    };
    plugins
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let position = i32::try_from(index).map_err(|_| {
                RepositoryError::Internal(format!("{entity} {id} has too many plugins"))
            })?;
            Ok(PluginRow {
                position,
                plugin_ref: item.plugin_ref.clone(),
                plugin_uuid: plugin_uuid(&item.plugin_ref),
                config: encode(entity, id, &item.config)?,
            })
        })
        .collect()
}

/// `plugins_sharing` NULL ⇒ `None`; otherwise the rows ordered by position.
fn plugins_from_rows(
    entity: &str,
    id: Uuid,
    plugins_sharing: Option<&str>,
    mut rows: Vec<(i32, String, Option<Json>)>,
) -> Result<Option<PluginsConfig>, RepositoryError> {
    let Some(sharing) = plugins_sharing else {
        return Ok(None);
    };
    let sharing = sharing_column(entity, id, "plugins_sharing", sharing)?;
    rows.sort_by_key(|(position, _, _)| *position);
    let items = rows
        .into_iter()
        .map(|(_, plugin_ref, config)| {
            Ok(PluginBinding {
                plugin_ref,
                config: decode_opt(entity, id, "plugin config", config.as_ref())?
                    .unwrap_or_default(),
            })
        })
        .collect::<Result<_, RepositoryError>>()?;
    Ok(Some(PluginsConfig { sharing, items }))
}

// ---------------------------------------------------------------------------
// Upstream
// ---------------------------------------------------------------------------

/// The rows of one upstream, every column set (`created_at = updated_at = now`).
pub(super) struct UpstreamRows {
    pub(super) row: upstream::ActiveModel,
    pub(super) tags: Vec<upstream_tag::ActiveModel>,
    pub(super) plugins: Vec<upstream_plugin::ActiveModel>,
}

pub(super) fn upstream_rows(
    u: &Upstream,
    now: OffsetDateTime,
) -> Result<UpstreamRows, RepositoryError> {
    let (auth_plugin_ref, auth_plugin_uuid, auth_config) = match &u.auth {
        None => (None, None, None),
        Some(auth) => (
            Some(auth.plugin_type.clone()),
            plugin_uuid(&auth.plugin_type),
            auth.config
                .as_ref()
                .map(|c| encode(UPSTREAM, u.id, c))
                .transpose()?,
        ),
    };
    let row = upstream::ActiveModel {
        id: Set(u.id),
        tenant_id: Set(u.tenant_id),
        alias: Set(u.alias.clone()),
        protocol: Set(u.protocol.clone()),
        enabled: Set(u.enabled),
        managed_by: Set(managed_by_text(u.managed_by).to_owned()),
        schema_version: Set(SCHEMA_VERSION),
        server: Set(encode(UPSTREAM, u.id, &StoredServer::from(&u.server))?),
        auth_plugin_ref: Set(auth_plugin_ref),
        auth_plugin_uuid: Set(auth_plugin_uuid),
        auth_config: Set(auth_config),
        auth_sharing: Set(section_sharing(u.auth.as_ref(), |a| a.sharing)),
        headers: Set(u
            .headers
            .as_ref()
            .map(|h| encode(UPSTREAM, u.id, &StoredHeaders::from(h)))
            .transpose()?),
        cors: Set(u
            .cors
            .as_ref()
            .map(|c| encode(UPSTREAM, u.id, &StoredCors::new(c, false)))
            .transpose()?),
        cors_sharing: Set(section_sharing(u.cors.as_ref(), |c| c.sharing)),
        rate_limit: Set(u
            .rate_limit
            .as_ref()
            .map(|rl| encode(UPSTREAM, u.id, &StoredRateLimit::from(rl)))
            .transpose()?),
        rate_limit_sharing: Set(section_sharing(u.rate_limit.as_ref(), |rl| rl.sharing)),
        plugins_sharing: Set(u
            .plugins
            .as_ref()
            .map(|p| sharing_text(p.sharing).to_owned())),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let tags = u
        .tags
        .iter()
        .map(|tag| upstream_tag::ActiveModel {
            upstream_id: Set(u.id),
            tenant_id: Set(u.tenant_id),
            tag: Set(tag.clone()),
        })
        .collect();
    let plugins = plugin_rows(UPSTREAM, u.id, u.plugins.as_ref())?
        .into_iter()
        .map(|p| upstream_plugin::ActiveModel {
            upstream_id: Set(u.id),
            tenant_id: Set(u.tenant_id),
            position: Set(p.position),
            plugin_ref: Set(p.plugin_ref),
            plugin_uuid: Set(p.plugin_uuid),
            schema_version: Set(SCHEMA_VERSION),
            config: Set(Some(p.config)),
        })
        .collect();
    Ok(UpstreamRows { row, tags, plugins })
}

pub(super) fn upstream_from_rows(
    row: upstream::Model,
    tags: Vec<upstream_tag::Model>,
    plugins: Vec<upstream_plugin::Model>,
) -> Result<Upstream, RepositoryError> {
    let id = row.id;
    let sharing = |column: &str, text: &str| sharing_column(UPSTREAM, id, column, text);
    let auth = match row.auth_plugin_ref {
        None => None,
        Some(plugin_type) => Some(AuthConfig {
            plugin_type,
            sharing: sharing("auth_sharing", &row.auth_sharing)?,
            config: decode_opt(UPSTREAM, id, "auth_config", row.auth_config.as_ref())?,
        }),
    };
    let cors = decode_opt::<StoredCors>(UPSTREAM, id, "cors", row.cors.as_ref())?
        .map(|c| {
            Ok::<_, RepositoryError>(c.into_domain(sharing("cors_sharing", &row.cors_sharing)?))
        })
        .transpose()?;
    let rate_limit =
        decode_opt::<StoredRateLimit>(UPSTREAM, id, "rate_limit", row.rate_limit.as_ref())?
            .map(|rl| {
                Ok::<_, RepositoryError>(
                    rl.into_domain(sharing("rate_limit_sharing", &row.rate_limit_sharing)?),
                )
            })
            .transpose()?;
    let plugins = plugins_from_rows(
        UPSTREAM,
        id,
        row.plugins_sharing.as_deref(),
        plugins
            .into_iter()
            .map(|p| (p.position, p.plugin_ref, p.config))
            .collect(),
    )?;
    let managed_by =
        parse_managed_by(&row.managed_by).map_err(|e| invalid_stored(UPSTREAM, id, e))?;
    Ok(Upstream {
        id,
        tenant_id: row.tenant_id,
        alias: row.alias,
        server: decode::<StoredServer>(UPSTREAM, id, "server", &row.server)?.into(),
        protocol: row.protocol,
        enabled: row.enabled,
        auth,
        headers: decode_opt::<StoredHeaders>(UPSTREAM, id, "headers", row.headers.as_ref())?
            .map(Into::into),
        plugins,
        rate_limit,
        cors,
        tags: sorted_tags(tags.into_iter().map(|t| t.tag)),
        managed_by,
    })
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// The rows of one route, every column set (`created_at = updated_at = now`).
pub(super) struct RouteRows {
    pub(super) row: route::ActiveModel,
    pub(super) children: RouteChildRows,
}

/// The child rows of one route to write. Exactly one of `http_match` (with
/// `methods`) and `grpc_match` is set.
pub(super) struct RouteChildRows {
    pub(super) http_match: Option<route_http_match::ActiveModel>,
    pub(super) methods: Vec<route_method::ActiveModel>,
    pub(super) grpc_match: Option<route_grpc_match::ActiveModel>,
    pub(super) tags: Vec<route_tag::ActiveModel>,
    pub(super) plugins: Vec<route_plugin::ActiveModel>,
}

/// The child rows of one stored route.
#[derive(Default)]
pub(super) struct RouteChildren {
    pub(super) http_match: Option<route_http_match::Model>,
    pub(super) methods: Vec<route_method::Model>,
    pub(super) grpc_match: Option<route_grpc_match::Model>,
    pub(super) tags: Vec<route_tag::Model>,
    pub(super) plugins: Vec<route_plugin::Model>,
}

pub(super) fn route_rows(r: &Route, now: OffsetDateTime) -> Result<RouteRows, RepositoryError> {
    let (match_type, match_config, http_match, methods, grpc_match) =
        match (&r.match_rules.http, &r.match_rules.grpc) {
            (Some(http), None) => (
                MATCH_TYPE_HTTP,
                Some(encode(ROUTE, r.id, &StoredMatchConfig::from(http))?),
                Some(route_http_match::ActiveModel {
                    route_id: Set(r.id),
                    tenant_id: Set(r.tenant_id),
                    path_prefix: Set(http.path.clone()),
                }),
                http.methods
                    .iter()
                    .map(|m| route_method::ActiveModel {
                        route_id: Set(r.id),
                        tenant_id: Set(r.tenant_id),
                        method: Set(method_text(*m).to_owned()),
                    })
                    .collect(),
                None,
            ),
            (None, Some(grpc)) => (
                MATCH_TYPE_GRPC,
                None,
                None,
                Vec::new(),
                Some(route_grpc_match::ActiveModel {
                    route_id: Set(r.id),
                    tenant_id: Set(r.tenant_id),
                    service: Set(grpc.service.clone()),
                    method: Set(grpc.method.clone()),
                }),
            ),
            _ => {
                return Err(RepositoryError::Internal(format!(
                    "route {} must have exactly one of an HTTP and a gRPC match",
                    r.id
                )));
            }
        };
    let row = route::ActiveModel {
        id: Set(r.id),
        tenant_id: Set(r.tenant_id),
        upstream_id: Set(r.upstream_id),
        enabled: Set(r.enabled),
        priority: Set(r.priority),
        match_type: Set(match_type.to_owned()),
        managed_by: Set(managed_by_text(r.managed_by).to_owned()),
        schema_version: Set(SCHEMA_VERSION),
        match_config: Set(match_config),
        // Route CORS keeps its sharing mode in the JSON (no column).
        cors: Set(r
            .cors
            .as_ref()
            .map(|c| encode(ROUTE, r.id, &StoredCors::new(c, true)))
            .transpose()?),
        rate_limit: Set(r
            .rate_limit
            .as_ref()
            .map(|rl| encode(ROUTE, r.id, &StoredRateLimit::from(rl)))
            .transpose()?),
        rate_limit_sharing: Set(section_sharing(r.rate_limit.as_ref(), |rl| rl.sharing)),
        plugins_sharing: Set(r
            .plugins
            .as_ref()
            .map(|p| sharing_text(p.sharing).to_owned())),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let tags = r
        .tags
        .iter()
        .map(|tag| route_tag::ActiveModel {
            route_id: Set(r.id),
            tenant_id: Set(r.tenant_id),
            tag: Set(tag.clone()),
        })
        .collect();
    let plugins = plugin_rows(ROUTE, r.id, r.plugins.as_ref())?
        .into_iter()
        .map(|p| route_plugin::ActiveModel {
            route_id: Set(r.id),
            tenant_id: Set(r.tenant_id),
            position: Set(p.position),
            plugin_ref: Set(p.plugin_ref),
            plugin_uuid: Set(p.plugin_uuid),
            schema_version: Set(SCHEMA_VERSION),
            config: Set(Some(p.config)),
        })
        .collect();
    Ok(RouteRows {
        row,
        children: RouteChildRows {
            http_match,
            methods,
            grpc_match,
            tags,
            plugins,
        },
    })
}

/// Stored methods in canonical (declaration) order.
fn methods_from_rows(
    route_id: Uuid,
    rows: &[route_method::Model],
) -> Result<Vec<HttpMethod>, RepositoryError> {
    let mut methods = rows
        .iter()
        .map(|m| parse_stored_method(&m.method).map_err(|e| invalid_stored(ROUTE, route_id, e)))
        .collect::<Result<Vec<_>, _>>()?;
    methods.sort();
    Ok(methods)
}

pub(super) fn route_from_rows(
    row: route::Model,
    children: RouteChildren,
) -> Result<Route, RepositoryError> {
    let id = row.id;
    let match_rules = match row.match_type.as_str() {
        MATCH_TYPE_HTTP => {
            let http = children
                .http_match
                .ok_or_else(|| invalid_stored(ROUTE, id, "missing HTTP match row"))?;
            let config_json = row
                .match_config
                .as_ref()
                .ok_or_else(|| invalid_stored(ROUTE, id, "missing match_config"))?;
            let config: StoredMatchConfig = decode(ROUTE, id, "match_config", config_json)?;
            MatchRules {
                http: Some(HttpMatch {
                    methods: methods_from_rows(id, &children.methods)?,
                    path: http.path_prefix,
                    query_allowlist: config.query_allowlist,
                    path_suffix_mode: config.path_suffix_mode.into(),
                }),
                grpc: None,
            }
        }
        MATCH_TYPE_GRPC => {
            let grpc = children
                .grpc_match
                .ok_or_else(|| invalid_stored(ROUTE, id, "missing gRPC match row"))?;
            MatchRules {
                http: None,
                grpc: Some(GrpcMatch {
                    service: grpc.service,
                    method: grpc.method,
                }),
            }
        }
        other => {
            return Err(invalid_stored(
                ROUTE,
                id,
                format!("unknown match type '{other}'"),
            ));
        }
    };
    let cors = decode_opt::<StoredCors>(ROUTE, id, "cors", row.cors.as_ref())?
        .map(|c| {
            let sharing = c
                .sharing
                .ok_or_else(|| invalid_stored(ROUTE, id, "cors: missing sharing"))?;
            Ok::<_, RepositoryError>(c.into_domain(sharing.into()))
        })
        .transpose()?;
    let rate_limit =
        decode_opt::<StoredRateLimit>(ROUTE, id, "rate_limit", row.rate_limit.as_ref())?
            .map(|rl| {
                Ok::<_, RepositoryError>(rl.into_domain(sharing_column(
                    ROUTE,
                    id,
                    "rate_limit_sharing",
                    &row.rate_limit_sharing,
                )?))
            })
            .transpose()?;
    let plugins = plugins_from_rows(
        ROUTE,
        id,
        row.plugins_sharing.as_deref(),
        children
            .plugins
            .into_iter()
            .map(|p| (p.position, p.plugin_ref, p.config))
            .collect(),
    )?;
    let managed_by = parse_managed_by(&row.managed_by).map_err(|e| invalid_stored(ROUTE, id, e))?;
    Ok(Route {
        id,
        tenant_id: row.tenant_id,
        upstream_id: row.upstream_id,
        match_rules,
        plugins,
        rate_limit,
        cors,
        tags: sorted_tags(children.tags.into_iter().map(|t| t.tag)),
        priority: row.priority,
        enabled: row.enabled,
        managed_by,
    })
}

/// An enabled HTTP route candidate that allows the request method: only the
/// columns route selection reads.
#[derive(sea_orm::FromQueryResult)]
pub(super) struct CandidateRow {
    pub(super) id: Uuid,
    pub(super) tenant_id: Uuid,
    pub(super) upstream_id: Uuid,
    pub(super) priority: i32,
    pub(super) path_prefix: String,
}

/// The selection view of a candidate that allows `method`: only the fields
/// `route_matching::select_route` reads (id, tenant, upstream, enabled,
/// priority, methods, path). The winner is decoded in full afterwards.
pub(super) fn http_candidate(row: CandidateRow, method: HttpMethod) -> Route {
    Route {
        id: row.id,
        tenant_id: row.tenant_id,
        upstream_id: row.upstream_id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![method],
                path: row.path_prefix,
                query_allowlist: Vec::new(),
                path_suffix_mode: PathSuffixMode::default(),
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: Vec::new(),
        priority: row.priority,
        enabled: true,
        // Not read by `select_route`; the winner is decoded in full.
        managed_by: ManagedBy::Api,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sea_orm::TryIntoModel;

    use super::*;
    use crate::domain::gts_helpers::{
        APIKEY_AUTH_PLUGIN_ID, AUTH_PLUGIN_SCHEMA, HTTP_PROTOCOL_ID, TIMEOUT_GUARD_PLUGIN_ID,
    };
    use crate::domain::model::{
        CorsConfig, CorsHttpMethod, Endpoint, RateLimitAlgorithm, RateLimitConfig, RateLimitScope,
        RateLimitStrategy, Scheme, Server, SustainedRate, Window,
    };

    const ALL_SHARING: [SharingMode; 3] = [
        SharingMode::Private,
        SharingMode::Inherit,
        SharingMode::Enforce,
    ];

    #[test]
    fn sharing_texts_round_trip() {
        for mode in ALL_SHARING {
            assert_eq!(parse_sharing(sharing_text(mode)), Ok(mode));
        }
        assert_eq!(
            ALL_SHARING.map(sharing_text),
            ["private", "inherit", "enforce"]
        );
        assert!(parse_sharing("Private").is_err());
        assert!(parse_sharing("").is_err());
    }

    /// The route CORS JSON and the sharing columns spell modes the same way.
    #[test]
    fn json_and_column_sharing_texts_agree() {
        for mode in ALL_SHARING {
            let json =
                serde_json::to_string(&super::super::json::StoredSharingMode::from(mode)).unwrap();
            assert_eq!(json, format!("\"{}\"", sharing_text(mode)));
        }
    }

    #[test]
    fn managed_by_texts_round_trip() {
        for owner in [ManagedBy::Api, ManagedBy::Registry] {
            assert_eq!(parse_managed_by(managed_by_text(owner)), Ok(owner));
        }
        assert!(parse_managed_by("Registry").is_err());
        assert!(parse_managed_by("").is_err());
    }

    #[test]
    fn method_texts_round_trip() {
        let all = [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Delete,
            HttpMethod::Patch,
        ];
        for method in all {
            assert_eq!(parse_stored_method(method_text(method)), Ok(method));
        }
        assert_eq!(
            all.map(method_text),
            ["GET", "POST", "PUT", "DELETE", "PATCH"]
        );
        assert!(parse_stored_method("get").is_err());
        assert!(parse_stored_method("HEAD").is_err());
    }

    #[test]
    fn plugin_uuid_is_the_uuid_instance_only() {
        let id = Uuid::new_v4();
        assert_eq!(plugin_uuid(&format!("{AUTH_PLUGIN_SCHEMA}{id}")), Some(id));
        assert_eq!(plugin_uuid(APIKEY_AUTH_PLUGIN_ID), None);
        assert_eq!(plugin_uuid("no-tilde"), None);
    }

    fn rate_limit(sharing: SharingMode) -> RateLimitConfig {
        RateLimitConfig {
            sharing,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 10,
                window: Window::Second,
            },
            burst: None,
            budget: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            pool_owner_id: None,
        }
    }

    fn cors(sharing: SharingMode) -> CorsConfig {
        CorsConfig {
            sharing,
            enabled: true,
            allowed_origins: vec!["https://a.example".into()],
            allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Options],
            expose_headers: vec![],
            allow_credentials: false,
        }
    }

    fn sample_upstream() -> Upstream {
        Upstream {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            alias: "api".into(),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                }],
            },
            protocol: HTTP_PROTOCOL_ID.into(),
            enabled: true,
            auth: Some(AuthConfig {
                plugin_type: format!("{AUTH_PLUGIN_SCHEMA}{}", Uuid::new_v4()),
                sharing: SharingMode::Inherit,
                config: Some(HashMap::from([("k".to_owned(), "v".to_owned())])),
            }),
            headers: None,
            plugins: Some(PluginsConfig {
                sharing: SharingMode::Enforce,
                items: vec![PluginBinding {
                    plugin_ref: TIMEOUT_GUARD_PLUGIN_ID.into(),
                    config: HashMap::new(),
                }],
            }),
            rate_limit: Some(rate_limit(SharingMode::Enforce)),
            cors: Some(cors(SharingMode::Inherit)),
            tags: vec!["a".into(), "b".into()],
            managed_by: ManagedBy::Registry,
        }
    }

    fn sample_route() -> Route {
        Route {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            upstream_id: Uuid::new_v4(),
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get, HttpMethod::Delete],
                    path: "/v1".into(),
                    query_allowlist: vec!["q".into()],
                    path_suffix_mode: PathSuffixMode::Disabled,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: Some(rate_limit(SharingMode::Inherit)),
            cors: Some(cors(SharingMode::Enforce)),
            tags: vec!["t".into()],
            priority: 3,
            enabled: true,
            managed_by: ManagedBy::Registry,
        }
    }

    /// The stored models of `u`, as a read would return them.
    fn upstream_models(
        u: &Upstream,
    ) -> (
        upstream::Model,
        Vec<upstream_tag::Model>,
        Vec<upstream_plugin::Model>,
    ) {
        let rows = upstream_rows(u, OffsetDateTime::now_utc()).unwrap();
        (
            rows.row.try_into_model().unwrap(),
            rows.tags
                .into_iter()
                .map(|t| t.try_into_model().unwrap())
                .collect(),
            rows.plugins
                .into_iter()
                .map(|p| p.try_into_model().unwrap())
                .collect(),
        )
    }

    fn route_models(r: &Route) -> (route::Model, RouteChildren) {
        let rows = route_rows(r, OffsetDateTime::now_utc()).unwrap();
        let c = rows.children;
        (
            rows.row.try_into_model().unwrap(),
            RouteChildren {
                http_match: c.http_match.map(|m| m.try_into_model().unwrap()),
                methods: c
                    .methods
                    .into_iter()
                    .map(|m| m.try_into_model().unwrap())
                    .collect(),
                grpc_match: c.grpc_match.map(|m| m.try_into_model().unwrap()),
                tags: c
                    .tags
                    .into_iter()
                    .map(|t| t.try_into_model().unwrap())
                    .collect(),
                plugins: c
                    .plugins
                    .into_iter()
                    .map(|p| p.try_into_model().unwrap())
                    .collect(),
            },
        )
    }

    #[test]
    fn upstream_round_trips_through_rows() {
        let u = sample_upstream();
        let (row, tags, plugins) = upstream_models(&u);
        // Sharing lives in the columns, not in the JSON.
        assert!(row.cors.as_ref().unwrap().get("sharing").is_none());
        assert!(row.rate_limit.as_ref().unwrap().get("sharing").is_none());
        assert_eq!(row.cors_sharing, "inherit");
        assert_eq!(row.plugins_sharing.as_deref(), Some("enforce"));
        assert!(row.auth_plugin_uuid.is_some());
        assert_eq!(upstream_from_rows(row, tags, plugins).unwrap(), u);
    }

    #[test]
    fn route_round_trips_through_rows() {
        let r = sample_route();
        let (row, children) = route_models(&r);
        // Route CORS keeps its sharing in the JSON; rate limit does not.
        assert_eq!(row.cors.as_ref().unwrap()["sharing"], "enforce");
        assert!(row.rate_limit.as_ref().unwrap().get("sharing").is_none());
        assert_eq!(row.plugins_sharing, None);
        assert_eq!(route_from_rows(row, children).unwrap(), r);
    }

    #[test]
    fn stored_tags_and_methods_come_back_canonical() {
        let mut r = sample_route();
        r.tags = vec!["b".into(), "B".into(), "a_1".into(), "a-2".into()];
        if let Some(http) = r.match_rules.http.as_mut() {
            http.methods = vec![HttpMethod::Patch, HttpMethod::Get, HttpMethod::Delete];
        }
        let (row, mut children) = route_models(&r);
        children.tags.reverse();
        let read = route_from_rows(row, children).unwrap();
        assert_eq!(read.tags, ["B", "a-2", "a_1", "b"]);
        assert_eq!(
            read.match_rules.http.unwrap().methods,
            [HttpMethod::Get, HttpMethod::Delete, HttpMethod::Patch]
        );
    }

    /// Three bindings of one plugin, told apart by their config.
    fn three_plugins() -> PluginsConfig {
        PluginsConfig {
            sharing: SharingMode::Private,
            items: (1..=3)
                .map(|n| PluginBinding {
                    plugin_ref: TIMEOUT_GUARD_PLUGIN_ID.into(),
                    config: HashMap::from([("timeout_ms".to_owned(), n.to_string())]),
                })
                .collect(),
        }
    }

    /// Child rows come back in heap order (no `ORDER BY`), which `PostgreSQL`
    /// can change after deletes and vacuum; bindings are rebuilt by position.
    #[test]
    fn plugin_rows_in_any_order_come_back_in_position_order() {
        let mut u = sample_upstream();
        u.plugins = Some(three_plugins());
        let (row, tags, mut plugins) = upstream_models(&u);
        plugins.reverse();
        assert_eq!(upstream_from_rows(row, tags, plugins).unwrap(), u);

        let mut r = sample_route();
        r.plugins = Some(three_plugins());
        let (row, mut children) = route_models(&r);
        children.plugins.reverse();
        assert_eq!(route_from_rows(row, children).unwrap(), r);
    }

    #[track_caller]
    fn assert_invalid<T: std::fmt::Debug>(result: Result<T, RepositoryError>, needle: &str) {
        match result {
            Err(RepositoryError::Internal(msg)) => {
                assert!(msg.contains("is invalid") && msg.contains(needle), "{msg}");
            }
            other => panic!("expected Internal mentioning {needle:?}, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_upstream_rows_are_internal_errors() {
        let u = sample_upstream();
        let corrupt = |f: &dyn Fn(&mut upstream::Model)| {
            let (mut row, tags, plugins) = upstream_models(&u);
            f(&mut row);
            upstream_from_rows(row, tags, plugins)
        };
        assert_invalid(corrupt(&|r| r.server = serde_json::json!("{")), "server");
        assert_invalid(corrupt(&|r| r.managed_by = "user".into()), "owner");
        assert_invalid(
            corrupt(&|r| r.auth_sharing = "public".into()),
            "auth_sharing",
        );
        assert_invalid(
            corrupt(&|r| r.plugins_sharing = Some("x".into())),
            "plugins_sharing",
        );
        assert_invalid(
            corrupt(&|r| {
                r.rate_limit = r.rate_limit.take().map(|mut j| {
                    j["algorithm"] = "leaky_bucket".into();
                    j
                });
            }),
            "rate_limit",
        );
        assert_invalid(corrupt(&|r| r.cors = Some(serde_json::json!([]))), "cors");
        assert_invalid(
            corrupt(&|r| r.auth_config = Some(serde_json::json!(1))),
            "auth_config",
        );

        let (row, tags, mut plugins) = upstream_models(&u);
        plugins[0].config = Some(serde_json::json!("not a map"));
        assert_invalid(upstream_from_rows(row, tags, plugins), "plugin config");
    }

    #[test]
    fn corrupt_route_rows_are_internal_errors() {
        let r = sample_route();
        let corrupt = |f: &dyn Fn(&mut route::Model, &mut RouteChildren)| {
            let (mut row, mut children) = route_models(&r);
            f(&mut row, &mut children);
            route_from_rows(row, children)
        };
        assert_invalid(
            corrupt(&|row, _| row.match_type = "ws".into()),
            "match type",
        );
        assert_invalid(corrupt(&|row, _| row.managed_by = "user".into()), "owner");
        assert_invalid(
            corrupt(&|_, c| c.methods[0].method = "get".into()),
            "HTTP method",
        );
        assert_invalid(corrupt(&|_, c| c.http_match = None), "HTTP match row");
        assert_invalid(corrupt(&|row, _| row.match_config = None), "match_config");
        assert_invalid(
            corrupt(&|row, _| {
                row.cors = row.cors.take().map(|mut j| {
                    j.as_object_mut().unwrap().remove("sharing");
                    j
                });
            }),
            "sharing",
        );
        assert_invalid(
            corrupt(&|row, _| row.rate_limit_sharing = "none".into()),
            "rate_limit_sharing",
        );
        assert_invalid(
            corrupt(&|row, c| {
                row.match_type = MATCH_TYPE_GRPC.into();
                c.grpc_match = None;
            }),
            "gRPC match row",
        );
    }
}

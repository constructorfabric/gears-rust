use super::alias::{enforce_alias_create_derived, enforce_alias_create_with};
use super::bind::parse_secret_ref;
use super::budget::validate_budget_config;
use super::canonical::canonicalize_match_rules;
use crate::domain::error::DomainError;
use crate::domain::gts_helpers::{
    AUTH_PLUGIN_SCHEMA, GUARD_PLUGIN_SCHEMA, IMPLEMENTED_AUTH_PLUGIN_IDS,
    IMPLEMENTED_GUARD_PLUGIN_IDS, IMPLEMENTED_TRANSFORM_PLUGIN_IDS, TRANSFORM_PLUGIN_SCHEMA,
};
use crate::domain::model::{
    AuthConfig, CorsConfig, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HeadersConfig,
    HttpMatch, HttpMethod, MatchRules, PluginsConfig, RateLimitAlgorithm, RateLimitConfig, Route,
    Scheme, UpdateRouteRequest, UpdateUpstreamRequest,
};
use crate::domain::repo::RowKey;
use crate::domain::ssrf::SsrfGuard;

/// The checks of an upstream create that read nothing: the request's fields,
/// its endpoints against the SSRF policy, and the syntax of its credential
/// references. Returns the alias the create stores. The Management API and
/// the registry reconcile both run it before any read.
pub(in crate::domain::services) fn validate_upstream_request(
    ssrf: &SsrfGuard,
    allow_http: bool,
    req: &CreateUpstreamRequest,
) -> Result<String, DomainError> {
    validate_upstream_content(
        ssrf,
        allow_http,
        &req.tags,
        &req.protocol,
        req.auth.as_ref(),
        req.headers.as_ref(),
        req.plugins.as_ref(),
        req.cors.as_ref(),
        req.rate_limit.as_ref(),
        &req.server.endpoints,
    )?;
    match req.alias.as_deref() {
        Some(user_alias) => enforce_alias_create_with(user_alias, &req.server.endpoints),
        None => enforce_alias_create_derived(&req.server.endpoints),
    }
}

/// [`validate_upstream_request`] for a full-replacement update, without the
/// alias: which alias rules apply depends on the stored row and the writer.
pub(in crate::domain::services) fn validate_upstream_update_request(
    ssrf: &SsrfGuard,
    allow_http: bool,
    req: &UpdateUpstreamRequest,
) -> Result<(), DomainError> {
    validate_upstream_content(
        ssrf,
        allow_http,
        &req.tags,
        &req.protocol,
        req.auth.as_ref(),
        req.headers.as_ref(),
        req.plugins.as_ref(),
        req.cors.as_ref(),
        req.rate_limit.as_ref(),
        &req.server.endpoints,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the shared body of the create and update upstream checks"
)]
fn validate_upstream_content(
    ssrf: &SsrfGuard,
    allow_http: bool,
    tags: &[String],
    protocol: &str,
    auth: Option<&AuthConfig>,
    headers: Option<&HeadersConfig>,
    plugins: Option<&PluginsConfig>,
    cors: Option<&CorsConfig>,
    rate_limit: Option<&RateLimitConfig>,
    endpoints: &[Endpoint],
) -> Result<(), DomainError> {
    validate_tags(tags)?;
    validate_upstream_refs(protocol, auth)?;
    validate_plugin_refs(plugins)?;
    validate_plugin_ids(auth, plugins)?;
    validate_json_text(auth, headers, cors, plugins)?;
    validate_endpoints(endpoints)?;
    forbid_http_endpoints(endpoints, allow_http, "")?;
    validate_endpoints_ssrf(ssrf, endpoints)?;
    if let Some(cors) = cors {
        crate::domain::cors::validate_cors_config(cors)?;
    }
    if let Some(rate_limit) = rate_limit {
        validate_rate_limit_values(rate_limit)?;
    }
    if let Some(budget) = rate_limit.and_then(|rl| rl.budget.as_ref()) {
        validate_budget_config(budget)?;
    }
    if let Some(auth) = auth {
        for credential in auth.credential_refs() {
            parse_secret_ref(credential)?;
        }
    }
    Ok(())
}

/// The checks of a route create that read nothing. Returns the canonical
/// match rules the create stores. The Management API and the registry
/// reconcile both run it before any read.
pub(in crate::domain::services) fn validate_route_request(
    req: &CreateRouteRequest,
) -> Result<MatchRules, DomainError> {
    validate_route_content(
        &req.tags,
        &req.match_rules,
        req.plugins.as_ref(),
        req.cors.as_ref(),
        req.rate_limit.as_ref(),
    )
}

/// [`validate_route_request`] for a full-replacement update.
pub(in crate::domain::services) fn validate_route_update_request(
    req: &UpdateRouteRequest,
) -> Result<MatchRules, DomainError> {
    validate_route_content(
        &req.tags,
        &req.match_rules,
        req.plugins.as_ref(),
        req.cors.as_ref(),
        req.rate_limit.as_ref(),
    )
}

fn validate_route_content(
    tags: &[String],
    match_rules: &MatchRules,
    plugins: Option<&PluginsConfig>,
    cors: Option<&CorsConfig>,
    rate_limit: Option<&RateLimitConfig>,
) -> Result<MatchRules, DomainError> {
    validate_tags(tags)?;
    if let Some(rate_limit) = rate_limit {
        validate_rate_limit_values(rate_limit)?;
    }
    validate_match_fields(match_rules)?;
    validate_plugin_refs(plugins)?;
    validate_plugin_ids(None, plugins)?;
    validate_json_text(None, None, cors, plugins)?;
    if let Some(cors) = cors {
        crate::domain::cors::validate_cors_config(cors)?;
    }
    let mut rules = match_rules.clone();
    canonicalize_match_rules(&mut rules);
    validate_match_rules(&rules)?;
    Ok(rules)
}

/// Ensure exactly one of `http` or `grpc` is present in the match rules.
///
/// Rejects routes where both fields are `None` (matches nothing) or both
/// are `Some` (ambiguous protocol).
pub(in crate::domain::services) fn validate_match_rules(
    rules: &MatchRules,
) -> Result<(), DomainError> {
    match (&rules.http, &rules.grpc) {
        (None, None) => Err(DomainError::validation(
            "match rules must specify exactly one of 'http' or 'grpc'",
        )),
        (Some(_), Some(_)) => Err(DomainError::validation(
            "match rules must specify exactly one of 'http' or 'grpc', not both",
        )),
        _ => Ok(()),
    }
}

/// Most tags an upstream or route may carry.
const MAX_TAGS: usize = 64;
/// Longest tag, in bytes.
pub(crate) const MAX_TAG_BYTES: usize = 128;
/// Longest HTTP path prefix, in bytes.
pub(crate) const MAX_PATH_BYTES: usize = 2048;
/// Longest gRPC service or method name, in bytes.
pub(crate) const MAX_GRPC_NAME_BYTES: usize = 256;
/// Longest protocol, auth plugin type or plugin reference, in bytes.
pub(crate) const MAX_REF_BYTES: usize = 256;
/// Most plugin bindings an upstream or route may carry. The proxy reads them
/// joined to their parent row, so this also bounds the rows of one read.
const MAX_PLUGINS: usize = 32;
/// Most query parameters an HTTP match may allow.
const MAX_QUERY_PARAMS: usize = 64;
/// Longest allowed query parameter name, in bytes.
const MAX_QUERY_PARAM_BYTES: usize = 128;

/// At most [`MAX_TAGS`] tags of at most [`MAX_TAG_BYTES`] bytes, without
/// control characters. Tags are stored in `varchar` columns sized to the
/// limit, which `PostgreSQL` rejects NUL in.
pub(in crate::domain::services) fn validate_tags(tags: &[String]) -> Result<(), DomainError> {
    if tags.len() > MAX_TAGS {
        return Err(DomainError::validation(format!(
            "at most {MAX_TAGS} tags are allowed, got {}",
            tags.len()
        )));
    }
    for (i, tag) in tags.iter().enumerate() {
        validate_stored_text(&format!("tags[{i}]"), tag, MAX_TAG_BYTES)?;
        if tag.is_empty()
            || !tag
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        {
            return Err(DomainError::validation(format!(
                "tags[{i}] must match ^[a-z0-9_-]+$"
            )));
        }
    }
    Ok(())
}

/// A rate limit that can admit a request: `rate`, `capacity` and `cost` are
/// at least 1, and `cost` fits the bucket. Zero would admit everything
/// (`cost`) or nothing with a fixed retry hint (`rate`, `capacity`).
fn validate_rate_limit_values(rate_limit: &RateLimitConfig) -> Result<(), DomainError> {
    if rate_limit.sustained.rate == 0 {
        return Err(DomainError::validation(
            "rate_limit.sustained.rate must be at least 1",
        ));
    }
    if rate_limit.burst.as_ref().is_some_and(|b| b.capacity == 0) {
        return Err(DomainError::validation(
            "rate_limit.burst.capacity must be at least 1",
        ));
    }
    if rate_limit.cost == 0 {
        return Err(DomainError::validation(
            "rate_limit.cost must be at least 1",
        ));
    }
    // A request costing more than a full bucket is never admitted. A sliding
    // window ignores `burst`.
    let (capacity, field) = match (rate_limit.algorithm, &rate_limit.burst) {
        (RateLimitAlgorithm::TokenBucket, Some(burst)) => (burst.capacity, "burst.capacity"),
        _ => (rate_limit.sustained.rate, "sustained.rate"),
    };
    if rate_limit.cost > capacity {
        return Err(DomainError::validation(format!(
            "rate_limit.cost ({}) must not exceed rate_limit.{field} ({capacity})",
            rate_limit.cost
        )));
    }
    Ok(())
}

/// Bound the matched path prefix and gRPC names, which are stored in sized
/// `varchar` columns like tags.
pub(in crate::domain::services) fn validate_match_fields(
    rules: &MatchRules,
) -> Result<(), DomainError> {
    if let Some(http) = &rules.http {
        validate_stored_text("match.http.path", &http.path, MAX_PATH_BYTES)?;
        if http.methods.is_empty() {
            return Err(DomainError::validation(
                "match.http.methods must list at least one method",
            ));
        }
        // A prefix that is not absolute (empty included), or one the proxy
        // would reject in a request path (stray `%`, `;`, encoded
        // dot-segment, non-UTF-8 escape), could never match.
        crate::domain::proxy_path::validate_route_prefix(&http.path)?;
        if http.query_allowlist.len() > MAX_QUERY_PARAMS {
            return Err(DomainError::validation(format!(
                "match.http.query_allowlist allows at most {MAX_QUERY_PARAMS} parameters, got {}",
                http.query_allowlist.len()
            )));
        }
        for (i, param) in http.query_allowlist.iter().enumerate() {
            validate_stored_text(
                &format!("match.http.query_allowlist[{i}]"),
                param,
                MAX_QUERY_PARAM_BYTES,
            )?;
        }
    }
    if let Some(grpc) = &rules.grpc {
        validate_stored_text("match.grpc.service", &grpc.service, MAX_GRPC_NAME_BYTES)?;
        validate_stored_text("match.grpc.method", &grpc.method, MAX_GRPC_NAME_BYTES)?;
    }
    Ok(())
}

/// Bound the upstream's protocol and auth plugin type, which are stored in
/// `varchar` columns that `PostgreSQL` rejects NUL in. Which plugin
/// `auth.type` names is [`validate_plugin_ids`]'s concern.
pub(in crate::domain::services) fn validate_upstream_refs(
    protocol: &str,
    auth: Option<&AuthConfig>,
) -> Result<(), DomainError> {
    validate_stored_text("protocol", protocol, MAX_REF_BYTES)?;
    if let Some(auth) = auth {
        validate_stored_text("auth.type", &auth.plugin_type, MAX_REF_BYTES)?;
    }
    Ok(())
}

/// Refuse plaintext endpoints unless the gear allows them (HTTPS-only
/// constraint). Writes and the proxy both call it, so a violation reads the
/// same wherever it is caught.
pub(crate) fn forbid_http_endpoints(
    endpoints: &[Endpoint],
    allow_http: bool,
    instance: &str,
) -> Result<(), DomainError> {
    if !allow_http && endpoints.iter().any(|ep| ep.scheme == Scheme::Http) {
        return Err(DomainError::Validation {
            field: "server.endpoints.scheme",
            reason: oagw_sdk::field::HTTP_UPSTREAM_FORBIDDEN,
            detail: "upstream endpoint uses HTTP; only HTTPS endpoints are permitted".to_owned(),
            instance: instance.to_owned(),
        });
    }
    Ok(())
}

/// The plugins an upstream or route names must be implemented, and of the
/// kind their place takes: an auth plugin in `auth.type`, guards and
/// transforms in `plugins.items`. Catalog-only, custom and unknown ids are
/// `NOT_IMPLEMENTED`; a plugin of the wrong kind is a `VALIDATION` error.
pub(in crate::domain::services) fn validate_plugin_ids(
    auth: Option<&AuthConfig>,
    plugins: Option<&PluginsConfig>,
) -> Result<(), DomainError> {
    if let Some(auth) = auth {
        // A guard or transform here is the wrong kind, not a missing
        // plugin: reported like an auth plugin in `plugins.items`.
        if auth.plugin_type.starts_with(GUARD_PLUGIN_SCHEMA)
            || auth.plugin_type.starts_with(TRANSFORM_PLUGIN_SCHEMA)
        {
            return Err(DomainError::validation_for(
                "auth.type",
                oagw_sdk::field::VALIDATION,
                format!(
                    "auth.type: '{}' is not an auth plugin; bind it in plugins.items",
                    auth.plugin_type
                ),
            ));
        }
        if !IMPLEMENTED_AUTH_PLUGIN_IDS.contains(&auth.plugin_type.as_str()) {
            return Err(not_implemented(
                "auth.type",
                format!(
                    "auth plugin '{}' is not implemented; use one of {IMPLEMENTED_AUTH_PLUGIN_IDS:?}",
                    auth.plugin_type
                ),
            ));
        }
    }
    for (i, item) in plugins.iter().flat_map(|p| &p.items).enumerate() {
        let plugin_ref = item.plugin_ref.as_str();
        if plugin_ref.starts_with(AUTH_PLUGIN_SCHEMA) {
            return Err(DomainError::validation_for(
                "plugins.items.plugin_ref",
                oagw_sdk::field::VALIDATION,
                format!(
                    "plugins.items[{i}]: '{plugin_ref}' is an auth plugin; set it in auth.type"
                ),
            ));
        }
        if !IMPLEMENTED_GUARD_PLUGIN_IDS.contains(&plugin_ref)
            && !IMPLEMENTED_TRANSFORM_PLUGIN_IDS.contains(&plugin_ref)
        {
            return Err(not_implemented(
                "plugins.items.plugin_ref",
                format!(
                    "plugins.items[{i}]: plugin '{plugin_ref}' is not implemented; use one of {:?}",
                    [
                        IMPLEMENTED_GUARD_PLUGIN_IDS,
                        IMPLEMENTED_TRANSFORM_PLUGIN_IDS
                    ]
                    .concat()
                ),
            ));
        }
    }
    Ok(())
}

/// A 400 for configuration OAGW does not implement.
fn not_implemented(field: &'static str, detail: String) -> DomainError {
    DomainError::validation_for(field, oagw_sdk::field::NOT_IMPLEMENTED, detail)
}

/// At most [`MAX_PLUGINS`] plugin bindings, with references bounded like
/// [`validate_upstream_refs`]. Which plugins they name is
/// [`validate_plugin_ids`]'s concern.
pub(in crate::domain::services) fn validate_plugin_refs(
    plugins: Option<&PluginsConfig>,
) -> Result<(), DomainError> {
    if let Some(plugins) = plugins
        && plugins.items.len() > MAX_PLUGINS
    {
        return Err(DomainError::validation(format!(
            "plugins.items allows at most {MAX_PLUGINS} plugins, got {}",
            plugins.items.len()
        )));
    }
    for (i, item) in plugins.iter().flat_map(|p| &p.items).enumerate() {
        validate_stored_text(
            &format!("plugins.items[{i}].plugin_ref"),
            &item.plugin_ref,
            MAX_REF_BYTES,
        )?;
    }
    Ok(())
}

/// Reject NUL in the free-form strings stored inside JSON columns: auth and
/// plugin config, header rules and CORS lists. `PostgreSQL`'s `jsonb` cannot
/// store `\u0000`, so such input would otherwise fail as a 500 there only.
pub(in crate::domain::services) fn validate_json_text(
    auth: Option<&AuthConfig>,
    headers: Option<&HeadersConfig>,
    cors: Option<&CorsConfig>,
    plugins: Option<&PluginsConfig>,
) -> Result<(), DomainError> {
    if let Some(config) = auth.and_then(|a| a.config.as_ref()) {
        reject_nul_in_map("auth.config", config)?;
    }
    if let Some(request) = headers.and_then(|h| h.request.as_ref()) {
        reject_nul_in_map("headers.request.set", &request.set)?;
        reject_nul_in_map("headers.request.add", &request.add)?;
        reject_nul("headers.request.remove", &request.remove)?;
        reject_nul(
            "headers.request.passthrough_allowlist",
            &request.passthrough_allowlist,
        )?;
    }
    if let Some(response) = headers.and_then(|h| h.response.as_ref()) {
        reject_nul_in_map("headers.response.set", &response.set)?;
        reject_nul_in_map("headers.response.add", &response.add)?;
        reject_nul("headers.response.remove", &response.remove)?;
    }
    if let Some(cors) = cors {
        reject_nul("cors.allowed_origins", &cors.allowed_origins)?;
        reject_nul("cors.expose_headers", &cors.expose_headers)?;
    }
    for (i, item) in plugins.iter().flat_map(|p| &p.items).enumerate() {
        reject_nul_in_map(&format!("plugins.items[{i}].config"), &item.config)?;
    }
    Ok(())
}

fn reject_nul<'a>(
    name: &str,
    values: impl IntoIterator<Item = &'a String>,
) -> Result<(), DomainError> {
    if values.into_iter().any(|v| v.contains('\0')) {
        return Err(DomainError::validation(format!(
            "{name} must not contain NUL characters"
        )));
    }
    Ok(())
}

fn reject_nul_in_map(
    name: &str,
    entries: &std::collections::HashMap<String, String>,
) -> Result<(), DomainError> {
    reject_nul(name, entries.keys().chain(entries.values()))
}

fn validate_stored_text(name: &str, value: &str, max_bytes: usize) -> Result<(), DomainError> {
    if value.len() > max_bytes {
        return Err(DomainError::validation(format!(
            "{name} must not exceed {max_bytes} bytes, got {}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(DomainError::validation(format!(
            "{name} must not contain control characters"
        )));
    }
    Ok(())
}

/// Validate the endpoint list for a server configuration.
///
/// Rules:
/// - At least one endpoint is required.
/// - All endpoints must use either IP addresses or hostnames — no mixing.
/// - All endpoints must share the same scheme (upstream-level invariant).
pub(in crate::domain::services) fn validate_endpoints(
    endpoints: &[Endpoint],
) -> Result<(), DomainError> {
    if endpoints.is_empty() {
        return Err(DomainError::validation(
            "server must have at least one endpoint",
        ));
    }

    for (i, ep) in endpoints.iter().enumerate() {
        if ep.port == 0 {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] port must be between 1 and 65535"
            )));
        }
    }

    // IPv6 endpoints are not yet supported — reject early with a clear message.
    // SSRF protections for IPv6 (link-local, ULA, IPv4-mapped, etc.) are in
    // place (`ssrf.rs`), but the proxy and endpoint infrastructure has not been
    // tested with IPv6 upstream addresses yet.
    for (i, ep) in endpoints.iter().enumerate() {
        if ep.normalized_host().parse::<std::net::Ipv6Addr>().is_ok() {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] uses IPv6 address '{}'; IPv6 endpoints are not yet supported",
                ep.host
            )));
        }
    }

    // Check all-IP vs all-hostname consistency.
    let ip_count = endpoints.iter().filter(|ep| ep.is_ip()).count();
    if ip_count != 0 && ip_count != endpoints.len() {
        return Err(DomainError::validation(
            "all endpoints must use either IP addresses or hostnames; mixed configurations are not allowed",
        ));
    }

    // Validate hostname format (RFC 1123) for non-IP endpoints.
    if ip_count == 0 {
        for (i, ep) in endpoints.iter().enumerate() {
            validate_hostname(i, &ep.host)?;
        }
    }

    // Enforce identical scheme and port across the pool.
    if endpoints.len() > 1 {
        let first_scheme = &endpoints[0].scheme;
        let first_port = endpoints[0].port;
        for (i, ep) in endpoints.iter().enumerate().skip(1) {
            if ep.scheme != *first_scheme {
                return Err(DomainError::validation(format!(
                    "endpoint[{i}] scheme {:?} differs from endpoint[0] scheme {:?}; all endpoints must share the same scheme",
                    ep.scheme, first_scheme
                )));
            }
            if ep.port != first_port {
                return Err(DomainError::validation(format!(
                    "endpoint[{i}] port {} differs from endpoint[0] port {}; all endpoints must share the same port",
                    ep.port, first_port
                )));
            }
        }
    }

    Ok(())
}

/// Validate endpoints against the SSRF guard.
///
/// Rejects endpoints whose host is a blocked hostname or whose IP address
/// is denied by the built-in or extra deny-lists (unless allow-listed).
/// When the guard is disabled, this is a no-op.
pub(in crate::domain::services) fn validate_endpoints_ssrf(
    guard: &SsrfGuard,
    endpoints: &[Endpoint],
) -> Result<(), DomainError> {
    if !guard.is_enabled() {
        return Ok(());
    }
    validate_endpoint_hostnames_ssrf(guard, endpoints)?;
    for (i, ep) in endpoints.iter().enumerate() {
        let host = ep.normalized_host();
        if let Ok(ip) = host.parse::<std::net::IpAddr>()
            && guard.is_ip_blocked(ip)
        {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] IP address '{}' is blocked by SSRF protection: {}",
                ep.host,
                guard.ip_block_reason(ip),
            )));
        }
    }
    Ok(())
}

/// Reject an endpoint whose host is on the SSRF hostname deny-list. Runs on
/// every alias resolution too, so a stored upstream stops proxying once the
/// deny-list covers it; resolved IPs are checked by the data plane.
pub(in crate::domain::services) fn validate_endpoint_hostnames_ssrf(
    guard: &SsrfGuard,
    endpoints: &[Endpoint],
) -> Result<(), DomainError> {
    for (i, ep) in endpoints.iter().enumerate() {
        if let Some(blocked) = guard.is_hostname_blocked(&ep.normalized_host()) {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] hostname '{}' is blocked by SSRF protection (matches '{blocked}')",
                ep.host,
            )));
        }
    }
    Ok(())
}

/// Validate a hostname per RFC 1123: max 253 chars total, each label 1–63 chars,
/// labels contain only ASCII alphanumeric + hyphen, labels don't start/end with
/// hyphen. A trailing dot (FQDN) is tolerated and stripped before validation.
fn validate_hostname(index: usize, host: &str) -> Result<(), DomainError> {
    let h = host.strip_suffix('.').unwrap_or(host);
    if h.is_empty() {
        return Err(DomainError::validation(format!(
            "endpoint[{index}] host is empty"
        )));
    }
    if h.len() > 253 {
        return Err(DomainError::validation(format!(
            "endpoint[{index}] host '{}' exceeds 253 characters",
            host
        )));
    }
    for label in h.split('.') {
        if label.is_empty() {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' contains an empty label"
            )));
        }
        if label.len() > 63 {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' exceeds 63 characters"
            )));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' contains invalid characters; \
                 only ASCII alphanumeric and '-' are allowed"
            )));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' must not start or end with '-'"
            )));
        }
    }
    Ok(())
}

/// The method two HTTP matches on the same upstream both serve at the same
/// path and priority, if any. Callers check that both routes are enabled.
pub(crate) fn overlapping_method(
    a: &HttpMatch,
    a_priority: i32,
    b: &HttpMatch,
    b_priority: i32,
) -> Option<HttpMethod> {
    if a.path != b.path || a_priority != b_priority {
        return None;
    }
    a.methods.iter().find(|m| b.methods.contains(m)).copied()
}

/// The 409 for a route write that overlaps a stored route on `method` (the
/// rule of [`overlapping_method`]). One rule for every writer: a stale
/// registry row blocks an overlapping registry write until it moves.
pub(in crate::domain::services) fn route_overlap_conflict(
    route: &Route,
    method: HttpMethod,
) -> DomainError {
    let path = route
        .match_rules
        .http
        .as_ref()
        .map_or("", |h| h.path.as_str());
    DomainError::conflict(
        "route",
        format!("{}:{}:{:?}", route.upstream_id, path, method),
        format!(
            "route overlap: an enabled route already exists on upstream '{}' \
                 with path '{}', priority {}, method {:?}",
            route.upstream_id, path, route.priority, method
        ),
    )
}

/// Every pair of routes the types registry provides that overlap each other,
/// by the rule of [`route_overlap_conflict`], with the first shared method. Pairs
/// are ordered by the lower route ID first, then by the higher.
pub(crate) fn registry_route_overlaps(
    routes: &[(RowKey, &CreateRouteRequest)],
) -> Vec<(RowKey, RowKey, HttpMethod)> {
    let enabled_http = routes.iter().filter_map(|(key, req)| {
        let http = req.match_rules.http.as_ref()?;
        req.enabled.then_some((*key, *req, http))
    });
    let enabled_http: Vec<_> = enabled_http.collect();
    let mut pairs = Vec::new();
    for (i, (a_key, a, a_http)) in enabled_http.iter().enumerate() {
        for (b_key, b, b_http) in &enabled_http[i + 1..] {
            if a_key.tenant_id != b_key.tenant_id || a.upstream_id != b.upstream_id {
                continue;
            }
            if let Some(m) = overlapping_method(a_http, a.priority, b_http, b.priority) {
                let (first, second) = if a_key.id <= b_key.id {
                    (*a_key, *b_key)
                } else {
                    (*b_key, *a_key)
                };
                pairs.push((first, second, m));
            }
        }
    }
    pairs.sort_by_key(|(a, b, _)| (a.id, b.id));
    pairs
}

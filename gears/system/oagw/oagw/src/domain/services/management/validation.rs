use crate::domain::error::DomainError;
use crate::domain::model::{
    AuthConfig, CorsConfig, CreateRouteRequest, Endpoint, HeadersConfig, HttpMatch, HttpMethod,
    ListQuery, ManagedBy, MatchRules, PluginsConfig, Route,
};
use crate::domain::repo::{RouteRepository, RowKey};
use crate::domain::ssrf::SsrfGuard;
use uuid::Uuid;

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
/// `varchar` columns that `PostgreSQL` rejects NUL in.
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

/// At most [`MAX_PLUGINS`] plugin bindings, with references bounded like
/// [`validate_upstream_refs`].
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
fn overlapping_method(
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

/// Check that no existing **enabled** route under the same upstream shares
/// `(path_prefix, priority, method)` with the candidate route.
///
/// `exclude_id` is `Some(route.id)` on update to skip the route being
/// modified (it will be compared against its new state, not itself).
///
/// The registry reconcile (`writer` [`ManagedBy::Registry`]) skips stored
/// registry routes: by the time it writes, each of them is either pruned or
/// about to be rewritten to the content [`check_registry_route_overlaps`]
/// already checked, so they cannot conflict once the reconcile is done.
///
/// Returns `DomainError::Conflict` on violation (maps to 409).
pub(in crate::domain::services) async fn check_route_overlap(
    routes: &dyn RouteRepository,
    candidate: &Route,
    exclude_id: Option<Uuid>,
    writer: ManagedBy,
) -> Result<(), DomainError> {
    // Disabled routes cannot cause match-time ambiguity.
    if !candidate.enabled {
        return Ok(());
    }

    let candidate_http = match &candidate.match_rules.http {
        Some(h) => h,
        None => return Ok(()), // No HTTP match rules → no overlap to check.
    };

    // Fetch all routes for this (tenant, upstream).
    let all = routes
        .list(
            candidate.tenant_id,
            Some(candidate.upstream_id),
            &ListQuery {
                top: u32::MAX,
                skip: 0,
            },
        )
        .await
        .map_err(DomainError::from)?;

    for existing in &all {
        // Skip self on update.
        if Some(existing.id) == exclude_id {
            continue;
        }
        if writer == ManagedBy::Registry && existing.managed_by == ManagedBy::Registry {
            continue;
        }
        // Only enabled routes can conflict.
        if !existing.enabled {
            continue;
        }
        // Must have HTTP match rules.
        let Some(existing_http) = &existing.match_rules.http else {
            continue;
        };
        if let Some(m) = overlapping_method(
            candidate_http,
            candidate.priority,
            existing_http,
            existing.priority,
        ) {
            return Err(DomainError::conflict(
                "route",
                format!("{}:{}:{:?}", candidate.upstream_id, candidate_http.path, m),
                format!(
                    "route overlap: an enabled route already exists on upstream '{}' \
                         with path '{}', priority {}, method {:?}",
                    candidate.upstream_id, candidate_http.path, candidate.priority, m
                ),
            ));
        }
    }

    Ok(())
}

/// Check the routes the types registry provides against each other, by the
/// rule of [`check_route_overlap`], before the reconcile writes any of them.
pub(crate) fn check_registry_route_overlaps(
    routes: &[(RowKey, &CreateRouteRequest)],
) -> Result<(), DomainError> {
    let enabled_http = routes.iter().filter_map(|(key, req)| {
        let http = req.match_rules.http.as_ref()?;
        req.enabled.then_some((key, *req, http))
    });
    let enabled_http: Vec<_> = enabled_http.collect();
    for (i, (a_key, a, a_http)) in enabled_http.iter().enumerate() {
        for (b_key, b, b_http) in &enabled_http[i + 1..] {
            if a_key.tenant_id != b_key.tenant_id || a.upstream_id != b.upstream_id {
                continue;
            }
            if let Some(m) = overlapping_method(a_http, a.priority, b_http, b.priority) {
                let (first, second) = (a_key.id.min(b_key.id), a_key.id.max(b_key.id));
                return Err(DomainError::validation(format!(
                    "types-registry routes {first} and {second} overlap on upstream '{}' \
                     (tenant={}): path '{}', priority {}, method {m:?}",
                    a.upstream_id, a_key.tenant_id, a_http.path, a.priority
                )));
            }
        }
    }
    Ok(())
}

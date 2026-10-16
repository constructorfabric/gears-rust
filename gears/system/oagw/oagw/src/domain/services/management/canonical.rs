use super::alias::{enforce_alias_create_derived, enforce_alias_create_with};
use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, MatchRules, Route, Upstream,
};

/// De-duplicate tags and sort them ascending, so every storage backend stores
/// and returns the same list.
pub(in crate::domain::services) fn canonical_tags(mut tags: Vec<String>) -> Vec<String> {
    tags.sort();
    tags.dedup();
    tags
}

/// De-duplicate HTTP methods and sort them in canonical (declaration) order.
pub(in crate::domain::services) fn canonicalize_match_rules(rules: &mut MatchRules) {
    if let Some(http) = rules.http.as_mut() {
        http.methods.sort();
        http.methods.dedup();
    }
}

/// The fields of `stored` that differ from `req` as `create_upstream` would
/// store it (derived or validated alias, canonical tags). Fails as create
/// would when `req` has no valid alias.
pub(crate) fn upstream_drift(
    stored: &Upstream,
    req: &CreateUpstreamRequest,
) -> Result<Vec<&'static str>, DomainError> {
    let alias = match req.alias.as_deref() {
        Some(alias) => enforce_alias_create_with(alias, &req.server.endpoints),
        None => enforce_alias_create_derived(&req.server.endpoints),
    }?;
    // Exhaustive, so a new field must be compared or ignored here.
    let Upstream {
        id: _,
        tenant_id: _,
        managed_by: _,
        alias: stored_alias,
        server,
        protocol,
        enabled,
        auth,
        headers,
        plugins,
        rate_limit,
        cors,
        tags,
    } = stored;
    Ok([
        ("alias", alias == *stored_alias),
        ("server", *server == req.server),
        ("protocol", *protocol == req.protocol),
        ("enabled", *enabled == req.enabled),
        ("auth", *auth == req.auth),
        ("headers", *headers == req.headers),
        ("plugins", *plugins == req.plugins),
        ("rate_limit", *rate_limit == req.rate_limit),
        ("cors", *cors == req.cors),
        ("tags", *tags == canonical_tags(req.tags.clone())),
    ]
    .into_iter()
    .filter_map(|(field, same)| (!same).then_some(field))
    .collect())
}

/// The fields of `stored` that differ from `req` as `create_route` would
/// store it (canonical tags and HTTP methods).
pub(crate) fn route_drift(stored: &Route, req: &CreateRouteRequest) -> Vec<&'static str> {
    let mut match_rules = req.match_rules.clone();
    canonicalize_match_rules(&mut match_rules);
    // Exhaustive, so a new field must be compared or ignored here.
    let Route {
        id: _,
        tenant_id: _,
        managed_by: _,
        upstream_id,
        match_rules: stored_match_rules,
        plugins,
        rate_limit,
        cors,
        tags,
        priority,
        enabled,
    } = stored;
    [
        ("upstream_id", *upstream_id == req.upstream_id),
        ("match", *stored_match_rules == match_rules),
        ("plugins", *plugins == req.plugins),
        ("rate_limit", *rate_limit == req.rate_limit),
        ("cors", *cors == req.cors),
        ("tags", *tags == canonical_tags(req.tags.clone())),
        ("priority", *priority == req.priority),
        ("enabled", *enabled == req.enabled),
    ]
    .into_iter()
    .filter_map(|(field, same)| (!same).then_some(field))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{
        Endpoint, HttpMatch, HttpMethod, ManagedBy, PathSuffixMode, Scheme, Server,
    };
    use uuid::Uuid;

    #[test]
    fn tags_are_deduplicated_and_sorted() {
        let tags = vec!["b".to_owned(), "a".to_owned(), "b".to_owned()];
        assert_eq!(canonical_tags(tags), vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn methods_are_deduplicated_and_sorted_in_declaration_order() {
        let mut rules = MatchRules {
            http: Some(HttpMatch {
                methods: vec![
                    HttpMethod::Patch,
                    HttpMethod::Get,
                    HttpMethod::Post,
                    HttpMethod::Get,
                ],
                path: "/".into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        };
        canonicalize_match_rules(&mut rules);
        assert_eq!(
            rules.http.unwrap().methods,
            vec![HttpMethod::Get, HttpMethod::Post, HttpMethod::Patch]
        );
    }

    /// What create normalizes (derived alias, tag and method order) is not
    /// drift; a real change is.
    #[test]
    fn drift_ignores_what_create_normalizes() {
        let req = CreateUpstreamRequest {
            id: None,
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                }],
            },
            protocol: oagw_sdk::HTTP_PROTOCOL_ID.into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec!["b".into(), "a".into(), "b".into()],
            enabled: true,
        };
        let stored = Upstream {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            alias: "api.example.com".into(),
            server: req.server.clone(),
            protocol: req.protocol.clone(),
            enabled: true,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec!["a".into(), "b".into()],
            managed_by: ManagedBy::Api,
        };
        assert!(upstream_drift(&stored, &req).unwrap().is_empty());
        let disabled = Upstream {
            enabled: false,
            ..stored.clone()
        };
        assert_eq!(upstream_drift(&disabled, &req).unwrap(), ["enabled"]);

        let route_req = CreateRouteRequest {
            id: None,
            upstream_id: stored.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Post, HttpMethod::Get, HttpMethod::Post],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };
        let mut match_rules = route_req.match_rules.clone();
        canonicalize_match_rules(&mut match_rules);
        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: stored.tenant_id,
            upstream_id: stored.id,
            match_rules,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        };
        assert!(route_drift(&route, &route_req).is_empty());
    }
}

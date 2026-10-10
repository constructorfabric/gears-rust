//! Route selection rules shared by every `RouteRepository` backend.
//!
//! Both the in-memory and the database repositories load candidate routes
//! and delegate the choice to [`select_route`], so the selection order cannot
//! drift between backends (`cpt-cf-oagw-algo-route-matching`).

use crate::domain::model::{HttpMethod, Route};
use uuid::Uuid;

/// Parse an HTTP method name (case-insensitive). Unknown methods return `None`.
pub(crate) fn parse_method(s: &str) -> Option<HttpMethod> {
    match s.to_uppercase().as_str() {
        "GET" => Some(HttpMethod::Get),
        "POST" => Some(HttpMethod::Post),
        "PUT" => Some(HttpMethod::Put),
        "DELETE" => Some(HttpMethod::Delete),
        "PATCH" => Some(HttpMethod::Patch),
        _ => None,
    }
}

/// Select the best enabled HTTP route for `method` and `path`.
///
/// Only routes owned by a tenant in `tenant_chain` and attached to one of
/// `upstream_ids` are considered (the argument order of
/// `RouteRepository::find_matching_in_tenants`). The winner is chosen in this
/// order: upstream preference (position in `upstream_ids`), tenant chain
/// position, longest path prefix, higher priority number, lowest route ID.
pub(crate) fn select_route<'a>(
    candidates: impl IntoIterator<Item = &'a Route>,
    tenant_chain: &[Uuid],
    upstream_ids: &[Uuid],
    method: HttpMethod,
    path: &str,
) -> Option<&'a Route> {
    candidates
        .into_iter()
        .filter_map(|route| {
            match_rank(route, tenant_chain, upstream_ids, method, path).map(|rank| (rank, route))
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, route)| route)
}

/// The rank of `route` among the candidates of [`select_route`], lowest
/// wins, or `None` when it does not match.
pub(crate) fn match_rank(
    route: &Route,
    tenant_chain: &[Uuid],
    upstream_ids: &[Uuid],
    method: HttpMethod,
    path: &str,
) -> Option<impl Ord + use<>> {
    if !route.enabled {
        return None;
    }
    let http = route.match_rules.http.as_ref()?;
    if !http.methods.contains(&method) || !path.starts_with(&http.path) {
        return None;
    }
    let upstream_rank = upstream_ids
        .iter()
        .position(|id| *id == route.upstream_id)?;
    let tenant_rank = tenant_chain.iter().position(|id| *id == route.tenant_id)?;
    Some((
        upstream_rank,
        tenant_rank,
        std::cmp::Reverse(http.path.len()),
        std::cmp::Reverse(route.priority),
        route.id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{GrpcMatch, HttpMatch, ManagedBy, MatchRules, PathSuffixMode};

    fn route(tenant: Uuid, upstream: Uuid, path: &str, priority: i32) -> Route {
        Route {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            upstream_id: upstream,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Post],
                    path: path.into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority,
            enabled: true,
            managed_by: ManagedBy::Api,
        }
    }

    fn pick(
        routes: &[Route],
        chain: &[Uuid],
        upstreams: &[Uuid],
        method: &str,
        path: &str,
    ) -> Option<Uuid> {
        select_route(routes, chain, upstreams, parse_method(method)?, path).map(|r| r.id)
    }

    #[test]
    fn longest_prefix_wins() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1", 0), route(t, u, "/v1/chat", 0)];
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/chat/completions"),
            Some(routes[1].id)
        );
    }

    #[test]
    fn higher_priority_wins_on_equal_prefix() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1", 1), route(t, u, "/v1", 10)];
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/x"),
            Some(routes[1].id)
        );
    }

    #[test]
    fn lowest_id_breaks_remaining_ties() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1", 0), route(t, u, "/v1", 0)];
        let lowest = routes.iter().map(|r| r.id).min();
        assert_eq!(pick(&routes, &[t], &[u], "POST", "/v1/x"), lowest);
        let reversed = [routes[1].clone(), routes[0].clone()];
        assert_eq!(pick(&reversed, &[t], &[u], "POST", "/v1/x"), lowest);
    }

    #[test]
    fn upstream_preference_beats_longer_prefix() {
        let (t, selected, ancestor) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let routes = [
            route(t, ancestor, "/v1/chat", 0),
            route(t, selected, "/v1", 0),
        ];
        assert_eq!(
            pick(&routes, &[t], &[selected, ancestor], "POST", "/v1/chat"),
            Some(routes[1].id)
        );
    }

    #[test]
    fn falls_back_to_ancestor_upstream() {
        let (t, selected, ancestor) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, ancestor, "/v1", 0)];
        assert_eq!(
            pick(&routes, &[t], &[selected, ancestor], "POST", "/v1/chat"),
            Some(routes[0].id)
        );
    }

    #[test]
    fn closer_tenant_wins_within_an_upstream() {
        let (child, root, u) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(root, u, "/v1/chat", 0), route(child, u, "/v1", 0)];
        assert_eq!(
            pick(&routes, &[child, root], &[u], "POST", "/v1/chat"),
            Some(routes[1].id)
        );
    }

    #[test]
    fn routes_outside_chain_or_upstreams_are_ignored() {
        let (t, other_t, u, other_u) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let routes = [route(other_t, u, "/v1", 0), route(t, other_u, "/v1", 0)];
        assert_eq!(pick(&routes, &[t], &[u], "POST", "/v1"), None);
    }

    #[test]
    fn disabled_method_mismatch_unknown_method_and_grpc_do_not_match() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let mut disabled = route(t, u, "/v1", 0);
        disabled.enabled = false;
        let mut grpc = route(t, u, "/v1", 0);
        grpc.match_rules = MatchRules {
            http: None,
            grpc: Some(GrpcMatch {
                service: "svc".into(),
                method: "m".into(),
            }),
        };
        let routes = [disabled, grpc];
        assert_eq!(pick(&routes, &[t], &[u], "POST", "/v1"), None);

        let post_only = [route(t, u, "/v1", 0)];
        assert_eq!(pick(&post_only, &[t], &[u], "GET", "/v1"), None);
        assert_eq!(pick(&post_only, &[t], &[u], "HEAD", "/v1"), None);
    }

    #[test]
    fn parse_method_is_case_insensitive() {
        assert_eq!(parse_method("post"), Some(HttpMethod::Post));
        assert_eq!(parse_method("OPTIONS"), None);
    }
}

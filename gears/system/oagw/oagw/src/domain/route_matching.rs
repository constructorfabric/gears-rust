//! Route selection rules shared by every `RouteRepository` backend.
//!
//! Both the in-memory and the database repositories load candidate routes
//! and delegate the choice to [`select_route`], so the selection order cannot
//! drift between backends (`cpt-cf-oagw-algo-route-matching`).

use crate::domain::error::DomainError;
use crate::domain::model::{HttpMethod, Route};
use crate::domain::proxy_path::normalize_percent_encoding;
use oagw_sdk::field;
use std::borrow::Cow;
use std::cmp::Reverse;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Parse an HTTP method name (case-insensitive). Unknown methods return `None`.
pub(crate) fn parse_method(s: &str) -> Option<HttpMethod> {
    match s.to_uppercase().as_str() {
        "GET" => Some(HttpMethod::Get),
        "POST" => Some(HttpMethod::Post),
        "PUT" => Some(HttpMethod::Put),
        "DELETE" => Some(HttpMethod::Delete),
        "PATCH" => Some(HttpMethod::Patch),
        "HEAD" => Some(HttpMethod::Head),
        "OPTIONS" => Some(HttpMethod::Options),
        _ => None,
    }
}

/// Whether the normalized request `path` is under the route `prefix`, on a
/// whole-segment boundary: `/v1/chat` covers `/v1/chat` and `/v1/chat/x`,
/// not `/v1/chatty`. A prefix ending in `/` covers everything below it, and
/// route `/` also covers the bare alias (empty path).
///
/// Returns the rest of `path` after the prefix, the suffix the proxy appends
/// to the route path upstream, or `None` when the route does not cover it.
fn path_after_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix == "/" {
        // The root route covers every path, and the bare alias.
        return Some(path);
    }
    // A trailing `/` on the prefix gets the same boundary rule: requests
    // arrive with it collapsed (`/v1/admin/` as `/v1/admin`), and `%2F`
    // after `/v1/admin` must not fall through to a broader route either.
    let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    match path.strip_prefix(prefix) {
        Some(rest) if rest.is_empty() || !continues_segment(rest) => Some(rest),
        _ => None,
    }
}

/// Whether `rest` continues the prefix's last segment: it starts with a
/// character a segment name is made of (ASCII letter or digit, `-`, `_`,
/// `~`). Anything else ends the segment: `/`, but also `.json`, `:verb`, an
/// escape (`%2F`, `%252F`), `\\` or a non-ASCII separator, which upstreams
/// may split on or strip (Rails formats, IIS trailing dots). So `/v1/chatty`
/// is not under `/v1/chat`, while `/v1/chat.json` and `/v1/chat%2Fx` are.
///
/// The rule is for ASCII names: an escaped non-ASCII character also ends
/// the segment, so `/v1/chat%C3%A9` is under `/v1/chat`. It cannot skip a
/// narrower route (that prefix would start with the same escape and be
/// longer), while reading it as part of the name would let one that folds
/// to a separator (`%EF%BC%8F`, a fullwidth `/`) fall through to `/v1`.
fn continues_segment(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '~'))
}

/// [`path_after_prefix`] for a stored route prefix: requests are matched in
/// normal form ([`normalize_percent_encoding`]), so the prefix is compared in
/// the same form. Both route matching and the proxy's suffix handling use
/// this, so they agree on where the prefix ends.
pub(crate) fn path_after_route_prefix<'a>(path: &'a str, route_prefix: &str) -> Option<&'a str> {
    path_after_prefix(path, &normalized_route_prefix(route_prefix))
}

/// A stored route prefix in the form requests are matched in. Prefixes
/// written since write-time normalization are already in it and borrowed.
fn normalized_route_prefix(prefix: &str) -> Cow<'_, str> {
    if prefix.contains('%') {
        Cow::Owned(normalize_percent_encoding(prefix))
    } else {
        Cow::Borrowed(prefix)
    }
}

/// Select the best enabled HTTP route for `method` and `path`: see
/// [`RouteSelection`]. `Ok(None)`: no route matches.
pub(crate) fn select_route<'a>(
    candidates: impl IntoIterator<Item = &'a Route>,
    tenant_chain: &[Uuid],
    upstream_ids: &[Uuid],
    method: HttpMethod,
    path: &str,
) -> Result<Option<&'a Route>, SeparatorBypass> {
    let mut selection = RouteSelection::new(tenant_chain, upstream_ids, method, path);
    let mut best = None;
    for route in candidates {
        if selection.offer(route) {
            best = Some(route);
        }
    }
    Ok(selection.winner()?.and(best))
}

/// The request path selects another route when its `%2F`, `%5C` or `\` is
/// read as `/` ([`RouteSelection`]). The client is refused with 400
/// `INVALID_PROXY_PATH`; the detail does not name the other route.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeparatorBypass;

impl SeparatorBypass {
    /// The 400 answered for the request path `instance`.
    pub(crate) fn into_error(self, instance: &str) -> DomainError {
        DomainError::Validation {
            field: "path",
            reason: field::INVALID_PROXY_PATH,
            detail: "an encoded `/` or `\\` in the proxy path selects a different route \
                     than the decoded path; send the path decoded"
                .into(),
            instance: instance.to_owned(),
        }
    }
}

/// Rank of a matching route, lowest wins: the fields compare in order.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// Position of the route's upstream in `upstream_ids`.
    upstream: usize,
    /// Position of the route's tenant in the tenant chain.
    tenant: usize,
    /// Longer prefix first.
    prefix_len: Reverse<usize>,
    /// Higher priority first.
    priority: Reverse<i32>,
    id: Uuid,
}

/// The choice of [`select_route`], one candidate at a time, so a repository
/// can rank routes in place.
///
/// Only routes owned by a tenant in `tenant_chain` and attached to one of
/// `upstream_ids` are considered (the argument order of
/// `RouteRepository::find_matching_in_tenants`). The winner is chosen in this
/// order: upstream preference (position in `upstream_ids`), tenant chain
/// position, longest path prefix, higher priority number, lowest route ID.
///
/// An encoded `/` or `\` (`%2F`, `%5C`, or a fullwidth or small solidus)
/// ends a segment but does not start a new one, so `/v1%2Fadmin` is under
/// `/v1`. An upstream that decodes it, folds it (NFKC), or turns `\` into
/// `/`, before routing serves `/v1/admin`. So the candidates are also
/// ranked with those read as `/` in both the path and the
/// prefixes; when that picks another route (here `/v1/admin`), or a route
/// where the path as sent matches none, the request would bypass it and is
/// refused ([`SeparatorBypass`]). The same holds the other way: with routes
/// `/v3` and `/v3/a%2Fb`, `/v3/a/b` is refused.
#[domain_model]
pub(crate) struct RouteSelection<'a> {
    tenant_chain: &'a [Uuid],
    upstream_ids: &'a [Uuid],
    method: HttpMethod,
    path: &'a str,
    /// `path` with the separators read as `/` ([`decode_separators`]).
    decoded: Cow<'a, str>,
    best: Option<Rank>,
    best_decoded: Option<Rank>,
}

impl<'a> RouteSelection<'a> {
    /// A selection for one request, with no candidate offered yet.
    pub(crate) fn new(
        tenant_chain: &'a [Uuid],
        upstream_ids: &'a [Uuid],
        method: HttpMethod,
        path: &'a str,
    ) -> Self {
        Self {
            tenant_chain,
            upstream_ids,
            method,
            path,
            decoded: decode_separators(path),
            best: None,
            best_decoded: None,
        }
    }

    /// Rank `route`; `true` when it is the best match of `path` so far.
    pub(crate) fn offer(&mut self, route: &Route) -> bool {
        let Some((upstream, tenant, http_path)) = self.eligible(route) else {
            return false;
        };
        let rank = |prefix: &str| Rank {
            upstream,
            tenant,
            prefix_len: Reverse(prefix.len()),
            priority: Reverse(route.priority),
            id: route.id,
        };
        // Match and rank on the prefix in the form requests are matched in.
        let prefix = normalized_route_prefix(http_path);
        let decoded_prefix = decode_separators(&prefix);
        if path_after_prefix(&self.decoded, &decoded_prefix).is_some() {
            let decoded = rank(&decoded_prefix);
            if self.best_decoded.is_none_or(|best| decoded < best) {
                self.best_decoded = Some(decoded);
            }
        }
        if path_after_prefix(self.path, &prefix).is_none() {
            return false;
        }
        let sent = rank(&prefix);
        if self.best.is_some_and(|best| best <= sent) {
            return false;
        }
        self.best = Some(sent);
        true
    }

    /// The ID of the winner, `None` when no route matches either way, or
    /// [`SeparatorBypass`] when reading the separators as `/` picks another
    /// route.
    pub(crate) fn winner(&self) -> Result<Option<Uuid>, SeparatorBypass> {
        match (self.best, self.best_decoded) {
            (None, None) => Ok(None),
            (Some(sent), Some(decoded)) if sent.id == decoded.id => Ok(Some(sent.id)),
            _ => Err(SeparatorBypass),
        }
    }

    /// The upstream and tenant positions of an enabled HTTP `route` for
    /// `method`, and its path prefix; `None` when it is not a candidate.
    fn eligible<'r>(&self, route: &'r Route) -> Option<(usize, usize, &'r str)> {
        if !route.enabled {
            return None;
        }
        let http = route.match_rules.http.as_ref()?;
        if !http.methods.contains(&self.method) {
            return None;
        }
        let upstream = self
            .upstream_ids
            .iter()
            .position(|id| *id == route.upstream_id)?;
        let tenant = self
            .tenant_chain
            .iter()
            .position(|id| *id == route.tenant_id)?;
        Some((upstream, tenant, &http.path))
    }
}

/// Escapes an upstream may read as `/` before routing: `/` and `\`, and the
/// fullwidth and small solidi that NFKC folds to them (U+FF0F, U+FF3C,
/// U+FE68).
const SEPARATOR_ESCAPES: &[&str] = &["%2F", "%5C", "%EF%BC%8F", "%EF%BC%BC", "%EF%B9%A8"];

/// The length of the separator escape `s` starts with, if any.
fn separator_escape_len(s: &str) -> Option<usize> {
    SEPARATOR_ESCAPES
        .iter()
        .find(|e| s.get(..e.len()).is_some_and(|p| p.eq_ignore_ascii_case(e)))
        .map(|e| e.len())
}

/// Whether an upstream may read `c` as `/`.
fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '\u{FF0F}' | '\u{FF3C}' | '\u{FE68}')
}

/// `path` with its separators ([`SEPARATOR_ESCAPES`], `\` and the solidi
/// themselves) read as `/`, as an upstream that decodes or folds them before
/// routing does, and repeated `/` collapsed. Borrowed when there is none.
fn decode_separators(path: &str) -> Cow<'_, str> {
    let has_separator = path.chars().any(|c| c != '/' && is_separator(c))
        || path
            .match_indices('%')
            .any(|(i, _)| separator_escape_len(&path[i..]).is_some());
    if !has_separator {
        return Cow::Borrowed(path);
    }
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(c) = rest.chars().next() {
        let (sep, len) = match separator_escape_len(rest) {
            Some(len) => (true, len),
            None => (is_separator(c), c.len_utf8()),
        };
        if sep {
            if !out.ends_with('/') {
                out.push('/');
            }
        } else {
            out.push(c);
        }
        rest = &rest[len..];
    }
    Cow::Owned(out)
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
        select_route(routes, chain, upstreams, parse_method(method)?, path)
            .expect("no separator bypass")
            .map(|r| r.id)
    }

    /// [`select_route`] on POST, for the separator bypass cases.
    fn select(
        routes: &[Route],
        t: Uuid,
        u: Uuid,
        path: &str,
    ) -> Result<Option<Uuid>, SeparatorBypass> {
        select_route(routes, &[t], &[u], HttpMethod::Post, path).map(|r| r.map(|r| r.id))
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

    /// An encoded `/` or `\` cannot skip a narrower route: an upstream that
    /// decodes it before routing would serve the narrower route's path.
    #[test]
    fn encoded_separator_does_not_bypass_a_narrower_route() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1", 0), route(t, u, "/v1/admin", 0)];
        for path in [
            "/v1%2Fadmin",
            "/v1%2fadmin",
            "/v1%5Cadmin/x",
            "/v1\\admin",
            "/v1%2F%2Fadmin",
            "/v1%EF%BC%8Fadmin",
            "/v1%EF%B9%A8admin",
        ] {
            assert_eq!(select(&routes, t, u, path), Err(SeparatorBypass), "{path}");
        }
        // After the narrower prefix, or with nothing narrower to reach.
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/admin%2Fx"),
            Some(routes[1].id)
        );
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1%2Fother"),
            Some(routes[0].id)
        );

        // The root route covers everything, but not a way around `/v1/admin`.
        let routes = [route(t, u, "/", 0), route(t, u, "/v1/admin", 0)];
        assert_eq!(select(&routes, t, u, "/v1%2Fadmin"), Err(SeparatorBypass));
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v2%2Fx"),
            Some(routes[0].id)
        );

        // Decoded, the path reaches a route it does not match as sent: the
        // client is told to send it decoded, not that no route exists.
        let admin_only = [route(t, u, "/v1/admin", 0)];
        assert_eq!(
            select(&admin_only, t, u, "/v1%2Fadmin"),
            Err(SeparatorBypass)
        );
        assert_eq!(select(&admin_only, t, u, "/v2%2Fx"), Ok(None));
    }

    /// A route prefix with an encoded `/` is the same upstream path as one
    /// with `/`, so the plain form cannot reach it under a broader route.
    #[test]
    fn plain_path_does_not_bypass_an_encoded_route() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v3", 0), route(t, u, "/v3/a%2Fb", 0)];
        assert_eq!(select(&routes, t, u, "/v3/a/b"), Err(SeparatorBypass));
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v3/a%2Fb/x"),
            Some(routes[1].id)
        );
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v3/a/c"),
            Some(routes[0].id)
        );
    }

    /// Prefixes equal once decoded tie in the decoded ranking: priority
    /// picks one, and every request under the other is a bypass.
    #[test]
    fn prefixes_equal_once_decoded_leave_one_route_unreachable() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let encoded_first = [route(t, u, "/v3/a/b", 0), route(t, u, "/v3/a%2Fb", 1)];
        assert_eq!(
            select(&encoded_first, t, u, "/v3/a/b/x"),
            Err(SeparatorBypass)
        );
        let plain_first = [route(t, u, "/v3/a/b", 1), route(t, u, "/v3/a%2Fb", 0)];
        assert_eq!(
            select(&plain_first, t, u, "/v3/a/b/x"),
            Ok(Some(plain_first[0].id))
        );
        assert_eq!(
            select(&plain_first, t, u, "/v3/a%2Fb/x"),
            Err(SeparatorBypass)
        );
    }

    #[test]
    fn decode_separators_reads_escapes_and_backslash_as_slash() {
        assert!(matches!(decode_separators("/v1/chat"), Cow::Borrowed(_)));
        assert_eq!(decode_separators("/a%2Fb%2fc%5Cd%5ce\\f"), "/a/b/c/d/e/f");
        assert_eq!(decode_separators("/a%2F%2F/b"), "/a/b");
        // Fullwidth and small solidi, which NFKC folds to `/` and `\`.
        assert_eq!(
            decode_separators("/a%EF%BC%8Fb%ef%bc%bcc%EF%B9%A8d\u{FF0F}e"),
            "/a/b/c/d/e"
        );
        // Other escapes, and a double-encoded `/`, stay as they are.
        assert_eq!(
            decode_separators("/a%252Fb%20c%2F%EF%BC%8"),
            "/a%252Fb%20c/%EF%BC%8"
        );
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
    fn prefix_matches_whole_segments_only() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let chat = [route(t, u, "/v1/chat", 0)];
        assert_eq!(
            pick(&chat, &[t], &[u], "POST", "/v1/chat"),
            Some(chat[0].id)
        );
        assert_eq!(
            pick(&chat, &[t], &[u], "POST", "/v1/chat/completions"),
            Some(chat[0].id)
        );
        assert_eq!(pick(&chat, &[t], &[u], "POST", "/v1/chatty"), None);
        assert_eq!(pick(&chat, &[t], &[u], "POST", "/v1/chat-x"), None);
        assert_eq!(pick(&chat, &[t], &[u], "POST", "/v1/chat_x"), None);
        // Anything that does not continue the segment name ends it: the
        // upstream may serve these as `/v1/chat/...` or `/v1/chat`.
        for path in [
            "/v1/chat%2Fx",
            "/v1/chat%5Cx",
            "/v1/chat%252Fx",
            "/v1/chat\\x",
            "/v1/chat.json",
            "/v1/chat.",
            "/v1/chat:stream",
            "/v1/chat%EF%BC%8Fx",
        ] {
            assert_eq!(
                pick(&chat, &[t], &[u], "POST", path),
                Some(chat[0].id),
                "{path}"
            );
        }
        let v1 = [route(t, u, "/v1", 0)];
        assert_eq!(pick(&v1, &[t], &[u], "POST", "/v10"), None);
        assert_eq!(pick(&v1, &[t], &[u], "POST", "/v10/x"), None);
    }

    /// A prefix ending in `/` ends on the same segment boundary as one
    /// without, and covers the collapsed `/v1/admin` too.
    #[test]
    fn prefix_ending_in_slash_uses_the_boundary_rule() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1/admin/", 0), route(t, u, "/v1", 0)];
        for path in ["/v1/admin", "/v1/admin/users", "/v1/admin%2Fusers"] {
            assert_eq!(
                pick(&routes, &[t], &[u], "POST", path),
                Some(routes[0].id),
                "{path}"
            );
        }
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/administrator"),
            Some(routes[1].id)
        );
    }

    #[test]
    fn prefix_ending_in_slash_covers_its_subtree() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1/", 0)];
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/x"),
            Some(routes[0].id)
        );
        assert_eq!(pick(&routes, &[t], &[u], "POST", "/v10"), None);
    }

    #[test]
    fn root_route_covers_every_path_and_the_bare_alias() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/", 0)];
        assert_eq!(pick(&routes, &[t], &[u], "POST", ""), Some(routes[0].id));
        assert_eq!(pick(&routes, &[t], &[u], "POST", "/"), Some(routes[0].id));
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/any/x"),
            Some(routes[0].id)
        );
        // Only the root route covers the bare alias.
        let v1 = [route(t, u, "/v1", 0)];
        assert_eq!(pick(&v1, &[t], &[u], "POST", ""), None);
    }

    /// A prefix stored before write-time normalization ranks by its
    /// normalized length: `/v1/%63%68at` is `/v1/chat`, shorter than
    /// `/v1/chat/x`.
    #[test]
    fn stored_prefix_ranks_by_its_normal_length() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1/%63%68at", 0), route(t, u, "/v1/chat/x", 0)];
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/chat/x/y"),
            Some(routes[1].id)
        );
    }

    /// A prefix stored with encoded unreserved characters or lowercase
    /// escapes matches the normalized request path.
    #[test]
    fn stored_prefix_is_compared_in_normal_form() {
        let (t, u) = (Uuid::new_v4(), Uuid::new_v4());
        let routes = [route(t, u, "/v1/%7euser/a%2fb", 0)];
        assert_eq!(
            pick(&routes, &[t], &[u], "POST", "/v1/~user/a%2Fb/x"),
            Some(routes[0].id)
        );
    }

    #[test]
    fn parse_method_is_case_insensitive() {
        assert_eq!(parse_method("post"), Some(HttpMethod::Post));
        assert_eq!(parse_method("HEAD"), Some(HttpMethod::Head));
        assert_eq!(parse_method("OPTIONS"), Some(HttpMethod::Options));
        assert_eq!(parse_method("TRACE"), None);
        assert_eq!(parse_method("CONNECT"), None);
    }
}

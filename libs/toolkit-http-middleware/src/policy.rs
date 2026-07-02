//! Route authentication policy abstraction.
//!
//! [`security_context_middleware`](crate::auth::security_context_middleware) is
//! transport- and gear-agnostic: it does not know which routes require a tenant
//! JWT. That decision is delegated to a [`RouteAuthPolicy`] supplied via Axum
//! state at the gear/bootstrap layer.
//!
//! This replaces the earlier binary `PublicRoute` request-extension marker with
//! a per-`(method, path)` policy, matching the api-gateway's mature model: a
//! gear can mark routes public, mark them authenticated, and choose a default
//! for unmatched routes — all without touching this crate.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use http::Method;

/// Whether a route requires tenant-plane authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// No authentication required (public route). The middleware inserts an
    /// anonymous [`SecurityContext`](toolkit_security::SecurityContext) and
    /// passes the request through.
    None,
    /// Authentication required. A missing or invalid bearer token is rejected.
    Required,
}

/// Resolves the [`AuthRequirement`] for an inbound request.
///
/// Injected into
/// [`security_context_middleware`](crate::auth::security_context_middleware) via
/// Axum state at the gear/bootstrap layer. Most consumers can use the built-in
/// [`MatchitRouteAuthPolicy`]; implement this trait directly only for bespoke
/// policies. Kept object-safe so it can be shared as `Arc<dyn RouteAuthPolicy>`.
pub trait RouteAuthPolicy: Send + Sync {
    /// Resolve the authentication requirement for a `(method, path)` pair. The
    /// `path` is the gear-local path (already stripped of any nesting prefix by
    /// [`resolve_path`](crate::common::resolve_path)).
    fn resolve(&self, method: &Method, path: &str) -> AuthRequirement;
}

/// A [`RouteAuthPolicy`] backed by per-method [`matchit`] routers.
///
/// Routes are classified into two explicit sets — **authenticated** (require a
/// tenant JWT) and **public** (exempt) — with everything else governed by
/// `require_auth_by_default`. A path that matches both sets is treated as
/// **public** (an explicit public route always wins over a broad authenticated
/// pattern such as a `/{*rest}` fallback).
///
/// Build one with [`from_route_sets`](Self::from_route_sets). This is the
/// default policy for any gear serving HTTP (the api-gateway and the `OoP`
/// bootstrap HTTP server both use it); implement [`RouteAuthPolicy`] directly
/// only if you need different matching semantics.
#[derive(Clone)]
pub struct MatchitRouteAuthPolicy {
    authenticated: Arc<HashMap<Method, matchit::Router<()>>>,
    public: Arc<HashMap<Method, matchit::Router<()>>>,
    require_auth_by_default: bool,
}

impl MatchitRouteAuthPolicy {
    /// Build the policy from explicit authenticated/public `(method, path)`
    /// route sets. `require_auth_by_default` decides the requirement for routes
    /// that match neither set.
    ///
    /// Route patterns use `matchit` syntax (`{id}`, `{*rest}`); literal `:` in a
    /// segment is matched literally.
    ///
    /// # Errors
    /// Returns an error if a route pattern cannot be inserted into the matcher.
    pub fn from_route_sets(
        authenticated_routes: HashSet<(Method, String)>,
        public_routes: HashSet<(Method, String)>,
        require_auth_by_default: bool,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self {
            authenticated: Arc::new(build_matchers(authenticated_routes)?),
            public: Arc::new(build_matchers(public_routes)?),
            require_auth_by_default,
        })
    }
}

fn build_matchers(
    routes: HashSet<(Method, String)>,
) -> Result<HashMap<Method, matchit::Router<()>>, anyhow::Error> {
    let mut by_method: HashMap<Method, matchit::Router<()>> = HashMap::new();
    for (method, path) in routes {
        let matcher = by_method.entry(method).or_default();
        matcher
            .insert(path.as_str(), ())
            .map_err(|e| anyhow::anyhow!("Failed to insert route pattern '{path}': {e}"))?;
    }
    Ok(by_method)
}

impl RouteAuthPolicy for MatchitRouteAuthPolicy {
    fn resolve(&self, method: &Method, path: &str) -> AuthRequirement {
        // Explicit public wins over authenticated (e.g. a `/{*rest}` fallback).
        if self.public.get(method).is_some_and(|m| m.at(path).is_ok()) {
            return AuthRequirement::None;
        }

        if self
            .authenticated
            .get(method)
            .is_some_and(|m| m.at(path).is_ok())
        {
            return AuthRequirement::Required;
        }

        if self.require_auth_by_default {
            AuthRequirement::Required
        } else {
            AuthRequirement::None
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn policy(
        authenticated: &[(Method, &str)],
        public: &[(Method, &str)],
        require_auth_by_default: bool,
    ) -> MatchitRouteAuthPolicy {
        let to_set = |routes: &[(Method, &str)]| {
            routes
                .iter()
                .map(|(m, p)| (m.clone(), (*p).to_owned()))
                .collect::<HashSet<_>>()
        };
        MatchitRouteAuthPolicy::from_route_sets(
            to_set(authenticated),
            to_set(public),
            require_auth_by_default,
        )
        .unwrap()
    }

    #[test]
    fn literal_colon_paths_are_not_treated_as_params() {
        // Must not error: literal `:` segments are valid matchit routes.
        policy(
            &[(Method::GET, "events:poll"), (Method::GET, "events:stream")],
            &[],
            false,
        );
    }

    #[test]
    fn explicit_public_route_with_path_params_returns_none() {
        let p = policy(&[], &[(Method::GET, "/users/{id}")], true);
        assert_eq!(p.resolve(&Method::GET, "/users/42"), AuthRequirement::None);
    }

    #[test]
    fn explicit_public_route_exact_match_returns_none() {
        let p = policy(&[], &[(Method::GET, "/health")], true);
        assert_eq!(p.resolve(&Method::GET, "/health"), AuthRequirement::None);
    }

    #[test]
    fn explicit_authenticated_route_returns_required() {
        let p = policy(&[(Method::GET, "/admin/metrics")], &[], false);
        assert_eq!(
            p.resolve(&Method::GET, "/admin/metrics"),
            AuthRequirement::Required
        );
    }

    #[test]
    fn unmatched_route_follows_require_auth_by_default() {
        assert_eq!(
            policy(&[], &[], true).resolve(&Method::POST, "/unknown"),
            AuthRequirement::Required
        );
        assert_eq!(
            policy(&[], &[], false).resolve(&Method::POST, "/unknown"),
            AuthRequirement::None
        );
    }

    #[test]
    fn public_route_overrides_require_auth_by_default() {
        let p = policy(&[], &[(Method::GET, "/public")], true);
        assert_eq!(p.resolve(&Method::GET, "/public"), AuthRequirement::None);
    }

    #[test]
    fn authenticated_route_has_priority_over_default() {
        let p = policy(&[(Method::GET, "/users/{id}")], &[], false);
        assert_eq!(
            p.resolve(&Method::GET, "/users/123"),
            AuthRequirement::Required
        );
    }

    #[test]
    fn explicit_public_overrides_wildcard_authenticated_fallback() {
        let p = policy(
            &[(Method::GET, "/{*rest}")],
            &[(Method::GET, "/v1/auth/config")],
            true,
        );
        assert_eq!(
            p.resolve(&Method::GET, "/v1/auth/config"),
            AuthRequirement::None,
            "explicit public must win over wildcard authenticated fallback"
        );
        assert_eq!(
            p.resolve(&Method::GET, "/some/other/path"),
            AuthRequirement::Required,
            "wildcard authenticated still applies to non-public paths"
        );
    }

    #[test]
    fn different_methods_resolve_independently() {
        let p = policy(&[(Method::GET, "/user-management/v1/users")], &[], false);
        assert_eq!(
            p.resolve(&Method::GET, "/user-management/v1/users"),
            AuthRequirement::Required
        );
        assert_eq!(
            p.resolve(&Method::POST, "/user-management/v1/users"),
            AuthRequirement::None
        );
    }
}

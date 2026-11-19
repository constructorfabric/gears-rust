use std::collections::HashSet;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{PluginsConfig, RateLimitConfig, Route, Server, Upstream};

/// Check whether an upstream is visible to descendant tenants.
///
/// Per `cpt-cf-oagw-algo-tenant-alias-shadow` step 2b, an ancestor upstream is
/// visible if its own tenant matches the requester OR any per-field sharing flag
/// (`auth`, `rate_limit`, `circuit_breaker`, `plugins`, `cors`) is not `private`.
///
/// Returns `false` when all shareable fields are `None` — this is intentional.
/// An upstream with no auth, rate_limit, circuit_breaker, plugins, or cors has no configuration
/// to share with descendants, so it is treated as invisible. Fields without a
/// sharing mode (e.g. `headers`) do not contribute to visibility.
pub(crate) fn is_visible_to_descendant(upstream: &Upstream) -> bool {
    use crate::domain::model::SharingMode;

    let auth_shared = upstream
        .auth
        .as_ref()
        .is_some_and(|a| a.sharing != SharingMode::Private);
    let rate_shared = upstream
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing != SharingMode::Private);
    let breaker_shared = upstream
        .circuit_breaker
        .as_ref()
        .is_some_and(|c| c.sharing != SharingMode::Private);
    let plugins_shared = upstream
        .plugins
        .as_ref()
        .is_some_and(|p| p.sharing != SharingMode::Private);
    let cors_shared = upstream
        .cors
        .as_ref()
        .is_some_and(|c| c.sharing != SharingMode::Private);

    auth_shared || rate_shared || breaker_shared || plugins_shared || cors_shared
}

/// What a descendant tenant sees of an ancestor's `upstream`: the `private`
/// auth, rate limit, circuit breaker, plugins and CORS stay with their owner
/// (`inst-merge-1`). Headers have no sharing mode and are kept.
pub(super) fn shared_upstream_view(mut upstream: Upstream) -> Upstream {
    use crate::domain::model::SharingMode::Private;

    if upstream.auth.as_ref().is_some_and(|a| a.sharing == Private) {
        upstream.auth = None;
    }
    if upstream
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing == Private)
    {
        upstream.rate_limit = None;
    }
    if upstream
        .circuit_breaker
        .as_ref()
        .is_some_and(|c| c.sharing == Private)
    {
        upstream.circuit_breaker = None;
    }
    if upstream
        .plugins
        .as_ref()
        .is_some_and(|p| p.sharing == Private)
    {
        upstream.plugins = None;
    }
    if upstream.cors.as_ref().is_some_and(|c| c.sharing == Private) {
        upstream.cors = None;
    }
    upstream
}

/// What a descendant tenant sees of an ancestor's `route`: its `private`
/// plugins, rate limit and CORS stay with their owner.
pub(super) fn shared_route_view(mut route: Route) -> Route {
    use crate::domain::model::SharingMode::Private;

    if route.plugins.as_ref().is_some_and(|p| p.sharing == Private) {
        route.plugins = None;
    }
    if route
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing == Private)
    {
        route.rate_limit = None;
    }
    if route.cors.as_ref().is_some_and(|c| c.sharing == Private) {
        route.cors = None;
    }
    route
}

/// Compute the effective upstream configuration by merging ancestor upstreams
/// in the tenant chain (root → descendant).
///
/// Per `cpt-cf-oagw-algo-tenant-config-merge`:
/// - Auth:       `private` → local-only (blocked by ancestor `enforce`); `inherit` → override; `enforce` → sticky
/// - Rate limit: `private` → local-only (constrained by ancestor `enforce` via `min()`); else `min(ancestor, descendant)`
/// - Circuit breaker: as auth; `enforce` is sticky, otherwise the closest level's config wins
/// - Plugins:    `private` → local-only (ancestor `enforce` items preserved); else concatenate `ancestor + descendant`
/// - Tags:       union (add-only)
///
/// A limit or plugin an ancestor enforces stays enforced down the whole
/// chain and through the route, whatever the levels in between set.
///
/// `ancestor_chain` is ordered root-first: `[root, parent, ..., selected]`.
/// The last element is the selected (resolved) upstream. The caller removes
/// what another tenant keeps `private` ([`shared_upstream_view`]).
pub(crate) fn compute_effective_config(
    ancestor_chain: &[Upstream],
    route: Option<&Route>,
) -> Result<Upstream, DomainError> {
    compute_effective_config_reporting(ancestor_chain, route, &[], &mut |_| {}, &mut |_| {})
}

/// [`compute_effective_config`] that reports to `withheld` every ancestor
/// auth or header set it does not apply, and the enforced auth it refuses,
/// because the descendant's server is not within the owner's (`inst-merge-3a0`).
///
/// `shadowed` are the routes `route` shadows: on each upstream of the chain
/// above `route`'s, the route its owner would select for the same request.
/// Only their enforced plugins apply, so shadowing a path does not bypass
/// them. Their enforced rate limits are [`shadowed_route_limits`], checked
/// apart: the effective rate limit is the upstreams' only, whatever route
/// matched.
///
/// A route's enforced plugins apply at its owner's level, right after its
/// upstream in the root-first walk: an ancestor's enforce beats anything a
/// descendant sets, and for a `plugin_ref` enforced at several levels the
/// one closest to the root keeps its config. The same holds for `route`
/// when an ancestor owns it.
///
/// Reports to `applied` each `shadowed` route at least one of whose enforced
/// plugins enters the answer; one whose every `plugin_ref` a level above
/// already enforces changes nothing and is not reported.
pub(crate) fn compute_effective_config_reporting(
    ancestor_chain: &[Upstream],
    route: Option<&Route>,
    shadowed: &[Route],
    withheld: &mut dyn FnMut(Withheld),
    applied: &mut dyn FnMut(&Route),
) -> Result<Upstream, DomainError> {
    compute_effective_config_owned(ancestor_chain, route, shadowed, withheld, applied)
        .map(|(effective, _)| effective)
}

/// [`compute_effective_config_reporting`], with the tenant of the upstream
/// whose auth is in effect (the auth owner, `inst-merge-3a0`). The
/// per-request secret guards (ADR-0022) bound a secret's owner by it.
pub(crate) fn compute_effective_config_owned(
    ancestor_chain: &[Upstream],
    route: Option<&Route>,
    shadowed: &[Route],
    withheld: &mut dyn FnMut(Withheld),
    applied: &mut dyn FnMut(&Route),
) -> Result<(Upstream, Uuid), DomainError> {
    use crate::domain::model::{SharedPool, SharingMode};

    if ancestor_chain.is_empty() {
        return Err(DomainError::Internal {
            message: "compute_effective_config called with empty ancestor_chain".into(),
        });
    }

    // Start with the root upstream as the base.
    let mut effective = ancestor_chain[0].clone();
    if let Some(rl) = &mut effective.rate_limit {
        rl.limit_owner_id = Some(effective.id);
    }
    if let Some(cb) = &mut effective.circuit_breaker {
        cb.owner_id = Some(effective.id);
    }
    // Per `inst-merge-3a0`: the upstreams whose auth and headers are in
    // effect. They go only to endpoints their owner configured.
    let mut auth_owner = &ancestor_chain[0];
    let mut headers_owner = &ancestor_chain[0];
    // The `plugin_ref`s some level enforces: no later level or route may
    // replace or drop them.
    let mut enforced_plugins = HashSet::new();
    if let Some(plugins) = &effective.plugins {
        note_enforced_plugins(&mut enforced_plugins, plugins);
    }
    // The routes on `upstream_id` with their enforced plugins, and whether
    // they are shadowed.
    let routes_enforced_on = move |upstream_id: Uuid| {
        shadowed
            .iter()
            .map(|r| (r, true))
            .chain(route.map(|r| (r, false)))
            .filter(move |(r, _)| r.upstream_id == upstream_id)
            .filter_map(|(r, is_shadowed)| Some((r, enforced_route_plugins(r)?, is_shadowed)))
    };
    for (r, plugins, is_shadowed) in routes_enforced_on(ancestor_chain[0].id) {
        if merge_plugins(&mut effective.plugins, plugins, &mut enforced_plugins) && is_shadowed {
            applied(r);
        }
    }

    // Walk root → descendant, merging each layer.
    for layer in &ancestor_chain[1..] {
        let auth_enforced = effective
            .auth
            .as_ref()
            .is_some_and(|a| a.sharing == SharingMode::Enforce);
        if auth_enforced {
            // An enforced auth must be applied, so it must be applicable.
            if !server_within(&layer.server, &auth_owner.server) {
                withheld(Withheld::of(auth_owner, layer, WithheldKind::EnforcedAuth));
                return Err(DomainError::forbidden_with_reason(
                    ENFORCED_AUTH_OTHER_SERVER,
                    format!(
                        "upstream '{}' serves alias '{}' on endpoints upstream '{}' did not \
                         configure, and its enforced auth cannot be sent there",
                        layer.id, layer.alias, auth_owner.id
                    ),
                ));
            }
        } else if layer.auth.is_some() {
            auth_owner = layer;
        } else if effective.auth.is_some() && !server_within(&layer.server, &auth_owner.server) {
            effective.auth = None;
            withheld(Withheld::of(auth_owner, layer, WithheldKind::Auth));
        }
        if layer.headers.is_some() {
            headers_owner = layer;
        } else if effective.headers.is_some()
            && !server_within(&layer.server, &headers_owner.server)
        {
            effective.headers = None;
            withheld(Withheld::of(headers_owner, layer, WithheldKind::Headers));
        }

        // Auth merge
        merge_auth(&mut effective, layer);

        // Rate limit merge
        merge_rate_limit(&mut effective, layer);

        merge_circuit_breaker(&mut effective, layer);

        // Plugins merge
        if let Some(plugins) = &layer.plugins {
            merge_plugins(&mut effective.plugins, plugins, &mut enforced_plugins);
        }
        for (r, plugins, is_shadowed) in routes_enforced_on(layer.id) {
            if merge_plugins(&mut effective.plugins, plugins, &mut enforced_plugins) && is_shadowed
            {
                applied(r);
            }
        }

        // CORS merge
        merge_cors(&mut effective, layer)?;

        // Tags: union (add-only)
        for tag in &layer.tags {
            if !effective.tags.contains(tag) {
                effective.tags.push(tag.clone());
            }
        }

        // Server, protocol, enabled, alias: always use the selected upstream's values.
        effective.id = layer.id;
        effective.tenant_id = layer.tenant_id;
        effective.alias = layer.alias.clone();
        effective.server = layer.server.clone();
        effective.protocol = layer.protocol.clone();
        effective.enabled = layer.enabled;
        effective.headers = layer.headers.clone().or(effective.headers);
    }

    // Defense-in-depth: if the effective config has a shared budget but
    // no pool was set (single-element chain or all layers were no-ops), the
    // effective upstream owns it.
    if let Some(ref mut rl) = effective.rate_limit
        && rl.shared_pool.is_none()
    {
        rl.shared_pool = SharedPool::defined_by(effective.id, rl);
    }

    // A shadowed route's enforced rate limit is not merged here: the proxy
    // checks it on the route's own bucket (`shadowed_route_limits`). A
    // shadowed route on no upstream of the chain still applies its enforced
    // plugins (fail closed).
    for r in shadowed
        .iter()
        .filter(|r| !ancestor_chain.iter().any(|u| u.id == r.upstream_id))
    {
        if let Some(plugins) = enforced_route_plugins(r)
            && merge_plugins(&mut effective.plugins, plugins, &mut enforced_plugins)
        {
            applied(r);
        }
    }

    // Route-level overrides (route > upstream base per config layering).
    if let Some(route) = route {
        // Route plugins: as one more level, so enforced items stay. Its
        // enforced ones are already in, at its owner's level.
        if let Some(ref route_plugins) = route.plugins {
            merge_plugins(&mut effective.plugins, route_plugins, &mut enforced_plugins);
        }

        // The route's rate limit is not merged in: the proxy checks it on the
        // route's own bucket next to the upstream's (`inst-merge-5a`), so
        // routes with different limits do not reconfigure the upstream bucket.

        // Route CORS: follows same sharing semantics as upstream-level merge.
        // Per inst-merge-3a5: private → skip; inherit → union origins;
        // enforce → use upstream CORS (keep effective unchanged).
        if let Some(ref route_cors) = route.cors {
            let effective_is_enforced = effective
                .cors
                .as_ref()
                .is_some_and(|c| c.sharing == SharingMode::Enforce);
            if !effective_is_enforced {
                match route_cors.sharing {
                    SharingMode::Private | SharingMode::Enforce => {
                        // Private → skip; Enforce → use upstream CORS.
                    }
                    SharingMode::Inherit => {
                        let mut merged = route_cors.clone();
                        if let Some(ref upstream_cors) = effective.cors {
                            for origin in &upstream_cors.allowed_origins {
                                if !merged.allowed_origins.contains(origin) {
                                    merged.allowed_origins.push(origin.clone());
                                }
                            }
                        }
                        crate::domain::cors::validate_cors_config(&merged)?;
                        effective.cors = Some(merged);
                    }
                }
            }
        }

        // Route tags: union.
        for tag in &route.tags {
            if !effective.tags.contains(tag) {
                effective.tags.push(tag.clone());
            }
        }
    }

    let auth_owner_tenant_id = auth_owner.tenant_id;
    Ok((effective, auth_owner_tenant_id))
}

/// The enforced rate limits of the `shadowed` routes (as given to
/// [`compute_effective_config_reporting`]), in their order. The proxy checks
/// each on its route's own bucket, as if that route had matched
/// (`inst-merge-2a`); merged into the upstream limit, they would give the
/// upstream bucket a config that depends on the request path.
pub(crate) fn shadowed_route_limits(
    shadowed: &[Route],
) -> Vec<crate::domain::model::ShadowedRouteLimit> {
    use crate::domain::model::{ShadowedRouteLimit, SharingMode};

    shadowed
        .iter()
        .filter_map(|route| {
            let rate_limit = route.rate_limit.as_ref()?;
            (rate_limit.sharing == SharingMode::Enforce).then(|| ShadowedRouteLimit {
                route_id: route.id,
                rate_limit: rate_limit.clone(),
            })
        })
        .collect()
}

/// Whether every endpoint of `server` is one of `owner`'s, so a credential
/// `owner` configured may be sent to it. Endpoints compare by normalized host
/// (case-insensitive, no trailing dot or IPv6 brackets), port and scheme. One
/// way only: an endpoint `owner` does not list makes it another server, and so
/// does an empty `server`.
pub(super) fn server_within(server: &Server, owner: &Server) -> bool {
    if server.endpoints.is_empty() {
        return false;
    }
    let key = |e: &crate::domain::model::Endpoint| (e.normalized_host(), e.port, e.scheme as u8);
    let owner: std::collections::HashSet<_> = owner.endpoints.iter().map(key).collect();
    server.endpoints.iter().all(|e| owner.contains(&key(e)))
}

/// `permission_denied` reason when an enforced auth would go to endpoints its
/// owner did not configure.
pub(crate) const ENFORCED_AUTH_OTHER_SERVER: &str =
    oagw_sdk::reason::permission::ENFORCED_AUTH_OTHER_SERVER;

/// An ancestor's auth or headers the merge did not apply to a descendant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withheld {
    /// The upstream that configured the withheld setting.
    pub(crate) owner_id: Uuid,
    /// The descendant upstream on other endpoints.
    pub(crate) upstream_id: Uuid,
    /// The shared alias.
    pub(crate) alias: String,
    /// What was withheld.
    pub(crate) kind: WithheldKind,
}

/// The setting a [`Withheld`] is about.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WithheldKind {
    /// The owner's `auth`.
    Auth,
    /// The owner's request `headers`.
    Headers,
    /// The owner's enforced `auth`: the request is refused.
    EnforcedAuth,
}

impl Withheld {
    fn of(owner: &Upstream, layer: &Upstream, kind: WithheldKind) -> Self {
        Self {
            owner_id: owner.id,
            upstream_id: layer.id,
            alias: layer.alias.clone(),
            kind,
        }
    }
}

/// Merge auth config from a descendant layer onto the effective config.
///
/// Key invariant: once an ancestor sets `enforce`, no descendant can override
/// regardless of the descendant's own sharing mode.  This is defense-in-depth;
/// `validate_bind_constraints` also guards this at create/update time.
///
/// Sharing semantics:
/// - `Private` + ancestor enforced → keep ancestor (enforce is sticky)
/// - `Private` + ancestor not enforced → descendant replaces (local-only)
/// - `Inherit` → descendant overrides ancestor
/// - `Enforce` → descendant's enforce becomes sticky for further descendants
fn merge_auth(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .auth
        .as_ref()
        .is_some_and(|a| a.sharing == SharingMode::Enforce);

    match &layer.auth {
        None => {} // Absent → inherit from previous level (no-op).
        Some(_) if effective_is_enforced => {
            // Ancestor enforced — no descendant can change it regardless of sharing mode.
        }
        Some(descendant_auth) => {
            // Private → local-only replace; Inherit → override; Enforce → becomes sticky.
            effective.auth = Some(descendant_auth.clone());
        }
    }
}

/// Merge circuit breaker config: the closest level's replaces the ancestor's
/// unless an ancestor enforces its own, which then stays for every level
/// below (as [`merge_auth`]). `owner_id` names the level whose config is in
/// effect.
///
/// No `min()` as for rate limits: thresholds and the timeout pull in
/// different directions, so there is no one stricter config.
fn merge_circuit_breaker(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::{CircuitBreakerConfig, SharingMode};

    let effective_is_enforced = effective
        .circuit_breaker
        .as_ref()
        .is_some_and(|c| c.sharing == SharingMode::Enforce);
    match &layer.circuit_breaker {
        None => {}
        Some(_) if effective_is_enforced => {}
        Some(cb) => {
            effective.circuit_breaker = Some(CircuitBreakerConfig {
                owner_id: Some(layer.id),
                ..cb.clone()
            });
        }
    }
}

/// Merge rate limit config: `min(ancestor_enforced, descendant)`.
///
/// Key invariant: if the effective rate limit is already `Enforce`, a
/// descendant `Private` cannot drop it — `min()` is applied instead — and
/// the result stays `Enforce` for the levels below. `validate_bind_constraints`
/// refuses an override only under an enforcing closest ancestor, so this is
/// the only guard against a farther one.
///
/// When the effective rate limit has `budget.mode == Shared`, its pool
/// (owned by the effective upstream) is recorded in `shared_pool` so all
/// descendants draw on one pool bucket at runtime.
///
/// `limit_owner_id` becomes `layer`'s ID when its limit replaces the
/// effective one or wins the `min`, and stays the ancestor's otherwise.
pub(super) fn merge_rate_limit(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::{SharedPool, SharingMode};

    // Capture the shared pool before merging (the ancestor that defines the
    // shared budget owns it).
    let pool = effective
        .rate_limit
        .as_ref()
        .and_then(|rl| SharedPool::defined_by(effective.id, rl));

    let effective_is_enforced = effective
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing == SharingMode::Enforce);

    match &layer.rate_limit {
        None => {} // Absent = inherit from previous level (no-op).
        Some(descendant_rl) => {
            let mut merged = match descendant_rl.sharing {
                // Local-only, unless an ancestor enforces its limit: then min.
                SharingMode::Private if !effective_is_enforced => RateLimitConfig {
                    limit_owner_id: Some(layer.id),
                    ..descendant_rl.clone()
                },
                SharingMode::Private | SharingMode::Inherit | SharingMode::Enforce => {
                    min_rate_limit(effective.rate_limit.as_ref(), descendant_rl, layer.id)
                }
            };
            // `min` takes the winner's whole config: an enforced limit stays
            // enforced for the levels below, whichever value won.
            if effective_is_enforced || descendant_rl.sharing == SharingMode::Enforce {
                merged.sharing = SharingMode::Enforce;
            }
            effective.rate_limit = Some(merged);
        }
    }

    // Propagate the shared pool to descendants so the proxy draws on the
    // defining ancestor's pool bucket. Only set once: the first ancestor
    // that defines BudgetMode::Shared pins the pool; later merges must not
    // overwrite it.
    //
    // Skip for non-enforced Private: the descendant opted out of the
    // shared pool and should use its own independent bucket.
    let is_private_override = layer
        .rate_limit
        .as_ref()
        .is_some_and(|rl| rl.sharing == SharingMode::Private)
        && !effective_is_enforced;

    if !is_private_override
        && let Some(pool) = pool
        && let Some(ref mut rl) = effective.rate_limit
        && rl.shared_pool.is_none()
    {
        rl.shared_pool = Some(pool);
    }
}

/// The rate limit [`compute_effective_config`] gives `chain` (root-first,
/// the selected upstream last), shared pool and limit owner included,
/// without merging the other fields.
pub(super) fn effective_rate_limit(
    chain: &[Upstream],
) -> Option<crate::domain::model::RateLimitConfig> {
    use crate::domain::model::SharedPool;

    let (root, rest) = chain.split_first()?;
    let mut effective = root.clone();
    if let Some(rl) = &mut effective.rate_limit {
        rl.limit_owner_id = Some(effective.id);
    }
    for layer in rest {
        merge_rate_limit(&mut effective, layer);
        effective.id = layer.id;
    }
    let mut rate_limit = effective.rate_limit?;
    if rate_limit.shared_pool.is_none() {
        rate_limit.shared_pool = SharedPool::defined_by(effective.id, &rate_limit);
    }
    Some(rate_limit)
}

/// Merge CORS config from a descendant layer onto the effective config.
///
/// Per `inst-merge-3a5` (feature 0005 — tenant hierarchy):
/// - `Private`  → skip (do not modify effective).
/// - Absent     → inherit from previous level; Private must not propagate.
/// - `Inherit`  → descendant config wins, `allowed_origins` is the union
///   of ancestor + descendant origins (deduped); Private ancestor origins
///   are excluded from the union.
/// - `Enforce`  → use ancestor CORS (keep effective unchanged).
///
/// Ancestor enforce is sticky: once effective is `Enforce`, no descendant
/// can change it regardless of sharing mode.
fn merge_cors(effective: &mut Upstream, layer: &Upstream) -> Result<(), DomainError> {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .cors
        .as_ref()
        .is_some_and(|c| c.sharing == SharingMode::Enforce);

    match &layer.cors {
        None => {
            // Absent → inherit from previous level, but Private must not propagate.
            if effective
                .cors
                .as_ref()
                .is_some_and(|c| c.sharing == SharingMode::Private)
            {
                effective.cors = None;
            }
        }
        Some(_) if effective_is_enforced => {
            // Ancestor enforced — no descendant can change it.
        }
        Some(descendant_cors) => match descendant_cors.sharing {
            SharingMode::Private => {
                // Per inst-merge-3a5: private → skip (do not modify effective).
            }
            SharingMode::Enforce => {
                // Per inst-merge-3a5: enforce → use ancestor CORS (keep effective unchanged).
            }
            SharingMode::Inherit => {
                // Union allowed_origins from ancestor + descendant, skipping Private ancestor.
                let mut merged = descendant_cors.clone();
                if let Some(ref ancestor) = effective.cors
                    && ancestor.sharing != SharingMode::Private
                {
                    for origin in &ancestor.allowed_origins {
                        if !merged.allowed_origins.contains(origin) {
                            merged.allowed_origins.push(origin.clone());
                        }
                    }
                }
                crate::domain::cors::validate_cors_config(&merged)?;
                effective.cors = Some(merged);
            }
        },
    }

    Ok(())
}

/// Return the stricter of two rate limit configs (lower rate wins; `a` on a
/// tie). `b` winning makes `b_owner` its `limit_owner_id`.
fn min_rate_limit(
    a: Option<&RateLimitConfig>,
    b: &RateLimitConfig,
    b_owner: Uuid,
) -> RateLimitConfig {
    match a {
        None => RateLimitConfig {
            limit_owner_id: Some(b_owner),
            ..b.clone()
        },
        Some(a) => {
            let a_rate = rate_per_second(a);
            let b_rate = rate_per_second(b);
            if b_rate < a_rate {
                let mut winner = b.clone();
                winner.limit_owner_id = Some(b_owner);
                // Keep the shared pool of the existing effective config when
                // a stricter descendant wins.
                if winner.shared_pool.is_none() {
                    winner.shared_pool = a.shared_pool;
                }
                winner
            } else {
                a.clone()
            }
        }
    }
}

/// Convert a window enum to its duration in seconds.
pub(in crate::domain::services) fn window_to_secs(window: crate::domain::model::Window) -> f64 {
    use crate::domain::model::Window;
    match window {
        Window::Second => 1.0,
        Window::Minute => 60.0,
        Window::Hour => 3600.0,
        Window::Day => 86400.0,
    }
}

/// Normalize a rate limit to requests-per-second for comparison.
pub(in crate::domain::services) fn rate_per_second(
    rl: &crate::domain::model::RateLimitConfig,
) -> f64 {
    f64::from(rl.sustained.rate) / window_to_secs(rl.sustained.window)
}

/// Merge one level's plugins (an upstream's or the route's) onto the
/// effective ones, dedup by `plugin_ref`.
///
/// - `private` → local-only: the inherited items go, except the enforced ones.
/// - `inherit` / `enforce` → concatenate `ancestor + descendant`; the
///   descendant's config wins on a `plugin_ref` conflict.
///
/// An item in `enforced` is never dropped, and on a conflict it keeps its
/// config. The level's items join `enforced` when it enforces them. The
/// result is `enforce` while anything in it is enforced.
///
/// Returns whether any of the level's items entered.
fn merge_plugins(
    effective: &mut Option<PluginsConfig>,
    level: &PluginsConfig,
    enforced: &mut HashSet<String>,
) -> bool {
    use crate::domain::model::SharingMode;

    let inherited = effective.take().map(|p| p.items).unwrap_or_default();
    let mut items: Vec<_> = match level.sharing {
        SharingMode::Private => inherited
            .into_iter()
            .filter(|item| enforced.contains(&item.plugin_ref))
            .collect(),
        SharingMode::Inherit | SharingMode::Enforce => inherited,
    };
    let mut entered = false;
    for item in &level.items {
        if enforced.contains(&item.plugin_ref) {
            continue;
        }
        entered = true;
        if let Some(existing) = items.iter_mut().find(|m| m.plugin_ref == item.plugin_ref) {
            *existing = item.clone();
        } else {
            items.push(item.clone());
        }
    }
    note_enforced_plugins(enforced, level);
    let sharing = if enforced.is_empty() {
        level.sharing
    } else {
        SharingMode::Enforce
    };
    *effective = Some(PluginsConfig { sharing, items });
    entered
}

/// `route`'s plugins when it enforces them: what it applies at its owner's
/// level of the merge, also to a descendant's route that shadows it.
pub(super) fn enforced_route_plugins(route: &Route) -> Option<&PluginsConfig> {
    route
        .plugins
        .as_ref()
        .filter(|p| p.sharing == crate::domain::model::SharingMode::Enforce)
}

/// Add `plugins`' refs to `enforced` when they are enforced.
fn note_enforced_plugins(enforced: &mut HashSet<String>, plugins: &PluginsConfig) {
    if plugins.sharing == crate::domain::model::SharingMode::Enforce {
        enforced.extend(plugins.items.iter().map(|i| i.plugin_ref.clone()));
    }
}

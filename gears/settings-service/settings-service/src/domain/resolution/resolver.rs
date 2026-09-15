// Created: 2026-09-06 by Constructor Tech
//! The Value Resolver: cache-first, then a dispatch on scope class.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use settings_service_sdk::{EffectiveSource, SettingKey};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{
    EffectiveCache, EffectiveValue, OwnRow, ScopeTarget, TenantHierarchy, TrailEntry, scope_class,
    scope_path,
};
use crate::domain::access::{AccessRepository, EffectiveAccess, strictest};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::validation::TypeValidator;
use crate::domain::value::{StoredValue, ValueRepository};

/// The ancestry one read, or one batch, resolves against.
///
/// The chain is fetched at most once and only when a cascading declaration
/// needs it: a `global` or `local` setting never asks the tenant resolver.
pub struct Ancestry {
    root: Uuid,
    tenant: Uuid,
    chain: Option<Vec<Uuid>>,
}

impl Ancestry {
    /// For a target, given the root tenant.
    #[must_use]
    pub fn new(root: Uuid, target: ScopeTarget) -> Self {
        Self {
            root,
            tenant: target.tenant_id(root),
            chain: None,
        }
    }

    /// The requested tenant.
    #[must_use]
    pub fn tenant(&self) -> Uuid {
        self.tenant
    }

    /// The root tenant.
    #[must_use]
    pub fn root(&self) -> Uuid {
        self.root
    }

    /// The ancestor chain root to self, fetched on first use.
    async fn chain(&mut self, hierarchy: &dyn TenantHierarchy) -> Result<&[Uuid], DomainError> {
        if self.chain.is_none() {
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-6
            let chain = if self.tenant == self.root {
                vec![self.root]
            } else {
                hierarchy.chain(self.tenant).await?
            };
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-6
            self.chain = Some(chain);
        }
        Ok(self.chain.as_deref().unwrap_or(&[]))
    }
}

/// What the dispatch produced, before traits and caching.
struct Walk {
    value: Value,
    source: EffectiveSource,
    source_tenant: Option<Uuid>,
    secret_backed: bool,
    trail: Vec<TrailEntry>,
    resolved_row_last_change_at: Option<time::OffsetDateTime>,
    own_row: Option<OwnRow>,
}

/// The resolver.
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-effective:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-operations:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-scope-class:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-ancestry:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-defaults:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-fallthrough:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-outcomes:p1
pub struct ValueResolver<D, V, A> {
    declarations: D,
    values: V,
    access: A,
    hierarchy: Arc<dyn TenantHierarchy>,
    platform: Arc<dyn PlatformScope>,
    validator: Arc<dyn TypeValidator>,
    cache: Arc<EffectiveCache>,
}

/// The value a row carries, in the form a reader may see.
///
/// A secret row yields its opaque handle token — the reference into the
/// Credential Store — never plaintext; the plaintext path is a separate,
/// per-setting authorized call.
fn value_of(row: &StoredValue) -> (Value, bool) {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-10
    match (&row.value, &row.secret_ref) {
        (Some(value), _) => (value.clone(), false),
        (None, Some(secret_ref)) => (Value::String(secret_ref.clone()), true),
        (None, None) => (Value::Null, false),
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-10
}

/// Pick the effective row among the scopes inspected and build the trail.
///
/// `inspected` is ordered root to self; the deepest valid override wins.
fn select(
    declaration: &Declaration,
    tenant: Uuid,
    root: Uuid,
    inspected: &[Uuid],
    rows: &HashMap<Uuid, StoredValue>,
) -> Walk {
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-8
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-1
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-2
    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-3
    // Deepest first. A row flagged for review is skipped without being served
    // and without an error: the walk simply continues to the next nearest
    // scope, which for a cascading setting is the next ancestor and for the
    // others is nothing at all.
    let winner = inspected
        .iter()
        .rev()
        .filter_map(|t| rows.get(t))
        .find(|row| !row.needs_review);
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-3
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-2
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-1
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-8

    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-4
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-5
    // The trail is exactly the scopes inspected — the caller's own chain from
    // the root down — so a sibling or a descendant can never appear on it.
    let trail = inspected
        .iter()
        .map(|t| {
            let row = rows.get(t);
            TrailEntry {
                tenant_id: *t,
                scope: scope_path(*t, root),
                has_override: row.is_some(),
                provided_value: winner.is_some_and(|w| w.tenant_id == *t),
                needs_review: row.is_some_and(|r| r.needs_review),
                set_by: row.map(|r| r.set_by.clone()),
                last_change_at: row.map(|r| r.last_change_at),
            }
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-5
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-4

    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-5
    // The flagged row stays where it is: excluded from the walk above, but
    // reported on the requested scope's own state so an administrator sees it.
    let own_row = rows.get(&tenant).map(|r| OwnRow {
        needs_review: r.needs_review,
        needs_review_detail: r.needs_review_detail.clone(),
        last_change_at: r.last_change_at,
        updated_at: r.updated_at,
    });
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-5

    match winner {
        Some(row) => {
            let (value, secret_backed) = value_of(row);
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-9
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-10
            let source = if row.tenant_id == tenant {
                EffectiveSource::OwnOverride
            } else {
                EffectiveSource::Inherited
            };
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-10
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-9
            // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-8
            // The value arm of the recency indicator is the resolved row's own
            // timestamp: within the caller's chain by construction, never a
            // maximum over scopes it could not read.
            let resolved_row_last_change_at = Some(row.last_change_at);
            // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-8
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-6
            Walk {
                value,
                source,
                source_tenant: Some(row.tenant_id),
                secret_backed,
                trail,
                resolved_row_last_change_at,
                own_row,
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-6
        }
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-4
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-11
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-14
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-4
        // Every path ends here when no valid override exists: the Schema
        // Default, which lives on the declaration and is independent of any
        // override ever set or cleared. The source says so; the value cannot,
        // because a type admitting `null` may have `null` as a set value.
        None => Walk {
            value: declaration.default_value.clone(),
            source: EffectiveSource::SchemaDefault,
            source_tenant: None,
            secret_backed: false,
            trail,
            resolved_row_last_change_at: None,
            own_row,
        },
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough:p1:inst-vr-nrf-4
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-14
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-11
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-4
    }
}

fn index(rows: Vec<StoredValue>) -> HashMap<Uuid, StoredValue> {
    rows.into_iter().map(|r| (r.tenant_id, r)).collect()
}

fn same_detail(err: &DomainError) -> DomainError {
    match err {
        DomainError::Unavailable { detail } => DomainError::Unavailable {
            detail: detail.clone(),
        },
        other => DomainError::Internal {
            diagnostic: other.to_string(),
        },
    }
}

impl<D, V, A> ValueResolver<D, V, A>
where
    D: DeclarationRepository,
    V: ValueRepository,
    A: AccessRepository,
{
    /// Build the resolver over its repositories, ports and cache.
    pub fn new(
        declarations: D,
        values: V,
        access: A,
        hierarchy: Arc<dyn TenantHierarchy>,
        platform: Arc<dyn PlatformScope>,
        validator: Arc<dyn TypeValidator>,
        cache: Arc<EffectiveCache>,
    ) -> Self {
        Self {
            declarations,
            values,
            access,
            hierarchy,
            platform,
            validator,
            cache,
        }
    }

    /// A tenant's effective access for one declaration: the strictest row on
    /// its root-to-self chain, `overridable` when there is none.
    ///
    /// The chain is the same lookup the cascading walk uses, and it is walked
    /// for `local` and `global` settings too: they do not inherit their value,
    /// but their administrative access still narrows down the tree.
    ///
    /// # Errors
    /// [`DomainError`] when the chain or the rows cannot be read.
    pub async fn effective_access<C: DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
        target: ScopeTarget,
    ) -> Result<EffectiveAccess, DomainError> {
        let mut by_declaration = self
            .effective_access_for(conn, &[declaration_id], target)
            .await?;
        Ok(by_declaration
            .remove(&declaration_id)
            .unwrap_or(EffectiveAccess::OVERRIDABLE))
    }

    /// Effective access of one tenant for several declarations, over one chain
    /// lookup and one set query.
    ///
    /// # Errors
    /// [`DomainError`] when the chain or the rows cannot be read.
    pub async fn effective_access_for<C: DBRunner>(
        &self,
        conn: &C,
        declaration_ids: &[Uuid],
        target: ScopeTarget,
    ) -> Result<HashMap<Uuid, EffectiveAccess>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-1
        let root = self.platform.root_tenant().await?;
        let mut ancestry = Ancestry::new(root, target);
        let chain = ancestry.chain(self.hierarchy.as_ref()).await?.to_vec();
        // @cpt-end:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-1
        let rows = self
            .access
            .find_for_declarations(conn, &AccessScope::allow_all(), declaration_ids, &chain)
            .await?;
        let mut grouped: HashMap<Uuid, Vec<crate::domain::access::Restriction>> = HashMap::new();
        for row in rows {
            grouped.entry(row.declaration_id).or_default().push(row);
        }
        Ok(declaration_ids
            .iter()
            .map(|id| {
                let effective = grouped
                    .get(id)
                    .map_or(EffectiveAccess::OVERRIDABLE, |rows| strictest(rows));
                (*id, effective)
            })
            .collect())
    }

    /// The cache this resolver reads through.
    #[must_use]
    pub fn cache(&self) -> &Arc<EffectiveCache> {
        &self.cache
    }

    /// The hierarchy port, for callers that gate a target before resolving.
    #[must_use]
    pub fn hierarchy(&self) -> &Arc<dyn TenantHierarchy> {
        &self.hierarchy
    }

    /// The root tenant, whose id is platform scope.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the tenant resolver cannot answer.
    pub async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        self.platform.root_tenant().await
    }

    /// Resolve one setting at a target, cache first.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when no declaration exists at the key;
    /// [`DomainError::Retired`] when it is retired; [`DomainError::Unavailable`]
    /// when a dependency of the walk cannot answer — never a substituted Schema
    /// Default, which lives in the same database and is equally unreachable.
    pub async fn resolve<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
        target: ScopeTarget,
    ) -> Result<Arc<EffectiveValue>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-1
        let root = self.platform.root_tenant().await?;
        let mut ancestry = Ancestry::new(root, target);
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-1
        self.resolve_shared(conn, key, &mut ancestry).await
    }

    /// Resolve several settings at one target, sharing one ancestry walk.
    ///
    /// One outcome per key: a key that fails leaves the others intact, and the
    /// batch never collapses to a single failure.
    pub async fn resolve_bulk<C: DBRunner>(
        &self,
        conn: &C,
        keys: &[SettingKey],
        target: ScopeTarget,
    ) -> Vec<(SettingKey, Result<Arc<EffectiveValue>, DomainError>)> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-1
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-2
        let root = match self.platform.root_tenant().await {
            Ok(root) => root,
            Err(err) => {
                return keys
                    .iter()
                    .map(|k| (k.clone(), Err(same_detail(&err))))
                    .collect();
            }
        };
        let mut ancestry = Ancestry::new(root, target);
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-2
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-1
        let mut out = Vec::with_capacity(keys.len());
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-3
        for key in keys {
            // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-4
            let outcome = self.resolve_shared(conn, key, &mut ancestry).await;
            // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-4
            // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-5
            out.push((key.clone(), outcome));
            // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-5
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-6
        out
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve-bulk:p1:inst-vr-bulk-6
    }

    /// Resolve declarations already loaded — a browse page — at one target,
    /// sharing one ancestry walk; one outcome per declaration.
    pub async fn resolve_declarations<C: DBRunner>(
        &self,
        conn: &C,
        declarations: &[Declaration],
        target: ScopeTarget,
    ) -> Vec<Result<Arc<EffectiveValue>, DomainError>> {
        let root = match self.platform.root_tenant().await {
            Ok(root) => root,
            Err(err) => {
                return declarations
                    .iter()
                    .map(|_| Err(same_detail(&err)))
                    .collect();
            }
        };
        let mut ancestry = Ancestry::new(root, target);
        let mut out = Vec::with_capacity(declarations.len());
        for declaration in declarations {
            out.push(self.resolve_loaded(conn, declaration, &mut ancestry).await);
        }
        out
    }

    /// The declaration at a key, whatever its status, or nothing.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn find_declaration<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
    ) -> Result<Option<Declaration>, DomainError> {
        self.declarations
            .find_by_key(conn, &AccessScope::allow_all(), key.as_str())
            .await
    }

    /// The declarations filed under a category — the bulk read's other selector.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn declarations_in_category<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        category_id: Uuid,
    ) -> Result<Vec<Declaration>, DomainError> {
        self.declarations
            .find_by_category(conn, scope, category_id)
            .await
    }

    /// A page of declarations for the administrative browse, under the
    /// caller's scope constraints and administrative-domain visibility.
    ///
    /// # Errors
    /// [`DomainError::Validation`] on an unmapped field or unsupported
    /// operator in the query; [`DomainError`] when the read fails.
    pub async fn list_declarations<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<Declaration>, DomainError> {
        let visible = crate::domain::category::visibility::domain_visibility(scope);
        self.declarations.list(conn, scope, &visible, query).await
    }

    /// The overrides flagged for review among `declaration_ids` at any of
    /// `tenant_ids` — the administrative needs-review listing.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn flagged_overrides<C: DBRunner>(
        &self,
        conn: &C,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
    ) -> Result<Vec<StoredValue>, DomainError> {
        self.values
            .list_flagged(conn, &AccessScope::allow_all(), declaration_ids, tenant_ids)
            .await
    }

    /// Where a setting's value comes from at a target, with the trail.
    ///
    /// # Errors
    /// As [`Self::resolve`].
    pub async fn effective_source<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
        target: ScopeTarget,
    ) -> Result<(EffectiveSource, Option<String>, Vec<TrailEntry>), DomainError> {
        let resolved = self.resolve(conn, key, target).await?;
        Ok((
            resolved.source,
            resolved.source_scope.clone(),
            resolved.trail.clone(),
        ))
    }

    async fn resolve_shared<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
        ancestry: &mut Ancestry,
    ) -> Result<Arc<EffectiveValue>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-2
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-3
        if let Some(hit) = self.cache.get(key.as_str(), ancestry.tenant) {
            return Ok(hit);
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-3
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-2
        let declaration = self.declaration(conn, key).await?;
        self.resolve_loaded(conn, &declaration, ancestry).await
    }

    /// The declaration behind a key, or the outcome that stands in for it.
    async fn declaration<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
    ) -> Result<Declaration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-4
        // Declarations carry no tenant column and runtime configuration is not
        // an administrative read: the walk sees every declaration, and the
        // administrative surface applies its own visibility before calling.
        let found = self
            .declarations
            .find_by_key(conn, &AccessScope::allow_all(), key.as_str())
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-4
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-5
        // No row is all the service observes. Whether the owning gear has yet
        // to register or the key never existed is the consumer's to settle
        // from its own boot ordering; nothing here guesses.
        let Some(declaration) = found else {
            return Err(DomainError::NotFound {
                resource: "declaration",
            });
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-5
        Ok(declaration)
    }

    async fn resolve_loaded<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        ancestry: &mut Ancestry,
    ) -> Result<Arc<EffectiveValue>, DomainError> {
        if let Some(hit) = self.cache.get(&declaration.key, ancestry.tenant) {
            return Ok(hit);
        }
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-6
        // A positive fact, distinct from not-found; the retained values stay
        // in the table and are not returned.
        if declaration.status == "retired" {
            return Err(DomainError::Retired {
                key: declaration.key.clone(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-6
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-7
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-8
        // A failing dependency propagates as it is. The Schema Default is not
        // substituted: it lives in the same database as the rows the walk
        // could not read, and a value served from a half-answered walk would
        // be a guess dressed as a resolution.
        let walk = self.dispatch(conn, declaration, ancestry).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-8
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-7
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-9
        let traits = self.traits(&declaration.value_type_id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-9
        let (root, tenant) = (ancestry.root, ancestry.tenant);
        let effective = Arc::new(EffectiveValue {
            key: declaration.key.clone(),
            declaration_id: declaration.id,
            scope: scope_path(tenant, root),
            tenant_id: tenant,
            value: walk.value,
            source: walk.source,
            source_scope: walk.source_tenant.map(|t| scope_path(t, root)),
            traits,
            trail: walk.trail,
            data_classification: declaration.data_classification.clone(),
            domain_affinity: declaration.domain_affinity.clone(),
            secret_backed: walk.secret_backed,
            declaration_last_change_at: declaration.last_change_at,
            resolved_row_last_change_at: walk.resolved_row_last_change_at,
            own_row: walk.own_row,
        });
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-11
        self.cache.populate(Arc::clone(&effective));
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-11
        // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-12
        Ok(effective)
        // @cpt-end:cpt-cf-settings-service-flow-value-resolution-resolve:p1:inst-vr-resolve-12
    }

    /// The value type's trait set, for rendering.
    ///
    /// A type the registry no longer knows degrades to an empty set rather
    /// than failing the read: the value still resolves, only its rendering
    /// metadata is gone. An unreachable registry is unavailability, as for any
    /// dependency of the walk.
    async fn traits(&self, value_type_id: &str) -> Result<Value, DomainError> {
        match self.validator.resolve_traits(value_type_id).await {
            // A type without traits carries `null`; the result always carries
            // an object, so a client renders it without a special case.
            Ok(traits) if traits.raw.is_object() => Ok(traits.raw),
            Ok(_) | Err(DomainError::Validation { .. }) => {
                Ok(Value::Object(serde_json::Map::new()))
            }
            Err(other) => Err(other),
        }
    }

    /// Scope-class resolution dispatch: a total dispatch over the three
    /// classes, each ending in the Schema Default.
    async fn dispatch<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        ancestry: &mut Ancestry,
    ) -> Result<Walk, DomainError> {
        // Inheritance crosses tenant boundaries by design — the walk reads an
        // ancestor's rows on the requested tenant's behalf — so the rows are
        // read unscoped here. Who may ask is decided at the door, by the
        // administrative surface's authorization or by the in-process trust
        // boundary, never by narrowing which rows the walk may see.
        let scope = AccessScope::allow_all();
        let (root, tenant) = (ancestry.root, ancestry.tenant);
        match declaration.scope_class.as_str() {
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-1
            scope_class::GLOBAL => {
                // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-2
                let row = self
                    .values
                    .find_one(conn, &scope, declaration.id, root)
                    .await?;
                // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-2
                // A tenant asking for a global setting is served the platform
                // value read-only: the platform row is never its own override.
                // Gating that on the tenant's effective access arrives with
                // tenant access restrictions; today every setting is visible.
                let rows = index(row.into_iter().collect());
                Ok(select(declaration, tenant, root, &[root], &rows))
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-1
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-5
            scope_class::CASCADING => {
                let chain = ancestry.chain(self.hierarchy.as_ref()).await?;
                // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-7
                // One exact-match set query over the ancestor ids. The chain
                // begins at the root, so platform scope is its first element
                // and needs no disjunct; the scope column is compared as an id
                // and never scanned as a path.
                let rows = self
                    .values
                    .find_in_tenants(conn, &scope, declaration.id, chain)
                    .await?;
                // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-7
                Ok(select(declaration, tenant, root, chain, &index(rows)))
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-5
            // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-12
            scope_class::LOCAL => {
                // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-13
                // The requested tenant's row and nothing else: a local setting
                // is never inherited, so no ancestor is asked for.
                let row = self
                    .values
                    .find_one(conn, &scope, declaration.id, tenant)
                    .await?;
                // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-13
                let rows = index(row.into_iter().collect());
                Ok(select(declaration, tenant, root, &[tenant], &rows))
            }
            // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-12
            other => Err(DomainError::Internal {
                diagnostic: format!(
                    "declaration `{}` carries the unknown scope class `{other}`",
                    declaration.key
                ),
            }),
        }
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-15
        // Each arm returns the resolved value with its source and source scope.
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-dispatch:p1:inst-vr-disp-15
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod resolver_tests;

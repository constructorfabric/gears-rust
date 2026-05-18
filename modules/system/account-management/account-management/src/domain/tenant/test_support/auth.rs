//! Mock PDP plumbing for service-level `#[tokio::test]` blocks. Two
//! shapes are exposed:
//!
//! * [`mock_enforcer`] wires [`MockAuthZResolver`], a permissive PDP
//!   that emits a single
//!   [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
//!   constraint rooted at the caller's `subject_tenant_id`. Mirrors
//!   the production policy bundle's "permit + clamp to caller's
//!   subtree" shape. Account Management calls
//!   `access_scope_with(... require_constraints(true))`, so an
//!   empty-constraint PDP would surface as
//!   [`AccessRequest::require_constraints`]-fail; the
//!   subject-rooted subtree clamp keeps every existing test
//!   passing because tests act within their subject's own subtree.
//! * [`constraint_bearing_enforcer`] wires
//!   [`ConstraintBearingAuthZResolver`], which models a
//!   policy-bundle-style PDP that pins the subtree root explicitly
//!   to a caller-supplied tenant id. Used by the regression tests
//!   in `service_tests.rs` that pin the post-#1813 subtree-clamp
//!   contract: an authorized read / update / soft-delete on a
//!   tenant **inside** the root's subtree MUST succeed; an
//!   authorized action on a tenant **outside** the root's subtree
//!   MUST collapse to `NotFound` at the database via the secure-
//!   extension layer.
//! * [`deny_all_enforcer`] wires [`DenyAllAuthZResolver`], which
//!   models a PDP that refuses every evaluation with
//!   `decision: false`. Used to pin the `EnforcerError::Denied →
//!   DomainError::CrossTenantDenied` mapping at every caller-facing
//!   service seam: a regression that drops the `self.authorize(...)`
//!   call from a public method (or wires it past the deny path)
//!   surfaces here as a lifted error instead of `CrossTenantDenied`.
//! * [`schema_selective_enforcer`] wires [`SchemaSelectiveAuthZResolver`],
//!   which permits every `Metadata.list` / non-metadata action (so
//!   the outer list gate passes) and emits `decision: true` for
//!   `Metadata.{read,write,delete,resolve}` only when the request's
//!   `resource.properties["schema_id"]` is on the resolver's
//!   configured allow-list. Used by the tenant-metadata service
//!   tests to pin the per-row schema-deny silent-drop in
//!   `list_metadata` (PRD §1848: "list responses omit entries the
//!   actor is not permitted to read").
//! * [`schema_unavailable_enforcer`] wires [`SchemaUnavailableAuthZResolver`],
//!   which returns `Err(AuthZResolverError::ServiceUnavailable(_))`
//!   for any `Metadata.read` evaluation. Pins the negative path on
//!   `caller_allows_schema_read`: non-`CrossTenantDenied` errors must
//!   propagate, not be silently dropped.

#![allow(dead_code, clippy::must_use_candidate, clippy::missing_panics_doc)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
    models::{Capability, EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use modkit_macros::domain_model;

/// Build a permit-with-subtree-clamp [`EvaluationResponse`] rooted at
/// `root_tenant_id`. Centralises the production-shape predicate
/// emission so both mocks below stay in sync.
fn permit_with_subtree(root_tenant_id: uuid::Uuid) -> EvaluationResponse {
    use authz_resolver_sdk::constraints::{Constraint, InTenantSubtreePredicate, Predicate};
    use modkit_security::pep_properties;

    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                    pep_properties::RESOURCE_ID,
                    root_tenant_id,
                ))],
            }],
            deny_reason: None,
        },
    }
}

/// Permissive PDP fake for service / handler tests.
///
/// Reads the caller's `subject.properties["tenant_id"]` (populated by
/// the PEP per the `AuthZEN` spec) and emits a single
/// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
/// constraint rooted at that tenant. Every existing service-level test
/// uses `ctx_for(root)`, so the compiled scope clamps to the root's
/// subtree — which transparently covers every tenant the test mutates.
/// Cross-tenant denial in production is owned by the real PDP behind a
/// `PolicyEnforcer` fed by the Tenant Resolver Plugin; the negative
/// path is regression-pinned by
/// [`ConstraintBearingAuthZResolver`] below.
#[domain_model]
pub struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverClient for MockAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        // Pluck the subject's tenant id out of the AuthZEN-spec
        // `subject.properties["tenant_id"]` slot the PEP builder
        // wrote. A missing / malformed slot is a test-wiring bug
        // (the production PEP at `authz-resolver-sdk::pep::enforcer`
        // always writes a stringified `Uuid`) — panic loudly so the
        // bug surfaces as a clear failure instead of as a confusing
        // empty-subtree `NotFound`.
        let root_str = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .expect(
                "MockAuthZResolver: subject.properties[\"tenant_id\"] is missing or not a string; \
                 build SecurityContext via SecurityContext::builder().subject_tenant_id(_) so \
                 the PEP enforcer populates the AuthZEN-spec slot",
            );
        let root = uuid::Uuid::parse_str(root_str).expect(
            "MockAuthZResolver: subject.properties[\"tenant_id\"] is not a valid UUID; \
             SecurityContext::builder takes a Uuid so this should be unreachable",
        );
        Ok(permit_with_subtree(root))
    }
}

/// Build a permissive [`PolicyEnforcer`] for tests. Pairs with
/// [`make_service`] and the inline `make_service` helpers used by the
/// service-level `#[tokio::test]` blocks.
#[must_use]
pub fn mock_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolver);
    // Mirror the production wiring in `module.rs`: AM advertises
    // `TenantHierarchy` so the PDP returns the native
    // `InTenantSubtree` predicate. Without the capability the
    // production PDP would downgrade to a pre-resolved `In`, and
    // tests using this enforcer would diverge from the production
    // request shape.
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

/// PDP fake that pins the subtree root explicitly to a caller-supplied
/// tenant id, regardless of the request's subject tenant. Used by the
/// regression tests that exercise the cross-subtree denial contract:
/// caller scoped to root, target tenant outside root's subtree → the
/// compiled subtree-clamp at the database collapses the row to
/// `NotFound` even though the PDP-side `decision: true` lets the
/// service-layer gate through.
#[domain_model]
pub struct ConstraintBearingAuthZResolver {
    /// Root tenant id the synthetic
    /// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
    /// predicate is rooted at. The compiled `AccessScope` clamps reads
    /// on `tenants.id` to that root's closure subtree via
    /// `tenant_closure`.
    pub root_tenant_id: uuid::Uuid,
}

#[async_trait]
impl AuthZResolverClient for ConstraintBearingAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let _ = request;
        Ok(permit_with_subtree(self.root_tenant_id))
    }
}

/// Build a [`PolicyEnforcer`] backed by [`ConstraintBearingAuthZResolver`].
/// The compiled scope clamps reads on the `tenants` entity (and any
/// other entity declaring an `OWNER_TENANT_ID` / `RESOURCE_ID` column
/// against the `InTenantSubtree` predicate) to the closure subtree
/// rooted at `root_tenant_id`.
#[must_use]
pub fn constraint_bearing_enforcer(root_tenant_id: uuid::Uuid) -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> =
        Arc::new(ConstraintBearingAuthZResolver { root_tenant_id });
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

/// PDP fake that refuses every evaluation with `decision: false` and
/// no constraints. Drives the [`crate::domain::error::DomainError::CrossTenantDenied`]
/// regression tests: every caller-facing service method that runs
/// through `self.authorize(...)` MUST propagate
/// `EnforcerError::Denied → DomainError::CrossTenantDenied`. A
/// regression that strips the `authorize` call from a public method
/// (or wires it past the deny path) surfaces here as a lifted /
/// non-`CrossTenantDenied` error.
#[domain_model]
pub struct DenyAllAuthZResolver;

#[async_trait]
impl AuthZResolverClient for DenyAllAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let _ = request;
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// Build a [`PolicyEnforcer`] backed by [`DenyAllAuthZResolver`]. Use
/// from caller-facing-method tests that pin the PEP-deny path.
#[must_use]
pub fn deny_all_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(DenyAllAuthZResolver);
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

/// PDP fake that allows every non-`Metadata.read` action and only
/// allows `Metadata.read` when the request's
/// `resource.properties["schema_id"]` matches the configured
/// allow-list. Drives the per-row schema-deny silent-drop pin in
/// [`crate::domain::metadata::service::MetadataService::list_metadata`]:
/// rows whose `schema_id` is **not** on the allow-list MUST be
/// omitted from the listing page rather than surfaced as an error.
///
/// `Metadata.list` is always permitted (outer gate) so the listing
/// flow reaches the per-row recheck. Non-metadata actions are also
/// permitted so unrelated tenant-side reads in the same flow are not
/// collateral-denied.
#[domain_model]
pub struct SchemaSelectiveAuthZResolver {
    /// Set of chained `gts.…~vendor.…~` schema ids the resolver
    /// permits `Metadata.read` on. Membership is checked against the
    /// `pep::SCHEMA_ID` property
    /// (`account_management::domain::metadata::service::pep::SCHEMA_ID`)
    /// supplied by the impl-side `MetadataService::authorize` call.
    pub allowed_schema_ids: Vec<String>,
}

#[async_trait]
impl AuthZResolverClient for SchemaSelectiveAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let root_str = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .expect("SchemaSelectiveAuthZResolver: subject.properties[tenant_id] missing");
        let root = uuid::Uuid::parse_str(root_str).expect("SchemaSelectiveAuthZResolver: bad UUID");

        // Per-row recheck happens on `Metadata.read`. For every other
        // action (LIST outer gate, WRITE, DELETE, RESOLVE, plus any
        // tenant-side action) the resolver behaves like the
        // permissive baseline.
        if request.action.name != "read" {
            return Ok(permit_with_subtree(root));
        }

        // `read` action: gate on the supplied `schema_id` property.
        // Absence of `schema_id` on a `read` evaluation means the
        // caller is the outer get_metadata path — we still allow it,
        // matching the production posture where the outer get_metadata
        // authorize call also sets SCHEMA_ID and a missing slot is a
        // test-wiring bug rather than a denial signal.
        let schema_id = request
            .resource
            .properties
            .get("schema_id")
            .and_then(serde_json::Value::as_str);

        match schema_id {
            Some(sid) if self.allowed_schema_ids.iter().any(|a| a == sid) => {
                Ok(permit_with_subtree(root))
            }
            Some(_) => Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            }),
            None => Ok(permit_with_subtree(root)),
        }
    }
}

/// Build a [`PolicyEnforcer`] backed by [`SchemaSelectiveAuthZResolver`].
/// Caller supplies the chained `gts.…~vendor.…~` schema ids the
/// resolver should permit on `Metadata.read`; everything else is
/// denied. Drives the schema-deny silent-drop pin in `list_metadata`.
#[must_use]
pub fn schema_selective_enforcer(allowed_schema_ids: Vec<String>) -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> =
        Arc::new(SchemaSelectiveAuthZResolver { allowed_schema_ids });
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

/// PDP fake that always surfaces a transport failure on
/// `Metadata.read` evaluations and permits every other action.
/// Drives the negative-path pin on `caller_allows_schema_read`: an
/// `AuthZResolverError::ServiceUnavailable` (or any non-denied
/// failure surfacing as `EnforcerError::Other` →
/// `DomainError::ServiceUnavailable`) MUST propagate out of
/// `list_metadata` rather than being silent-dropped together with the
/// `CrossTenantDenied` rows.
#[domain_model]
pub struct SchemaUnavailableAuthZResolver;

#[async_trait]
impl AuthZResolverClient for SchemaUnavailableAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        // Outer gate (LIST + non-read actions) still permits so the
        // flow reaches the per-row recheck where the transport error
        // fires.
        if request.action.name != "read" {
            let root_str = request
                .subject
                .properties
                .get("tenant_id")
                .and_then(serde_json::Value::as_str)
                .expect("SchemaUnavailableAuthZResolver: subject.properties[tenant_id] missing");
            let root =
                uuid::Uuid::parse_str(root_str).expect("SchemaUnavailableAuthZResolver: bad UUID");
            return Ok(permit_with_subtree(root));
        }
        Err(AuthZResolverError::ServiceUnavailable(
            "schema-unavailable test fake: simulated PDP transport failure on Metadata.read"
                .to_owned(),
        ))
    }
}

/// Build a [`PolicyEnforcer`] backed by [`SchemaUnavailableAuthZResolver`].
#[must_use]
pub fn schema_unavailable_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(SchemaUnavailableAuthZResolver);
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

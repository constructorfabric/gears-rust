//! Tests for the ledger authz descriptors and label stub schemas.

#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, TenantMode,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::{
    AuthzError, access_scope, actions, authz_label_type_schemas, labels, own_tenant_scope,
    resource_types,
};

#[test]
fn resource_types_carry_their_labels() {
    assert_eq!(resource_types::ENTRY.name(), labels::ENTRY);
    assert_eq!(resource_types::LEDGER.name(), labels::LEDGER);
    assert_eq!(resource_types::FISCAL_PERIOD.name(), labels::FISCAL_PERIOD);
    assert_eq!(resource_types::PAYMENT.name(), labels::PAYMENT);
}

#[test]
fn entry_actions_are_stable() {
    // Anti-drift: the `entry` resource's action consts are wire-stable strings
    // the PEP gate + the permission catalog (`crate::gts::permissions`) share.
    // `reverse` must exist as its own action — the reversal /
    // mapping-correction handlers gate on `(entry, reverse)`, NOT `(entry,
    // post)`, so original-posting authority and reversal authority are
    // separately grantable. Drop `REVERSE` and this fails to compile / match.
    assert_eq!(actions::POST, "post");
    assert_eq!(actions::REVERSE, "reverse");
    assert_eq!(actions::READ, "read");
}

#[test]
fn labels_are_concrete_gts_types() {
    // Stronger than a suffix match: every authz label must parse as a
    // structurally valid GTS id AND be a concrete TYPE id (type ids end `~`).
    for label in labels::ALL {
        assert!(
            ::gts::GtsId::try_new(label).is_ok(),
            "label {label} is not a structurally valid GTS id"
        );
        assert!(
            label.ends_with('~'),
            "label {label} must be a concrete type id"
        );
    }
}

#[test]
fn label_schemas_cover_every_label() {
    let schemas = authz_label_type_schemas();
    assert_eq!(schemas.len(), labels::ALL.len());
    for schema in &schemas {
        let id = schema["$id"].as_str().expect("$id string");
        let label = id.strip_prefix("gts://").expect("$id is gts:// prefixed");
        assert!(
            labels::ALL.contains(&label),
            "schema $id {id} does not map to a known label"
        );
        assert_eq!(schema["type"], "object");
    }
}

/// Degraded flat-`In` PDP fake: permits and emits a single flat
/// `In([allowed])` constraint over `OWNER_TENANT_ID` — the shape the
/// production PDP returns for a PEP that advertises no tenant-subtree
/// capability (this gear, [`PolicyEnforcer::new`] with no `with_capabilities`).
/// The request is ignored: the fake models a subject authorized only for the
/// single `allowed` tenant.
struct FlatInResolver {
    allowed: Uuid,
}

#[async_trait]
impl AuthZResolverApi for FlatInResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![self.allowed],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// A degraded-mode enforcer (no `with_capabilities`) over a subject authorized
/// for `allowed` only — mirrors the gear's production PEP wiring.
fn flat_in_enforcer(allowed: Uuid) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FlatInResolver { allowed }))
}

fn ctx_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

/// A write gate (`require_constraints = true` + a target `owner_tenant_id`)
/// must DENY when the target tenant is outside the PDP's compiled scope, and
/// ALLOW when it is inside. This pins the cross-tenant-write hole: the degraded
/// flat-`In` decision does not re-validate `owner_tenant_id` at the PDP, so the
/// gate itself must assert target membership.
#[tokio::test]
async fn write_gate_denies_target_outside_authorized_scope() {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant_a); // authorized for tenant_a only
    let ctx = ctx_for(tenant_a);

    // Cross-tenant write: target B is outside the authorized In([A]) -> Denied.
    let denied = access_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::POST,
        Some(tenant_b),
        None,
        true,
    )
    .await;
    assert!(
        matches!(denied, Err(AuthzError::Denied(_))),
        "posting into tenant B with scope In([A]) must be denied, got {denied:?}"
    );

    // In-scope write: target A is inside the authorized scope -> allowed, and
    // the returned scope carries the In([A]) filter for SQL-level binding.
    let allowed = access_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::POST,
        Some(tenant_a),
        None,
        true,
    )
    .await
    .expect("posting into own tenant A must be allowed");
    assert!(
        allowed.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_a),
        "the granted scope must carry the tenant-A filter"
    );
}

/// The `reverse` action gates exactly like `post` (a write on `entry`): a
/// cross-tenant target is denied and an in-scope target is allowed with the
/// `In([A])` filter. Pins that reversal authority routes through the same
/// degraded flat-`In` write gate as original posting.
#[tokio::test]
async fn reverse_gate_matches_post_write_semantics() {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant_a); // authorized for tenant_a only
    let ctx = ctx_for(tenant_a);

    let denied = access_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::REVERSE,
        Some(tenant_b),
        None,
        true,
    )
    .await;
    assert!(
        matches!(denied, Err(AuthzError::Denied(_))),
        "reversing into tenant B with scope In([A]) must be denied, got {denied:?}"
    );

    let allowed = access_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::REVERSE,
        Some(tenant_a),
        None,
        true,
    )
    .await
    .expect("reversing within own tenant A must be allowed");
    assert!(
        allowed.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_a),
        "the granted scope must carry the tenant-A filter"
    );
}

/// PDP fake that always fails to evaluate (models an unreachable PDP).
struct FailingResolver;

#[async_trait]
impl AuthZResolverApi for FailingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("pdp unreachable".to_owned()).create())
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// PDP fake that explicitly denies (`decision = false`).
struct DenyingResolver;

#[async_trait]
impl AuthZResolverApi for DenyingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: vec![],
                deny_reason: None,
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// An unreachable PDP must fail closed as `Unavailable` (→ 503), NOT `Denied`
/// (→ 403): the two carry different operator semantics and retry behaviour.
#[tokio::test]
async fn pdp_evaluation_failure_maps_to_unavailable() {
    let enforcer = PolicyEnforcer::new(Arc::new(FailingResolver));
    let ctx = ctx_for(Uuid::now_v7());
    let res = access_scope(
        &enforcer,
        &ctx,
        &resource_types::LEDGER,
        actions::PROVISION,
        Some(Uuid::now_v7()),
        None,
        true,
    )
    .await;
    assert!(
        matches!(res, Err(AuthzError::Unavailable(_))),
        "an unreachable PDP must fail closed as Unavailable, got {res:?}"
    );
}

/// An explicit PDP deny maps to `Denied` (→ 403).
#[tokio::test]
async fn pdp_decision_false_maps_to_denied() {
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingResolver));
    let ctx = ctx_for(Uuid::now_v7());
    let res = access_scope(
        &enforcer,
        &ctx,
        &resource_types::LEDGER,
        actions::PROVISION,
        Some(Uuid::now_v7()),
        None,
        true,
    )
    .await;
    assert!(
        matches!(res, Err(AuthzError::Denied(_))),
        "an explicit PDP deny must map to Denied, got {res:?}"
    );
}

/// A read (`owner_tenant_id = None`) skips the write-membership assertion and
/// returns the PDP's compiled `In([tenant])` scope verbatim for SQL binding.
#[tokio::test]
async fn read_path_returns_pdp_scope_without_membership_check() {
    let tenant = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant);
    let ctx = ctx_for(tenant);
    let scope = access_scope(
        &enforcer,
        &ctx,
        &resource_types::LEDGER,
        actions::READ,
        None,
        None,
        true,
    )
    .await
    .expect("read must be allowed");
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant),
        "the read scope must carry the tenant filter"
    );
}

/// The `(payment, write)` gate enforces the same cross-tenant write semantics as
/// `(entry, post)`: a target tenant outside the caller's compiled scope is
/// denied, an in-scope target is allowed with the `In([A])` filter for SQL
/// binding. Pins that settle / allocate cannot write into a foreign tenant's
/// ledger.
#[tokio::test]
async fn payment_write_gate_denies_target_outside_authorized_scope() {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant_a); // authorized for tenant_a only
    let ctx = ctx_for(tenant_a);

    let denied = access_scope(
        &enforcer,
        &ctx,
        &resource_types::PAYMENT,
        actions::WRITE,
        Some(tenant_b),
        None,
        true,
    )
    .await;
    assert!(
        matches!(denied, Err(AuthzError::Denied(_))),
        "settling/allocating into tenant B with scope In([A]) must be denied, got {denied:?}"
    );

    let allowed = access_scope(
        &enforcer,
        &ctx,
        &resource_types::PAYMENT,
        actions::WRITE,
        Some(tenant_a),
        None,
        true,
    )
    .await
    .expect("payment write within own tenant A must be allowed");
    assert!(
        allowed.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_a),
        "the granted scope must carry the tenant-A filter"
    );
}

/// A `(payment, read)` gate (`owner_tenant_id = None`) returns the PDP's compiled
/// `In([tenant])` scope verbatim — the SQL-level BOLA filter the allocation /
/// unallocated reads bind, so a foreign payment/payer resolves to empty.
#[tokio::test]
async fn payment_read_returns_pdp_scope_for_sql_filter() {
    let tenant = Uuid::now_v7();
    let enforcer = flat_in_enforcer(tenant);
    let ctx = ctx_for(tenant);
    let scope = access_scope(
        &enforcer,
        &ctx,
        &resource_types::PAYMENT,
        actions::READ,
        None,
        None,
        true,
    )
    .await
    .expect("payment read must be allowed");
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant),
        "the payment read scope must carry the tenant filter"
    );
}

/// A tenant-hierarchy PDP fake shaped like tr-authz for a request with no
/// `resource_id`: `Subtree` (the default) answers `In([subject, child])` — the
/// subject's tenant and its one descendant — and `RootOnly` answers
/// `In([subject])`. It records the tenant mode of every request it is asked.
struct HierarchyResolver {
    child: Uuid,
    modes: Mutex<Vec<Option<TenantMode>>>,
}

#[async_trait]
impl AuthZResolverApi for HierarchyResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let mode = req.context.tenant_context.map(|tc| tc.mode);
        self.modes.lock().expect("modes lock").push(mode.clone());
        let subject = req.subject.properties["tenant_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("subject tenant_id property");
        let tenants = if mode == Some(TenantMode::RootOnly) {
            vec![subject]
        } else {
            vec![subject, self.child]
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        tenants,
                    ))],
                }],
                deny_reason: None,
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

fn hierarchy(child: Uuid) -> (Arc<HierarchyResolver>, PolicyEnforcer) {
    let resolver = Arc::new(HierarchyResolver {
        child,
        modes: Mutex::new(Vec::new()),
    });
    let enforcer = PolicyEnforcer::new(resolver.clone());
    (resolver, enforcer)
}

/// The governed-write gate asks the PDP for the caller's tenant only; the read and
/// audit-elevation gate keeps the PDP's default tenant mode (`Subtree`).
#[tokio::test]
async fn own_tenant_scope_asks_root_only_and_access_scope_keeps_the_default() {
    let tenant = Uuid::now_v7();
    let (resolver, enforcer) = hierarchy(Uuid::now_v7());
    let ctx = ctx_for(tenant);

    own_tenant_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::POST,
        Some(tenant),
        None,
    )
    .await
    .expect("a write into the caller's own book is allowed");
    own_tenant_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::APPROVE,
        Some(tenant),
        None,
    )
    .await
    .expect("an approval verb in the caller's own tenant is allowed");
    access_scope(
        &enforcer,
        &ctx,
        &resource_types::ENTRY,
        actions::READ,
        None,
        None,
        true,
    )
    .await
    .expect("the read is allowed");

    assert_eq!(
        *resolver.modes.lock().expect("modes lock"),
        vec![Some(TenantMode::RootOnly), Some(TenantMode::RootOnly), None],
        "governed writes and approval verbs ask RootOnly; reads keep the PDP default"
    );
}

/// A parent user posting into a child's book is denied at the PEP gate: the
/// `RootOnly` scope holds only the parent's tenant, so the child target fails the
/// membership assertion before any dual-control threshold is read.
#[tokio::test]
async fn own_tenant_scope_denies_a_parent_write_into_a_child_book() {
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    let (_, enforcer) = hierarchy(child);

    let err = own_tenant_scope(
        &enforcer,
        &ctx_for(parent),
        &resource_types::ENTRY,
        actions::POST,
        Some(child),
        None,
    )
    .await
    .expect_err("a parent write into a child's book must be denied");
    assert!(matches!(err, AuthzError::Denied(_)), "got {err:?}");
}

/// The audit elevation check (`audit::cross_tenant_role_authorized`) asks the
/// write-shaped question with the target as `owner_tenant_id`; it stays on the
/// subtree scope, so an elevation into an authorized child still passes.
#[tokio::test]
async fn access_scope_still_admits_an_authorized_child_for_audit_elevation() {
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    let (_, enforcer) = hierarchy(child);

    access_scope(
        &enforcer,
        &ctx_for(parent),
        &resource_types::ENTRY,
        actions::AUDIT_READ,
        Some(child),
        None,
        true,
    )
    .await
    .expect("the audit elevation into an authorized child is allowed");
}

//! The PEP gate asks the PDP for the caller's tenant only (D-523).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, TenantMode,
};
use authz_resolver_sdk::{AuthZResolverApi, Constraint, InPredicate, PolicyEnforcer, Predicate};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use super::{OwnerTenant, ResourceRef, access_scope, actions, labels, resource_types};

/// Allows under `In([tenant])` and records the tenant mode of every request it is asked.
struct RecordingResolver {
    tenant: Uuid,
    modes: Mutex<Vec<Option<TenantMode>>>,
}

#[async_trait]
impl AuthZResolverApi for RecordingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.modes
            .lock()
            .unwrap()
            .push(request.context.tenant_context.map(|tc| tc.mode));
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![self.tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
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

/// A collection read, a single-row read and a write each ask for `RootOnly`, so the PDP never
/// expands the subject's subtree into an `IN` list of its descendant tenants.
#[tokio::test]
async fn every_request_asks_for_the_callers_tenant_only() {
    let tenant = Uuid::now_v7();
    let resolver = Arc::new(RecordingResolver {
        tenant,
        modes: Mutex::new(Vec::new()),
    });
    let enforcer = PolicyEnforcer::new(resolver.clone());
    let ctx = ctx_for(tenant);

    access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        None,
    )
    .await
    .expect("the collection read is allowed");
    access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        Some(ResourceRef(Uuid::now_v7())),
    )
    .await
    .expect("the single-row read is allowed");
    access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(tenant)),
        None,
    )
    .await
    .expect("the write into the caller's tenant is allowed");

    assert_eq!(
        *resolver.modes.lock().unwrap(),
        vec![Some(TenantMode::RootOnly); 3],
        "every PDP request names the caller's tenant only"
    );
}

/// D-526: the catalog is exactly the (label, action) pairs the doors enforce — nineteen of them
/// (3 + 2 + 3 + 3 + 3 + 2 + 3).
#[test]
fn nineteen_permissions_cover_exactly_the_enforced_pairs_and_inventory() {
    let all = crate::gts::permissions::all();
    assert_eq!(all.len(), 19);
    let actual = all
        .iter()
        .map(|p| (p.resource_type.as_str(), p.action.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let expected = [
        (labels::PRICE_BOOK, actions::READ),
        (labels::PRICE_BOOK, actions::AUTHOR),
        (labels::PRICE_BOOK, actions::SUBMIT),
        (labels::PRICE_BOOK_ENTRY, actions::READ),
        (labels::PRICE_BOOK_ENTRY, actions::AUTHOR),
        (labels::PRICE, actions::READ),
        (labels::PRICE, actions::AUTHOR),
        (labels::PRICE, actions::SUBMIT),
        (labels::PLAN, actions::READ),
        (labels::PLAN, actions::AUTHOR),
        (labels::PLAN, actions::SUBMIT),
        (labels::APPROVAL_UNIT, actions::READ),
        (labels::APPROVAL_UNIT, actions::APPROVE),
        (labels::APPROVAL_UNIT, actions::SUBMIT),
        (labels::CONFIG, actions::READ),
        (labels::CONFIG, actions::SETTINGS),
        (labels::ACCEPTANCE, actions::CREATE),
        (labels::ACCEPTANCE, actions::HOLD),
        (labels::ACCEPTANCE, actions::READ),
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        expected.len(),
        19,
        "the expectation itself names nineteen pairs"
    );
    assert_eq!(actual, expected);
    let prefix = gts_id!("cf.toolkit.authz.permission.v1~");
    let inventory = toolkit_gts::inventory::iter::<toolkit_gts::InventoryInstance>
        .into_iter()
        .filter(|e| {
            e.instance_id
                .starts_with(&format!("{prefix}cf.bss.pricing."))
        })
        .collect::<Vec<_>>();
    assert_eq!(inventory.len(), 19);
    for p in all {
        let id = p.id.to_string();
        let entry = inventory.iter().find(|e| e.instance_id == id).unwrap();
        assert_eq!(entry.type_id, prefix);
        assert_eq!((entry.payload_fn)(), serde_json::to_value(p).unwrap());
    }
}

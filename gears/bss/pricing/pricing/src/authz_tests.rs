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

use super::{OwnerTenant, ResourceRef, access_scope, actions, resource_types};

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

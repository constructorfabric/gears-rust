//! `GET /approval-policy/{kind}/effective` on Postgres (D-481).
//!
//! A resource constraint on the caller's grant must not be applied to `kind`.
//! `kind` is text; a `RESOURCE_ID` uuid compared with it is `text = uuid`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
mod plan_support;
use plan_support::{Fixture, entry_support, request};
use serde_json::json;
use std::sync::Arc;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

struct Narrow {
    tenant: Uuid,
    resource: Uuid,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Narrow {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::*;
        let constrained = request.action.name == "read"
            && matches!(
                request.resource.resource_type.as_str(),
                "gts.cf.bss.pricing.price_book_entry.v1~" | "gts.cf.bss.pricing.plan.v1~"
            );
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if constrained {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                vec![self.resource],
            )));
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}

async fn put_quorum(f: &Fixture, kind: &str, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind": kind, "quorum": quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}

/// D-481 on Postgres: quorum 3 and quorum 0 survive a resource-constrained grant.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn the_effective_quorum_is_the_tenants_under_a_resource_constraint_on_postgres() {
    let pg = pg_support::Pg::applied().await;
    let db = DBProvider::<DbError>::new(pg.db().await);
    let tenant = Uuid::new_v4();
    let f = Fixture::on(
        db,
        tenant,
        plan_support::entry_support::TestDsn::of(pg.url(true)),
        Arc::new(plan_support::Catalog::default()),
    )
    .await;
    put_quorum(&f, "prices", 3).await;
    put_quorum(&f, "plan_revision", 0).await;
    let app = entry_support::production(f.state.clone()).layer(axum::Extension(
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Narrow {
            tenant,
            resource: Uuid::new_v4(),
        })),
    ));
    let (s, prices, _) = request(
        &app,
        &f.ctx,
        "GET",
        "/approval-policy/prices/effective",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{prices}");
    assert_eq!(prices, json!({"kind": "prices", "quorum_required": 3}));
    let (s, plans, _) = request(
        &app,
        &f.ctx,
        "GET",
        "/approval-policy/plan_revision/effective",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{plans}");
    assert_eq!(
        plans,
        json!({"kind": "plan_revision", "quorum_required": 0})
    );
}

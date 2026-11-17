//! The boundary that the local client draws: results pass through, domain
//! errors become canonical errors.

use std::sync::Arc;

use license_enforcement_sdk::dto::{ImportEvidenceRequest, IssueGrantRequest, VerbRequest};
use license_enforcement_sdk::{GrantId, LicenseEnforcementApi};
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::LocalClient;
use crate::test_support::{DenyAuthZ, Env, ctx, env, env_with, terms, token};

fn local(env: &Env) -> LocalClient<crate::infra::storage::Store> {
    LocalClient::new(Arc::clone(&env.grants), Arc::clone(&env.check))
}

#[tokio::test]
async fn a_result_passes_through_the_boundary() {
    let env = env().await;
    let client = local(&env);
    let a = Uuid::now_v7();
    let issued = client
        .issue_grant(
            ctx(a),
            IssueGrantRequest {
                holder_tenant_id: a,
                grant: terms(),
                idempotency_token: token(),
            },
        )
        .await
        .unwrap();
    let g = &issued.grants[0];
    assert_eq!(&client.get_grant(ctx(a), g.id).await.unwrap(), g);
}

#[tokio::test]
async fn a_domain_error_becomes_its_canonical_error() {
    let env = env().await;
    let client = local(&env);
    let a = Uuid::now_v7();
    let err = client
        .get_grant(ctx(a), GrantId(Uuid::now_v7()))
        .await
        .unwrap_err();
    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");

    let g = crate::test_support::issue_one(&env, a, terms()).await;
    let mut stale = VerbRequest::new(token());
    stale.expected_version = Some(g.version + 1);
    let err = client.suspend_grant(ctx(a), g.id, stale).await.unwrap_err();
    assert!(matches!(err, CanonicalError::Aborted { .. }), "{err:?}");

    let err = client
        .import_evidence(
            ctx(a),
            ImportEvidenceRequest {
                bytes: String::new(),
                idempotency_token: token(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::Unimplemented { .. }),
        "{err:?}"
    );

    let denied = env_with(Arc::new(DenyAuthZ)).await;
    let err = local(&denied).get_grant(ctx(a), g.id).await.unwrap_err();
    assert!(
        matches!(err, CanonicalError::PermissionDenied { .. }),
        "{err:?}"
    );
}

use std::sync::Arc;

use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;

const SELLER: &str = "gts.cf.core.am.tenant_type.v1~vz.ams.tenants.partner.v1~";
const BUYER: &str = "gts.cf.core.am.tenant_type.v1~vz.ams.tenants.organization.v1~";

/// Canned tenant-type reader (stands in for the AM `get_tenant` adapter).
struct FakeReader(Option<String>);

#[async_trait]
impl TenantTypeReader for FakeReader {
    async fn tenant_type(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<Option<String>, CanonicalError> {
        Ok(self.0.clone())
    }
}

/// A guard whose configured seller set is exactly `{SELLER}`.
fn guard(tenant_type: Option<&str>) -> SellerGuard {
    SellerGuard::new(
        Arc::new(FakeReader(tenant_type.map(str::to_owned))),
        [SELLER.to_owned()],
    )
}

#[tokio::test]
async fn seller_type_passes() {
    let ctx = SecurityContext::anonymous();
    assert!(
        guard(Some(SELLER))
            .assert_owns_ledger(&ctx, Uuid::now_v7())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn non_seller_type_is_rejected() {
    let ctx = SecurityContext::anonymous();
    let err = guard(Some(BUYER))
        .assert_owns_ledger(&ctx, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::FailedPrecondition { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn unresolved_type_is_rejected() {
    let ctx = SecurityContext::anonymous();
    let err = guard(None)
        .assert_owns_ledger(&ctx, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::FailedPrecondition { .. }),
        "got {err:?}"
    );
}

/// A reader that surfaces a transient AM error (models `get_tenant` failing).
struct FailingReader;

#[async_trait]
impl TenantTypeReader for FailingReader {
    async fn tenant_type(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<Option<String>, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
}

/// A non-precondition AM error (transient / `NotFound`) must propagate
/// UNCHANGED, not be collapsed into the `FailedPrecondition` seller-reject —
/// otherwise a registry blip would masquerade as "not a ledger owner".
#[tokio::test]
async fn am_error_propagates_unchanged() {
    let ctx = SecurityContext::anonymous();
    let g = SellerGuard::new(Arc::new(FailingReader), [SELLER.to_owned()]);
    let err = g
        .assert_owns_ledger(&ctx, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "transient AM error must propagate, not collapse to FailedPrecondition; got {err:?}"
    );
}

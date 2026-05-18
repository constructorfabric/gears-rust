//! Tests for [`super::RgResourceOwnershipChecker`].
//!
//! Extracted into a companion file per dylint `DE1101` (inline test
//! blocks > 100 lines must move out of the production source file).
//! The fakes here are local to the checker's unit tests; the
//! cross-module `SlowRgClient` used by service-level integration
//! tests lives in `super::test_helpers`.

use super::*;
use modkit_odata::Page;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, GroupHierarchy, ResourceGroup, ResourceGroupError,
    ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest,
    UpdateTypeRequest,
};
use std::sync::Mutex;

#[allow(
    clippy::enum_variant_names,
    reason = "test fake mirrors RG `list_*` operations under test; the `List` prefix names the operation, not the variant kind"
)]
#[derive(Clone)]
enum FakeBehaviour {
    ListEmpty,
    ListNonEmpty,
    ListErr,
    ListDelay(Duration),
}

struct FakeRgClient {
    behaviour: Mutex<FakeBehaviour>,
    list_calls: Mutex<u32>,
    last_filter: Mutex<Option<Expr>>,
}

impl FakeRgClient {
    fn empty() -> Self {
        Self {
            behaviour: Mutex::new(FakeBehaviour::ListEmpty),
            list_calls: Mutex::new(0),
            last_filter: Mutex::new(None),
        }
    }

    fn non_empty() -> Self {
        Self {
            behaviour: Mutex::new(FakeBehaviour::ListNonEmpty),
            list_calls: Mutex::new(0),
            last_filter: Mutex::new(None),
        }
    }

    fn unavailable() -> Self {
        Self {
            behaviour: Mutex::new(FakeBehaviour::ListErr),
            list_calls: Mutex::new(0),
            last_filter: Mutex::new(None),
        }
    }

    fn slow(delay: Duration) -> Self {
        Self {
            behaviour: Mutex::new(FakeBehaviour::ListDelay(delay)),
            list_calls: Mutex::new(0),
            last_filter: Mutex::new(None),
        }
    }
}

fn sample_group(tenant_id: Uuid) -> ResourceGroup {
    ResourceGroup {
        id: Uuid::from_u128(0xDEAD),
        code: "gts.cf.core.rg.type.v1~acme.rg.test.example.v1~".into(),
        name: "sample".into(),
        hierarchy: GroupHierarchy {
            parent_id: None,
            tenant_id,
        },
        metadata: None,
    }
}

#[async_trait]
impl ResourceGroupClient for FakeRgClient {
    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        _request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!("not used by RgResourceOwnershipChecker")
    }
    async fn get_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!()
    }
    async fn list_types(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, ResourceGroupError> {
        unreachable!()
    }
    async fn update_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
        _request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!()
    }
    async fn delete_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
    ) -> Result<(), ResourceGroupError> {
        unreachable!()
    }
    async fn create_group(
        &self,
        _ctx: &SecurityContext,
        _request: CreateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError> {
        *self.list_calls.lock().expect("lock") += 1;
        *self.last_filter.lock().expect("lock") = query.filter().cloned();
        let behaviour = self.behaviour.lock().expect("lock").clone();
        match behaviour {
            FakeBehaviour::ListEmpty => Ok(Page::empty(1)),
            FakeBehaviour::ListNonEmpty => Ok(Page::new(
                vec![sample_group(Uuid::from_u128(0xAB))],
                modkit_odata::page::PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: 1,
                },
            )),
            FakeBehaviour::ListErr => {
                Err(ResourceGroupError::service_unavailable("rg backend down"))
            }
            FakeBehaviour::ListDelay(delay) => {
                tokio::time::sleep(delay).await;
                Ok(Page::empty(1))
            }
        }
    }
    async fn update_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn delete_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), ResourceGroupError> {
        unreachable!()
    }
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        unreachable!()
    }
    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        unreachable!()
    }
    async fn add_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<ResourceGroupMembership, ResourceGroupError> {
        unreachable!()
    }
    async fn remove_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<(), ResourceGroupError> {
        unreachable!()
    }
    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError> {
        unreachable!("not used by RgResourceOwnershipChecker")
    }
}

#[tokio::test]
async fn rg_checker_returns_zero_on_empty_page() {
    let client = Arc::new(FakeRgClient::empty());
    let checker = RgResourceOwnershipChecker::new(client.clone());
    let count = checker
        .count_ownership_links(&SecurityContext::anonymous(), Uuid::from_u128(0xAB))
        .await
        .expect("rg up => count");
    assert_eq!(count, 0);
    assert_eq!(*client.list_calls.lock().expect("lock"), 1);
    assert_filter_targets_tenant(&client, Uuid::from_u128(0xAB));
}

#[tokio::test]
async fn rg_checker_returns_one_on_non_empty_page() {
    let client = Arc::new(FakeRgClient::non_empty());
    let checker = RgResourceOwnershipChecker::new(client.clone());
    let count = checker
        .count_ownership_links(&SecurityContext::anonymous(), Uuid::from_u128(0xAB))
        .await
        .expect("rg up => count");
    // Probe is `$top=1`; delete_tenant only checks `> 0`, so reporting
    // `1` is sufficient — no need to drag back the full count.
    assert_eq!(count, 1);
    // Same filter-shape assertion as the empty-page test — a future
    // change that drops the filter ONLY on the non-empty branch
    // would otherwise pass silently while over-blocking siblings.
    assert_filter_targets_tenant(&client, Uuid::from_u128(0xAB));
}

/// Filter must reference the `tenant_id` field exposed by RG and
/// the specific tenant uuid; mis-naming would silently match
/// nothing on the RG side and dropping the filter would over-block
/// siblings. Both empty + non-empty list-result paths assert the
/// same shape via this helper.
fn assert_filter_targets_tenant(client: &Arc<FakeRgClient>, expected_uuid: Uuid) {
    let recorded = client.last_filter.lock().expect("lock").clone();
    let recorded = recorded.expect("filter recorded");
    match recorded {
        Expr::Compare(lhs, CompareOperator::Eq, rhs) => {
            assert!(
                matches!(*lhs, Expr::Identifier(ref s) if s == TENANT_ID_FIELD),
                "filter LHS must be the tenant_id identifier",
            );
            assert!(
                matches!(*rhs, Expr::Value(Value::Uuid(u)) if u == expected_uuid),
                "filter RHS must be the queried tenant uuid",
            );
        }
        other => panic!("unexpected filter shape: {other:?}"),
    }
}

#[tokio::test]
async fn rg_checker_propagates_client_failure_as_service_unavailable() {
    let client = Arc::new(FakeRgClient::unavailable());
    let checker = RgResourceOwnershipChecker::new(client);
    let err = checker
        .count_ownership_links(&SecurityContext::anonymous(), Uuid::from_u128(0xCD))
        .await
        .expect_err("rg down => err");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);
}

#[tokio::test(start_paused = true)]
async fn rg_checker_times_out_client_probe() {
    let client = Arc::new(FakeRgClient::slow(Duration::from_millis(50)));
    let checker = RgResourceOwnershipChecker::with_timeout(client.clone(), 10);
    let err = checker
        .count_ownership_links(&SecurityContext::anonymous(), Uuid::from_u128(0xEF))
        .await
        .expect_err("slow rg => timeout");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert!(
        err.to_string().contains("resource-group: timeout exceeded"),
        "got: {err}"
    );
    assert_eq!(*client.list_calls.lock().expect("lock"), 1);
}

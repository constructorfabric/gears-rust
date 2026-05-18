use account_management_sdk::UpdateTenantRequest;

use super::*;

#[test]
fn smallint_round_trip_is_total_over_known_values() {
    for s in [
        TenantStatus::Provisioning,
        TenantStatus::Active,
        TenantStatus::Suspended,
        TenantStatus::Deleted,
    ] {
        let v = s.as_smallint();
        assert_eq!(TenantStatus::from_smallint(v), Some(s));
    }
}

#[test]
fn from_smallint_rejects_unknown_values() {
    assert_eq!(TenantStatus::from_smallint(-1), None);
    assert_eq!(TenantStatus::from_smallint(4), None);
    assert_eq!(TenantStatus::from_smallint(42), None);
}

#[test]
fn is_sdk_visible_excludes_provisioning_only() {
    assert!(!TenantStatus::Provisioning.is_sdk_visible());
    assert!(TenantStatus::Active.is_sdk_visible());
    assert!(TenantStatus::Suspended.is_sdk_visible());
    assert!(TenantStatus::Deleted.is_sdk_visible());
}

#[test]
fn sdk_status_lifts_into_internal() {
    use account_management_sdk::TenantStatus as SdkStatus;
    assert_eq!(TenantStatus::from(SdkStatus::Active), TenantStatus::Active);
    assert_eq!(
        TenantStatus::from(SdkStatus::Suspended),
        TenantStatus::Suspended
    );
    assert_eq!(
        TenantStatus::from(SdkStatus::Deleted),
        TenantStatus::Deleted
    );
}

#[test]
fn internal_status_lowers_into_sdk_for_visible_variants() {
    use account_management_sdk::TenantStatus as SdkStatus;
    assert_eq!(
        SdkStatus::try_from(TenantStatus::Active).expect("Active is sdk-visible"),
        SdkStatus::Active
    );
    assert_eq!(
        SdkStatus::try_from(TenantStatus::Suspended).expect("Suspended is sdk-visible"),
        SdkStatus::Suspended
    );
    assert_eq!(
        SdkStatus::try_from(TenantStatus::Deleted).expect("Deleted is sdk-visible"),
        SdkStatus::Deleted
    );
}

#[test]
fn internal_status_lowering_provisioning_returns_err() {
    // Service-level filter (`is_sdk_visible`) drops Provisioning rows
    // before they reach `lower_to_tenant` / `lower_to_tenant_page`. If a bug ever bypasses
    // that filter, the lowering returns `Err(ProvisioningNotPublic)`
    // and the caller maps it to `DomainError::Internal` (HTTP 500),
    // not a process panic. This test pins the post-fix `TryFrom`
    // contract; the previous `From` panicked via `unreachable!()`.
    let err = account_management_sdk::TenantStatus::try_from(TenantStatus::Provisioning)
        .expect_err("Provisioning has no public SDK representation");
    assert_eq!(err, ProvisioningNotPublic);
}

#[test]
fn empty_update_is_empty() {
    assert!(UpdateTenantRequest::default().is_empty());
    assert!(!UpdateTenantRequest::new().with_name("x").is_empty());
}

// `validate_status_transition` was removed together with the
// PATCH-side status field. `Active` ↔ `Suspended` transitions go
// through `TenantRepo::set_status` (exercised under SERIALIZABLE
// retry in `repo_impl::updates::set_status`); the rejection rules for
// `Deleted` / `Provisioning` are covered by service-level tests for
// `suspend_tenant` / `unsuspend_tenant` and the integration tests for
// the soft-delete flow.

// `validate_tenant_name` was deleted in favour of
// `domain::gts_validation::validate_tenant_name_via_gts` (the
// resource-group `validate_metadata_via_gts` pattern). The schema-
// driven path is exercised through service-level tests that wire a
// `MockTypesRegistryClient` with the `gts.cf.core.am.tenant.v1~`
// schema registered.

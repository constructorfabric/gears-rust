//! Tests for the SDK tenant input/output contract.
//!
//! `TenantInfoQuery` is a pure marker for the
//! [`#[derive(ODataFilterable)]`](modkit_odata_macros::ODataFilterable)
//! macro; the generated `TenantInfoFilterField` enum is exercised via
//! the impl-crate repo tests and `paginate_odata` integration. The
//! tests below cover only the hand-written input shapes.

use super::*;

#[test]
fn tenant_update_default_is_empty() {
    let u = UpdateTenantRequest::default();
    assert!(u.is_empty());
}

#[test]
fn tenant_update_with_name_is_not_empty() {
    let u = UpdateTenantRequest {
        name: Some("x".into()),
        ..Default::default()
    };
    assert!(!u.is_empty());
}

// The previous `tenant_update_with_status_is_not_empty` test pinned
// the now-removed `status` field on `UpdateTenantRequest`. Lifecycle
// transitions go through the dedicated
// `AccountManagementClient::{suspend_tenant, unsuspend_tenant,
// delete_tenant}` methods; the patch shape carries only `name`.

#[test]
fn tenant_info_filter_fields_are_pinned() {
    // Pinned: the `$filter` / `$orderby` allow-list is part of the
    // public SDK contract. A new column (e.g. `name`) is a SemVer
    // minor bump for AM SDK and SHOULD show up here; an accidental
    // change (e.g. renaming `tenant_type_uuid`) would otherwise drift
    // past CI silently.
    use modkit_odata::filter::FilterField;

    let names: Vec<&'static str> = TenantInfoFilterField::FIELDS
        .iter()
        .map(modkit_odata::filter::FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec![
            "id",
            "status",
            "tenant_type_uuid",
            "self_managed",
            "created_at",
            "updated_at",
        ],
        "TenantInfoQuery filter-field surface drifted; bump SDK minor + update doc-comment when intentional"
    );
}

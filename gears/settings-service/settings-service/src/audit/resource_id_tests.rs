// Created: 2026-08-13 by Constructor Tech
//! Tests for the canonical audit resource id.
//!
//! DESIGN.md §4.2 requires one formatter shared by the audit write and the
//! history read. These pin the format itself, because a change here silently
//! splits a setting's history rather than failing anything.

use uuid::Uuid;

use settings_service_sdk::SettingKey;

use super::format;

fn key() -> SettingKey {
    SettingKey::parse("gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~")
        .expect("fixture key parses")
}

#[test]
fn a_tenant_scope_is_keyed_by_the_flat_uuid() {
    let tenant = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("valid uuid");
    assert_eq!(
        format(&key(), tenant),
        "cf.settings:gts.cf.core.settings.setting_type.v1~acme.settings.network.enable_proxy.v1~\
         @550e8400-e29b-41d4-a716-446655440000"
    );
}

#[test]
fn the_platform_scope_is_the_root_tenant_not_a_sentinel() {
    // DESIGN.md §4.1: platform scope is the root tenant's id, never NULL and
    // never a marker word. The root tenant's records therefore read exactly like
    // any other tenant's, and nothing downstream needs a special case.
    let root = Uuid::new_v4();
    let id = format(&key(), root);
    assert!(id.ends_with(&format!("@{root}")));
    assert!(!id.contains("@platform"));
}

#[test]
fn a_tenant_id_is_never_a_tenant_path() {
    // Ancestry is derived state, resolved by the Tenant Resolver and never
    // stored. A path-based id would break every historical record on a reparent
    // or rename; the immutable UUID stays valid for the life of the trail.
    let tenant = Uuid::new_v4();
    let id = format(&key(), tenant);
    assert!(
        !id.contains('/'),
        "a path separator would imply ancestry: {id}"
    );
    assert!(id.ends_with(&tenant.to_string()));
}

#[test]
fn the_key_does_not_collide_with_the_delimiters() {
    // The format's parseability rests on setting keys containing `~` and `.`
    // but never `:` or `@`. If a key ever could, the id would become ambiguous
    // and history queries would silently mismatch.
    let rendered = format(&key(), Uuid::new_v4());
    let body = rendered
        .strip_prefix("cf.settings:")
        .expect("carries the service prefix");
    assert_eq!(
        body.matches('@').count(),
        1,
        "exactly one scope separator, so the split point is unambiguous: {rendered}"
    );
    assert!(
        !body
            .split('@')
            .next()
            .expect("has a key half")
            .contains(':'),
        "a colon in the key half would collide with the prefix delimiter"
    );
}

#[test]
fn one_setting_and_scope_map_to_exactly_one_id() {
    // What makes history an exact-match query rather than a prefix search.
    let tenant = Uuid::new_v4();
    assert_eq!(format(&key(), tenant), format(&key(), tenant));
}

#[test]
fn different_scopes_of_one_setting_are_different_resources() {
    let root = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    assert_ne!(format(&key(), tenant), format(&key(), root));
}

// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for the secret-type catalog and type references.

use toolkit_gts::GTS_ID_PREFIX;

use super::*;

/// Pinned v5 UUID of the v2 `generic` type id. Test-only: no production code
/// reads it (a create always names its type; the migration DEFAULT is gone).
const GENERIC_TYPE_UUID_STR: &str = "c57822de-3aae-58b7-b712-71d907c999e2";

#[test]
fn catalog_names_and_ids_are_unique_and_well_formed() {
    let mut names: Vec<_> = SECRET_TYPE_CATALOG.iter().map(|d| d.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), SECRET_TYPE_CATALOG.len(), "duplicate names");

    let mut ids: Vec<_> = SECRET_TYPE_CATALOG.iter().map(|d| d.gts_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), SECRET_TYPE_CATALOG.len(), "duplicate gts ids");

    for d in SECRET_TYPE_CATALOG {
        assert!(
            d.gts_id.starts_with(&format!(
                "{GTS_ID_PREFIX}cf.core.credstore.credential.v1~cf.core.credstore."
            )),
            "{} not derived from the credential base type",
            d.name
        );
        assert!(d.gts_id.ends_with(".v1~"));
        assert!(
            !d.allow_sharing.is_empty(),
            "{}: a type must allow at least one sharing mode",
            d.name
        );
    }
}

#[test]
fn embedded_value_schemas_are_valid_json() {
    for d in SECRET_TYPE_CATALOG {
        if let Some(schema) = d.value_schema {
            let parsed: serde_json::Value =
                serde_json::from_str(schema).expect("schema must be valid JSON");
            assert!(parsed.is_object(), "{}: schema must be an object", d.name);
        }
    }
}

#[test]
fn type_uuid_is_deterministic_and_matches_registry_v5() {
    // Every catalog type has a resolvable deterministic UUID, and it is
    // stable across calls (the stored key must never drift).
    for d in SECRET_TYPE_CATALOG {
        let a = type_uuid(d.gts_id).expect("catalog gts id resolves to a uuid");
        let b = type_uuid(d.gts_id).unwrap();
        assert_eq!(a, b, "{}: uuid must be deterministic", d.name);
        assert_eq!(a.get_version_num(), 5, "{}: must be a v5 uuid", d.name);
    }
    // UUIDs are distinct per type.
    let mut uuids: Vec<_> = SECRET_TYPE_CATALOG
        .iter()
        .map(|d| type_uuid(d.gts_id))
        .collect();
    uuids.sort();
    uuids.dedup();
    assert_eq!(uuids.len(), SECRET_TYPE_CATALOG.len(), "type uuids collide");

    // Pin the v2 generic type uuid; a drift here means the constant is stale.
    assert_eq!(
        SecretType::generic().uuid().to_string(),
        GENERIC_TYPE_UUID_STR,
        "generic type uuid drifted; update GENERIC_TYPE_UUID_STR and this pin"
    );
}

#[test]
fn resolution_by_name_and_gts_id_round_trips() {
    for d in SECRET_TYPE_CATALOG {
        let by_name = SecretType::from_name(d.name).expect("known name");
        assert_eq!(by_name.gts_id(), d.gts_id);
        let by_id = SecretType::from_gts_id(d.gts_id).expect("known id");
        assert_eq!(by_id.name(), d.name);
    }
    assert!(SecretType::from_name("no-such-type").is_none());
    assert!(SecretType::from_gts_id(&format!("{GTS_ID_PREFIX}nope~")).is_none());
}

#[test]
fn generic_allows_everything() {
    let g = SecretType::generic();
    assert_eq!(g.name(), "generic");
    for m in [
        SharingMode::Private,
        SharingMode::Tenant,
        SharingMode::Shared,
    ] {
        assert!(g.descriptor().allows_sharing(m));
    }
    assert!(!g.descriptor().expirable);
}

#[test]
fn personal_token_is_private_only_flagship_restriction() {
    let t = SecretType::from_name("personal-token").expect("known");
    assert!(t.descriptor().allows_sharing(SharingMode::Private));
    assert!(!t.descriptor().allows_sharing(SharingMode::Tenant));
    assert!(!t.descriptor().allows_sharing(SharingMode::Shared));
}

#[test]
fn secret_type_debug_and_display_name_the_catalog_entry() {
    let t = SecretType::from_name("api-key").expect("known");
    assert_eq!(t.to_string(), "api-key");
    assert_eq!(format!("{t:?}"), "SecretType(\"api-key\")");
}

#[test]
fn secret_type_traits_require_allow_sharing_and_max_size_bytes() {
    use serde_json::json;
    assert!(
        serde_json::from_value::<SecretTypeTraits>(json!({"allow_sharing": ["tenant"]})).is_err(),
        "max_size_bytes is required"
    );
    assert!(
        serde_json::from_value::<SecretTypeTraits>(json!({"max_size_bytes": 1})).is_err(),
        "allow_sharing is required"
    );
    assert!(
        serde_json::from_value::<SecretTypeTraits>(
            json!({"allow_sharing": ["tenant"], "max_size_bytes": 1})
        )
        .is_ok()
    );
}

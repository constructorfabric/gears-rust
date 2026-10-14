//! The cursor binding, the value half of the match and the batch width.

use serde_json::{Value, json};
use settings_service_sdk::EffectiveSource;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{browse_binding, candidate_batch, page_size, value_part_matches};
use crate::api::rest::setting_dto::{EffectiveValueDto, render};
use crate::domain::resolution::EffectiveValue;
use crate::domain::search::Needle;

fn needle(raw: &str) -> Needle {
    Needle::parse(raw).expect("a valid needle")
}

fn rendered(classification: &str, value: Value, fallback: Value, pii: bool) -> EffectiveValueDto {
    let at = OffsetDateTime::from_unix_timestamp(100).expect("in range");
    let tenant = Uuid::new_v4();
    render(
        &EffectiveValue {
            key: "k".to_owned(),
            declaration_id: Uuid::nil(),
            scope: format!("/tenants/{tenant}"),
            tenant_id: tenant,
            value,
            source: EffectiveSource::Inherited,
            source_scope: Some("/".to_owned()),
            fallback,
            fallback_source: EffectiveSource::SchemaDefault,
            fallback_scope: None,
            traits: json!({}),
            trail: Vec::new(),
            data_classification: classification.to_owned(),
            domain_affinity: None,
            secret_backed: false,
            declaration_last_change_at: at,
            resolved_row_last_change_at: None,
            own_row: None,
        },
        pii,
    )
}

#[test]
fn the_binding_follows_the_filter_hash_and_the_trimmed_text() {
    let base = browse_binding(None, &needle("lantern"));
    assert_eq!(base, browse_binding(None, &needle("  lantern  ")));
    assert_ne!(base, browse_binding(None, &needle("lantern!")));
    assert_ne!(base, browse_binding(Some("abc"), &needle("lantern")));
    assert_eq!(
        browse_binding(Some("abc"), &needle("lantern")),
        browse_binding(Some("abc"), &needle("lantern"))
    );
}

#[test]
fn the_value_half_matches_the_value_or_the_fallback_without_regard_to_case() {
    let dto = rendered("public", json!("Sunrise"), json!("Dusk"), false);
    assert!(value_part_matches(&dto, false, &needle("sunr")));
    assert!(value_part_matches(&dto, false, &needle("DUSK")));
    assert!(!value_part_matches(&dto, false, &needle("noon")));
}

#[test]
fn the_value_half_reads_a_structured_value_on_its_leaves() {
    let dto = rendered(
        "public",
        json!({"host": "db.internal", "port": 5432}),
        json!(true),
        false,
    );
    assert!(value_part_matches(&dto, false, &needle("db.internal")));
    assert!(value_part_matches(&dto, false, &needle("5432")));
    assert!(value_part_matches(&dto, false, &needle("true")));
}

#[test]
fn a_masked_value_never_matches() {
    let secret = rendered("secret", json!("hunter2"), json!("hunter2"), true);
    assert!(!value_part_matches(&secret, false, &needle("hunter")));
    assert!(!value_part_matches(&secret, false, &needle("***")));
    let pii = rendered("pii", json!("ops@example.test"), json!("none"), false);
    assert!(!value_part_matches(&pii, false, &needle("ops@example")));
    let entitled = rendered("pii", json!("ops@example.test"), json!("none"), true);
    assert!(value_part_matches(&entitled, false, &needle("ops@example")));
}

#[test]
fn the_batch_is_wider_than_the_page_within_its_bounds() {
    assert_eq!(candidate_batch(1), 50);
    assert_eq!(candidate_batch(25), 100);
    assert_eq!(candidate_batch(50), 200);
    assert_eq!(candidate_batch(10_000), 200);
    assert_eq!(candidate_batch(u64::MAX), 200);
}

#[test]
fn a_json_null_never_matches() {
    let dto = rendered("public", json!(null), json!(null), false);
    assert!(!value_part_matches(&dto, false, &needle("null")));
    assert!(!value_part_matches(&dto, false, &needle("nul")));
    // A null on one side does not hide a match on the other.
    let dto = rendered("public", json!(null), json!("nullable"), false);
    assert!(value_part_matches(&dto, false, &needle("nullable")));
}

#[test]
fn a_secret_backed_row_never_matches_even_when_unmasked() {
    let dto = rendered("public", json!("handle-123"), json!("none"), false);
    assert!(value_part_matches(&dto, false, &needle("handle")));
    assert!(!value_part_matches(&dto, true, &needle("handle")));
}

#[test]
fn the_binding_is_stable_across_releases() {
    // FNV-1a 64 over a marker byte, the filter hash, a separator and the
    // trimmed text: pinned, so a change of algorithm shows up here.
    assert_eq!(browse_binding(None, &needle("lantern")), "501be27823acecf8");
    assert_eq!(
        browse_binding(Some("abc"), &needle("lantern")),
        "47d98c621931cfeb"
    );
}

#[test]
fn the_page_size_follows_the_declaration_page_bounds() {
    assert_eq!(page_size(None), 50);
    assert_eq!(page_size(Some(0)), 1);
    assert_eq!(page_size(Some(7)), 7);
    assert_eq!(page_size(Some(1000)), 200);
}

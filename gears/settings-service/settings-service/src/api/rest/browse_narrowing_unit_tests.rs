//! The cursor binding, the value half of the match and the batch width.

use serde_json::{Value, json};
use settings_service_sdk::EffectiveSource;
use uuid::Uuid;

use super::{browse_binding, candidate_batch, page_size, value_part_matches};
use crate::api::rest::setting_dto::{EffectiveValueDto, Reader, render};
use crate::domain::resolution::EffectiveValue;
use crate::domain::resolution::cache::tests_entry;
use crate::domain::search::Needle;

/// The target of the pinned bindings.
const TARGET: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

fn needle(raw: &str) -> Needle {
    Needle::parse(raw).expect("a valid needle")
}

fn rendered(classification: &str, value: Value, fallback: Value, pii: bool) -> EffectiveValueDto {
    render(
        &EffectiveValue {
            value,
            source: EffectiveSource::Inherited,
            source_scope: Some("/".to_owned()),
            fallback,
            data_classification: classification.to_owned(),
            ..tests_entry("k", Uuid::new_v4())
        },
        pii,
        Reader::Platform,
        &[],
    )
}

#[test]
fn the_binding_follows_the_filter_hash_the_target_and_the_trimmed_text() {
    let lantern = needle("lantern");
    let base = browse_binding(None, TARGET, Some(&lantern));
    assert_eq!(
        base,
        browse_binding(None, TARGET, Some(&needle("  lantern  ")))
    );
    assert_ne!(
        base,
        browse_binding(None, TARGET, Some(&needle("lantern!")))
    );
    assert_ne!(base, browse_binding(None, TARGET, None));
    assert_ne!(base, browse_binding(Some("abc"), TARGET, Some(&lantern)));
    assert_ne!(base, browse_binding(None, Uuid::new_v4(), Some(&lantern)));
    assert_eq!(
        browse_binding(Some("abc"), TARGET, Some(&lantern)),
        browse_binding(Some("abc"), TARGET, Some(&lantern))
    );
    // Without `q` the filter and the target still tell two listings apart.
    let plain = browse_binding(None, TARGET, None);
    assert_ne!(plain, browse_binding(Some("abc"), TARGET, None));
    assert_ne!(plain, browse_binding(None, Uuid::new_v4(), None));
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
    // FNV-1a 64 over the filter hash, the target's hyphenated id and the
    // trimmed text, each ended by the separator: pinned, so a change of
    // algorithm shows up here.
    assert_eq!(browse_binding(None, TARGET, None), "07b75f59a49119bf");
    assert_eq!(
        browse_binding(None, TARGET, Some(&needle("lantern"))),
        "24a24cc410428241"
    );
    assert_eq!(
        browse_binding(Some("abc"), TARGET, Some(&needle("lantern"))),
        "a66bb39130004aaf"
    );
}

#[test]
fn the_page_size_follows_the_declaration_page_bounds() {
    assert_eq!(page_size(None), 50);
    assert_eq!(page_size(Some(0)), 1);
    assert_eq!(page_size(Some(7)), 7);
    assert_eq!(page_size(Some(1000)), 200);
}

use gts::GtsId;
use uuid::Uuid;

use super::{EntityField, EntityKey, EntityKind, FieldSelection, IdempotencyKey, Projection};

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid GTS identifier")
}

#[test]
fn default_and_an_explicit_default_selection_are_one_projection() {
    let explicit = Projection::Select(FieldSelection::with(&EntityField::DEFAULT));
    assert_eq!(Projection::Default, explicit);
    assert_ne!(
        Projection::Default,
        Projection::Select(FieldSelection::full())
    );
}

#[test]
fn every_selection_includes_the_mandatory_fields() {
    for selection in [
        FieldSelection::light(),
        FieldSelection::with(&[EntityField::Content]),
        FieldSelection::full(),
    ] {
        for field in EntityField::MANDATORY {
            assert!(selection.contains(field), "{selection:?} lacks {field:?}");
        }
    }
    assert!(!FieldSelection::light().contains(EntityField::Content));
}

#[test]
fn selecting_one_document_does_not_select_the_others() {
    let traits = FieldSelection::with(&[EntityField::EffectiveTraits]);
    assert!(traits.contains(EntityField::EffectiveTraits));
    assert!(!traits.contains(EntityField::ResolvedSchema));
    assert!(!traits.contains(EntityField::EffectiveTraitsSchema));
}

#[test]
fn a_key_kind_comes_from_the_trailing_tilde_and_a_reference_has_none() {
    assert_eq!(
        EntityKey::from(id("gts.cf.test.pkg.thing.v1~")).kind(),
        Some(EntityKind::TypeSchema)
    );
    assert_eq!(
        EntityKey::from(id("gts.cf.test.pkg.thing.v1~cf.test.pkg.one.v1")).kind(),
        Some(EntityKind::Instance)
    );
    assert_eq!(EntityKey::GtsUuid(Uuid::nil()).kind(), None);
}

#[test]
fn a_reference_key_displays_in_canonical_form() {
    let uuid = Uuid::parse_str("A1A2A3A4-B1B2-C1C2-D1D2-D3D4D5D6D7D8").expect("uuid");
    assert_eq!(
        EntityKey::GtsUuid(uuid).to_string(),
        "a1a2a3a4-b1b2-c1c2-d1d2-d3d4d5d6d7d8"
    );
}

#[test]
fn an_idempotency_key_is_printable_ascii_without_surrounding_spaces() {
    for ok in ["k", "op-1 retry", &"x".repeat(IdempotencyKey::MAX_LEN)] {
        assert!(IdempotencyKey::new(ok).is_ok(), "{ok:?} must be accepted");
    }
    for bad in [
        String::new(),
        " k".to_owned(),
        "k ".to_owned(),
        "key\nother".to_owned(),
        "key\rother".to_owned(),
        "key\0".to_owned(),
        "tab\tkey".to_owned(),
        "cl\u{e9}".to_owned(),
        "x".repeat(IdempotencyKey::MAX_LEN + 1),
    ] {
        assert!(
            IdempotencyKey::new(bad.clone()).is_err(),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn a_refused_idempotency_key_names_its_field_and_reason() {
    use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

    for bad in ["key\u{7f}", "", " k"] {
        let error = IdempotencyKey::new(bad).expect_err("refused");
        let CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } = &error
        else {
            panic!("{bad:?}: not a field violation: {error:?}");
        };
        assert_eq!(field_violations.len(), 1, "{bad:?}");
        assert_eq!(field_violations[0].field, "idempotency_key", "{bad:?}");
        assert_eq!(
            field_violations[0].reason,
            crate::field::INVALID_IDEMPOTENCY_KEY,
            "{bad:?}"
        );
    }
    assert!(
        IdempotencyKey::new("tilde~").is_ok(),
        "0x7E is the last printable byte"
    );
}

proptest::proptest! {
    /// Keys accept 1..=`MAX_LEN` printable ASCII bytes without surrounding spaces.
    #[test]
    fn idempotency_key_acceptance_matches_its_predicate(
        bytes in proptest::collection::vec(0_u8..0x80, 0..300),
    ) {
        let key = String::from_utf8(bytes).expect("ASCII is UTF-8");
        let expected = !key.is_empty()
            && key.len() <= IdempotencyKey::MAX_LEN
            && key.bytes().all(|b| (0x20..=0x7E).contains(&b))
            && !key.starts_with(' ')
            && !key.ends_with(' ');
        proptest::prop_assert_eq!(IdempotencyKey::new(key.clone()).is_ok(), expected, "{:?}", key);
    }
}

#[test]
fn generated_keys_are_valid_and_distinct() {
    let a = IdempotencyKey::generate();
    let b = IdempotencyKey::generate();
    assert_ne!(a, b);
    assert!(IdempotencyKey::new(a.as_str()).is_ok());
}

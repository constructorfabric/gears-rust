use super::*;

fn parse(names: &[&str]) -> Result<FieldSelection, SelectionError> {
    FieldSelection::parse(names)
}

#[test]
fn absent_selection_equals_the_explicit_default_set() {
    let explicit = parse(&["gts_id", "gts_uuid", "kind", "origin", "lifecycle_status"])
        .expect("the default spelled out is a valid selection");
    assert_eq!(explicit, FieldSelection::default());
    assert_eq!(
        FieldSelection::default().canonical(),
        "gts_id,gts_uuid,kind,lifecycle_status,origin"
    );
}

#[test]
fn order_and_case_and_surrounding_whitespace_do_not_change_identity() {
    let a = parse(&["content", "gts_id"]).expect("valid");
    let b = parse(&[" GTS_ID ", "Content"]).expect("valid");
    assert_eq!(a, b);
    assert_eq!(a.canonical(), b.canonical());
}

#[test]
fn mandatory_fields_are_always_in_the_normalized_set() {
    let without = parse(&["content"]).expect("valid");
    let with =
        parse(&["gts_uuid", "content", "kind", "lifecycle_status", "gts_id"]).expect("valid");
    for field in FieldSelection::MANDATORY_FIELDS {
        assert!(without.contains(field), "{}", field.name());
    }
    assert_eq!(without, with, "naming a mandatory field changes nothing");
    assert_eq!(
        without.canonical(),
        "content,gts_id,gts_uuid,kind,lifecycle_status"
    );
}

#[test]
fn every_selectable_field_round_trips_through_its_name() {
    for field in EntityField::ALL {
        assert_eq!(EntityField::from_name(field.name()), Some(field));
        let one = parse(&[field.name()]).expect("each field is selectable alone");
        assert!(one.contains(field), "{}", field.name());
    }
    assert_eq!(
        FieldSelection::full().fields().count(),
        EntityField::ALL.len()
    );
}

#[test]
fn documents_are_selected_individually() {
    let traits = parse(&["effective_traits"]).expect("valid");
    assert!(traits.contains(EntityField::EffectiveTraits));
    assert!(!traits.contains(EntityField::ResolvedSchema));
    assert!(!traits.contains(EntityField::Content));
    assert!(!FieldSelection::default().selects_any_document());
    assert!(traits.selects_any_document());
}

#[test]
fn an_empty_selection_is_refused() {
    assert_eq!(parse(&[]), Err(SelectionError::Empty));
    assert!(matches!(parse(&["  "]), Err(SelectionError::EmptySegment)));
}

#[test]
fn an_empty_segment_is_refused_rather_than_dropped() {
    assert_eq!(
        parse(&["content", "", "kind"]),
        Err(SelectionError::EmptySegment)
    );
}

#[test]
fn a_duplicate_is_refused_after_normalization() {
    assert_eq!(
        parse(&["content", "CONTENT"]),
        Err(SelectionError::Duplicate("content".to_owned()))
    );
}

#[test]
fn unknown_unavailable_and_nested_names_are_told_apart() {
    assert_eq!(
        parse(&["contents"]),
        Err(SelectionError::Unknown("contents".to_owned()))
    );
    for name in ["availability", "owned_by_context_tenant"] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Unavailable(name.to_owned())),
            "{name} is a DESIGN field P0 cannot answer",
        );
    }
    for name in [
        "content.title",
        "effective/resolved_schema",
        "provenance.owning_gear",
    ] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Nested(name.to_owned())),
            "{name}",
        );
    }
}

/// `key`, `status` and `etag` are envelope metadata, never selectable.
#[test]
fn envelope_metadata_is_not_selectable() {
    for name in ["key", "status", "etag", "resource_version", "owning_gear"] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Unknown(name.to_owned())),
            "{name}",
        );
    }
}

#[test]
fn the_canonical_spelling_parses_back_to_the_same_selection() {
    for selection in [
        FieldSelection::default(),
        FieldSelection::full(),
        parse(&["provenance"]).expect("valid"),
    ] {
        let canonical = selection.canonical();
        let names: Vec<&str> = canonical.split(',').collect();
        assert_eq!(parse(&names), Ok(selection), "{canonical}");
    }
}

#[test]
fn every_field_is_listed_once_in_canonical_order() {
    use EntityField as F;
    // A new variant fails to compile here, pointing at `expected`.
    let exhaustive = |field: EntityField| match field {
        F::Content
        | F::EffectiveTraits
        | F::EffectiveTraitsSchema
        | F::GtsId
        | F::GtsUuid
        | F::Kind
        | F::LifecycleStatus
        | F::Origin
        | F::Provenance
        | F::ResolvedSchema => (),
    };
    let expected = [
        F::Content,
        F::EffectiveTraits,
        F::EffectiveTraitsSchema,
        F::GtsId,
        F::GtsUuid,
        F::Kind,
        F::LifecycleStatus,
        F::Origin,
        F::Provenance,
        F::ResolvedSchema,
    ];
    expected.into_iter().for_each(exhaustive);
    assert_eq!(EntityField::ALL, expected);
    let names = EntityField::ALL.map(EntityField::name);
    assert!(
        names.windows(2).all(|w| w[0] < w[1]),
        "unique and sorted: {names:?}"
    );
}

#[test]
fn every_selection_round_trips_through_its_canonical_spelling() {
    for mask in 1_u16..(1 << EntityField::ALL.len()) {
        let names: Vec<&str> = EntityField::ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, field)| field.name())
            .collect();
        let selection = parse(&names).expect("valid");
        let canonical = selection.canonical();
        let back: Vec<&str> = canonical.split(',').collect();
        assert_eq!(parse(&back), Ok(selection), "{canonical}");
        let respelled: Vec<String> = names
            .iter()
            .rev()
            .map(|name| format!(" {} ", name.to_uppercase()))
            .collect();
        assert_eq!(
            FieldSelection::parse(&respelled).map(FieldSelection::canonical),
            Ok(canonical)
        );
    }
}

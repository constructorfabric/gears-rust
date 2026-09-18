//! Unit tests for the collection read's `$filter`/`$orderby`/`$select`
//! validation (ADR-0005/ADR-0004).

use time::macros::datetime;
use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

use super::*;
use crate::domain::error::DomainError;

fn parse(raw: &str) -> Result<ParsedFilter, DomainError> {
    let parsed = toolkit_odata::parse_filter_string(raw).expect("valid OData syntax");
    parse_filter(parsed.as_expr())
}

fn reason_of(err: &DomainError) -> &'static str {
    match err {
        DomainError::InvalidRequest { reason, .. } => reason,
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[test]
fn reference_eq_and_in_are_sql_clamps() {
    let f = parse("reference eq 'openai-key'").expect("valid");
    assert_eq!(f.reference_in, Some(vec!["openai-key".to_owned()]));
    assert!(f.type_uuid_in.is_none());

    let f = parse("reference in ('a', 'b', 'c')").expect("valid");
    assert_eq!(
        f.reference_in,
        Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
    );
}

#[test]
fn reference_rejects_other_operators() {
    let err = parse("startswith(reference, 'op')").expect_err("startswith must be rejected");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

#[test]
fn type_eq_parses_the_full_gts_id_into_a_uuid() {
    let gts_id = "gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~";
    let f = parse(&format!("type eq '{gts_id}'")).expect("valid");
    let expected = credstore_sdk::GtsId::try_new(gts_id)
        .expect("valid")
        .to_uuid();
    assert_eq!(f.type_uuid_in, Some(vec![expected]));
}

#[test]
fn type_rejects_an_unparseable_gts_id() {
    let err = parse("type eq 'not-a-gts-id'").expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

#[test]
fn duplicate_field_is_rejected() {
    let err =
        parse("reference eq 'a' and reference eq 'b'").expect_err("duplicate must be rejected");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

#[test]
fn or_and_not_are_rejected() {
    assert!(parse("reference eq 'a' or reference eq 'b'").is_err());
    assert!(parse("not (reference eq 'a')").is_err());
}

#[test]
fn sharing_and_fallback_parse_known_wire_values() {
    let f = parse("sharing eq 'shared'").expect("valid");
    assert_eq!(f.sharing_eq, Some(SharingMode::Shared));

    let f = parse("fallback eq 'none'").expect("valid");
    assert_eq!(f.fallback_eq, Some(Fallback::None));

    let err = parse("sharing eq 'bogus'").expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

#[test]
fn expires_at_supports_ordering_comparators() {
    let f = parse("expires_at gt 2030-01-01T00:00:00Z").expect("valid");
    let (op, at) = f.expires_at.expect("present");
    assert_eq!(op, toolkit_odata::filter::FilterOp::Gt);
    assert_eq!(at, datetime!(2030-01-01 0:00 UTC));
}

#[test]
fn combined_reference_and_sharing_filter_parses_both() {
    let f = parse("reference eq 'r' and sharing eq 'tenant'").expect("valid");
    assert_eq!(f.reference_in, Some(vec!["r".to_owned()]));
    assert_eq!(f.sharing_eq, Some(SharingMode::Tenant));
}

#[test]
fn matches_post_reduction_applies_sharing_fallback_expires_at() {
    let f = parse("sharing eq 'shared'").expect("valid");
    assert!(f.matches_post_reduction(SharingMode::Shared, None, None));
    assert!(!f.matches_post_reduction(SharingMode::Tenant, None, None));

    let f = parse("fallback eq 'none'").expect("valid");
    assert!(f.matches_post_reduction(SharingMode::Tenant, Some(Fallback::None), None));
    assert!(!f.matches_post_reduction(SharingMode::Tenant, Some(Fallback::Inherit), None));
    // No own row (fallback: None) never matches a fallback predicate.
    assert!(!f.matches_post_reduction(SharingMode::Tenant, None, None));

    let no_filter = ParsedFilter::default();
    assert!(no_filter.matches_post_reduction(SharingMode::Private, None, None));
}

#[test]
fn value_mode_selector_accepts_exactly_reference_or_type() {
    let f = parse("reference eq 'r'").expect("valid");
    assert!(f.require_value_mode_selector().is_ok());

    let f = parse("type eq 'gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~'")
        .expect("valid");
    assert!(f.require_value_mode_selector().is_ok());

    let f = parse("sharing eq 'shared'").expect("valid");
    let err = f.require_value_mode_selector().expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::VALUE_MODE_SELECTOR);

    let neither = ParsedFilter::default();
    assert!(neither.require_value_mode_selector().is_err());
}

#[test]
fn select_allowlist_accepts_credential_fields_and_value() {
    for field in [
        "reference",
        "type",
        "sharing",
        "status",
        "fallback",
        "expires_at",
        "inheritance",
        "version",
        "updated_at",
        "owner_id",
        "value",
    ] {
        assert!(validate_select(&[field.to_owned()]).is_ok(), "{field}");
    }
    let err = validate_select(&["bogus".to_owned()]).expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::INVALID_SELECT);
}

#[test]
fn is_value_mode_detects_value_in_select() {
    assert!(is_value_mode(Some(&[
        "reference".to_owned(),
        "value".to_owned()
    ])));
    assert!(!is_value_mode(Some(&["reference".to_owned()])));
    assert!(!is_value_mode(None));
}

#[test]
fn admin_field_selected_detects_each_administrative_field_but_not_envelope_fields() {
    for field in [
        "sharing",
        "status",
        "fallback",
        "inheritance",
        "version",
        "updated_at",
        "owner_id",
    ] {
        assert!(
            admin_field_selected(Some(&[field.to_owned()])),
            "{field} must be detected as an administrative field"
        );
    }
    for field in ["reference", "type", "expires_at", "value"] {
        assert!(
            !admin_field_selected(Some(&[field.to_owned()])),
            "{field} must not be treated as administrative"
        );
    }
    assert!(!admin_field_selected(None));
}

#[test]
fn orderby_defaults_to_ascending_when_absent() {
    let dir = validate_metadata_orderby(&ODataOrderBy::empty()).expect("valid");
    assert_eq!(dir, ListDirection::Asc);
}

#[test]
fn orderby_accepts_reference_asc_and_desc() {
    let asc = ODataOrderBy(vec![OrderKey {
        field: "reference".to_owned(),
        dir: SortDir::Asc,
    }]);
    assert_eq!(
        validate_metadata_orderby(&asc).expect("valid"),
        ListDirection::Asc
    );

    let desc = ODataOrderBy(vec![OrderKey {
        field: "reference".to_owned(),
        dir: SortDir::Desc,
    }]);
    assert_eq!(
        validate_metadata_orderby(&desc).expect("valid"),
        ListDirection::Desc
    );
}

#[test]
fn orderby_rejects_any_other_field_or_multiple_keys() {
    let other = ODataOrderBy(vec![OrderKey {
        field: "updated_at".to_owned(),
        dir: SortDir::Asc,
    }]);
    let err = validate_metadata_orderby(&other).expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::INVALID_ORDERBY_FIELD);

    let multiple = ODataOrderBy(vec![
        OrderKey {
            field: "reference".to_owned(),
            dir: SortDir::Asc,
        },
        OrderKey {
            field: "reference".to_owned(),
            dir: SortDir::Desc,
        },
    ]);
    assert!(validate_metadata_orderby(&multiple).is_err());
}

#[test]
fn expires_at_predicate_applies_every_comparator_post_reduction() {
    let at = datetime!(2030-01-01 0:00 UTC);
    let earlier = datetime!(2029-01-01 0:00 UTC);
    let later = datetime!(2031-01-01 0:00 UTC);
    let with = |op| ParsedFilter {
        expires_at: Some((op, at)),
        ..ParsedFilter::default()
    };
    let matches = |f: &ParsedFilter, actual| {
        f.matches_post_reduction(SharingMode::Tenant, None, Some(actual))
    };

    assert!(matches(&with(FilterOp::Eq), at));
    assert!(!matches(&with(FilterOp::Eq), later));
    assert!(matches(&with(FilterOp::Ne), later));
    assert!(!matches(&with(FilterOp::Ne), at));
    assert!(matches(&with(FilterOp::Gt), later));
    assert!(!matches(&with(FilterOp::Gt), at));
    assert!(matches(&with(FilterOp::Ge), at));
    assert!(!matches(&with(FilterOp::Ge), earlier));
    assert!(matches(&with(FilterOp::Lt), earlier));
    assert!(!matches(&with(FilterOp::Lt), at));
    assert!(matches(&with(FilterOp::Le), at));
    assert!(!matches(&with(FilterOp::Le), later));
}

#[test]
fn a_row_without_an_expiry_never_matches_an_expires_at_predicate() {
    let f = parse("expires_at lt 2030-01-01T00:00:00Z").expect("valid");
    assert!(!f.matches_post_reduction(SharingMode::Tenant, None, None));
}

#[test]
fn type_in_list_parses_every_member_into_a_uuid() {
    let a = "gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~";
    let b = "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~";
    let f = parse(&format!("type in ('{a}', '{b}')")).expect("valid");
    let expected: Vec<_> = [a, b]
        .iter()
        .map(|id| credstore_sdk::GtsId::try_new(id).expect("valid").to_uuid())
        .collect();
    assert_eq!(f.type_uuid_in, Some(expected));
}

#[test]
fn every_field_is_rejected_when_named_twice() {
    for raw in [
        "reference in ('a') and reference in ('b')",
        "type eq 'gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~' and \
         type eq 'gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~'",
        "sharing eq 'shared' and sharing eq 'tenant'",
        "fallback eq 'none' and fallback eq 'inherit'",
        "expires_at gt 2029-01-01T00:00:00Z and expires_at lt 2031-01-01T00:00:00Z",
    ] {
        let err = parse(raw).expect_err("duplicate field must be rejected");
        assert_eq!(reason_of(&err), reasons::INVALID_FILTER, "{raw}");
    }
}

#[test]
fn only_eq_and_in_are_accepted_on_the_string_fields() {
    for raw in [
        "reference ne 'a'",
        "type ne 'gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~'",
        "sharing ne 'shared'",
        "fallback ne 'none'",
    ] {
        let err = parse(raw).expect_err("`ne` must be rejected");
        assert_eq!(reason_of(&err), reasons::INVALID_FILTER, "{raw}");
    }
}

#[test]
fn in_lists_are_rejected_on_the_in_memory_fields() {
    for raw in [
        "sharing in ('shared', 'tenant')",
        "fallback in ('none', 'inherit')",
        "expires_at in (2030-01-01T00:00:00Z)",
    ] {
        let err = parse(raw).expect_err("`in` must be rejected");
        assert_eq!(reason_of(&err), reasons::INVALID_FILTER, "{raw}");
    }
}

#[test]
fn fallback_rejects_an_unknown_wire_value() {
    let err = parse("fallback eq 'bogus'").expect_err("must reject");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

#[test]
fn value_converters_reject_a_value_of_the_wrong_shape() {
    // Defensive arms: the generic parser type-checks a field's value against
    // its `FieldKind` before these are reached, so they are only reachable
    // by calling the converter directly.
    let err = string_value(CredentialFilterField::Sharing, &ODataValue::Bool(true))
        .expect_err("a non-string value must be rejected");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);

    let err = expires_at_value(&ODataValue::String("2030-01-01".to_owned()))
        .expect_err("a non-datetime value must be rejected");
    assert_eq!(reason_of(&err), reasons::INVALID_FILTER);
}

//! Tests for the `$filter` / `$orderby` schema and the metadata-filter
//! parameter.

use serde_json::json;
use toolkit_odata::filter::FilterField as _;

use super::{
    KEYSET_SAFE_RECORD_FIELDS, MetadataFilter, UsageRecordFilterField, is_keyset_safe_record_field,
};
use crate::v2::error::UsageCollectorError;

#[test]
fn metadata_filter_new_accepts_non_empty_key_and_values() {
    let f =
        MetadataFilter::new("region", ["us-east-1", "eu-west-1"]).expect("valid metadata filter");
    assert_eq!(f.key().as_str(), "region");
    assert_eq!(
        f.values(),
        &["us-east-1".to_owned(), "eu-west-1".to_owned()]
    );
}

#[test]
fn metadata_filter_new_rejects_empty_key() {
    let err = MetadataFilter::new("", ["v"]).expect_err("empty key must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata_filter"),
        "expected InvalidMetadataFilter, got {err:?}"
    );
}

#[test]
fn metadata_filter_new_rejects_nul_byte_in_key() {
    let err = MetadataFilter::new("region\0", ["v"])
        .expect_err("NUL bytes in key must be rejected for jsonb compatibility");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata_filter"
    ));
}

/// The read-side half of `reject_nul_metadata_values`. A filter value is
/// bound against `r.metadata ->> $N`, which is `text`, and Postgres `text`
/// cannot hold a NUL — so a value carrying one fails inside the plugin as an
/// `Internal`, a 500 the caller provoked, exactly as an ingested one would.
#[test]
fn metadata_filter_new_rejects_nul_byte_in_a_value() {
    let err = MetadataFilter::new("region", ["us-east-1", "eu-west-1\0"])
        .expect_err("NUL bytes in a value must be rejected, as ingestion rejects them");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata_filter"
    ));
}

#[test]
fn metadata_filter_new_rejects_empty_values() {
    let err = MetadataFilter::new("region", Vec::<&str>::new())
        .expect_err("empty values must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument { ref field, .. } if field.as_str() == "metadata_filter"
    ));
}

#[test]
fn metadata_filter_serde_round_trips_wire_shape() {
    let f = MetadataFilter::new("region", ["us-east-1"]).expect("valid metadata filter");
    let value = serde_json::to_value(&f).expect("serialize MetadataFilter");
    assert_eq!(
        value,
        json!({"key": "region", "values": ["us-east-1"]}),
        "wire shape must be {{key, values}}; got {value}"
    );
    let decoded: MetadataFilter = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, f);
}

#[test]
fn metadata_filter_deserialize_routes_through_new_and_surfaces_validation() {
    let err = serde_json::from_value::<MetadataFilter>(json!({"key": "", "values": ["v"]}))
        .expect_err("empty key must surface as a serde error");
    assert!(
        err.to_string().contains("metadata key must not be empty"),
        "serde error must carry the MetadataKey validation detail; got {err}"
    );

    let err = serde_json::from_value::<MetadataFilter>(json!({"key": "region", "values": []}))
        .expect_err("empty values must surface as a serde error");
    assert!(
        err.to_string().contains("must carry at least one value"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn metadata_filter_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<MetadataFilter>(
        json!({"key": "region", "values": ["v"], "op": "eq"}),
    )
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn usage_record_query_filter_surface_rejects_gts_type_id_eq() {
    let err = toolkit_odata::filter::parse_odata_filter::<UsageRecordFilterField>(
        "gts_type_id eq 'gts.cf.core.uc.usage_record.v2~cf.compute._.vcpu_hours.v1~'",
    )
    .expect_err("gts_type_id must not be exposed on the OData filter surface");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_type_id"
        ),
        "expected UnknownField(\"gts_type_id\"), got {err:?}",
    );
}

#[test]
fn usage_record_query_filter_surface_rejects_gts_type_id_in_list() {
    let err = toolkit_odata::filter::parse_odata_filter::<UsageRecordFilterField>(
        "gts_type_id in ('a', 'b')",
    )
    .expect_err("gts_type_id must not be exposed on the OData filter surface");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_type_id"
        ),
        "expected UnknownField(\"gts_type_id\"), got {err:?}",
    );
}

#[test]
fn usage_record_query_filter_surface_rejects_gts_type_id_inside_composite() {
    let err = toolkit_odata::filter::parse_odata_filter::<UsageRecordFilterField>(
        "tenant_id eq 22222222-2222-2222-2222-222222222222 and gts_type_id eq 'x'",
    )
    .expect_err("gts_type_id-touching predicates must be rejected at parse time");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_type_id"
        ),
        "expected UnknownField(\"gts_type_id\"), got {err:?}",
    );
}

#[test]
fn the_filter_surface_carries_every_fixed_field_design_declares() {
    // The converse of the three refusals above: DESIGN §3.1's
    // `UsageRecordFilterField` row makes the admissible `$filter` set
    // these eight fixed names plus the queried meter's declared metadata
    // keys, resolved per request. Pinned because the schema struct is the
    // only place the fixed half is written down, and a name dropped from
    // it does not fail loudly — it degrades to the same `UnknownField`
    // refusal `gts_type_id` gets, which reads to a caller like their own
    // mistake.
    //
    // `$filter` only. The DESIGN row names `group_by` too, but the
    // aggregate path resolves a dimension through `AggregationDimension`,
    // a separate and currently narrower enum: five of these eight, with
    // no `entry_type`, `invalidates` or `origin`. This asserts nothing
    // about it.
    //
    // A lower bound, not the whole schema: `id`, `window_start` and
    // `window_end` are on it too, for the keyset and the plugin's
    // field-to-column mapping rather than for filtering — see the
    // file-level comment above `UsageRecordQuery`.
    for field in [
        "tenant_id",
        "resource_id",
        "resource_type",
        "subject_id",
        "subject_type",
        "entry_type",
        "origin",
        "invalidates",
    ] {
        assert!(
            UsageRecordFilterField::from_name(field).is_some(),
            "`{field}` is a fixed filterable field per DESIGN §3.1",
        );
    }
}

#[test]
fn the_keyset_safe_allowlist_is_exactly_the_mandatory_record_attributes() {
    // The anchor for the exported set. `is_keyset_safe_record_field` reads
    // `KEYSET_SAFE_RECORD_FIELDS`, and the caller-facing `$orderby`
    // rejection quotes it verbatim, so the constant IS the wire contract
    // and a silent addition to it would widen the admissible order surface
    // with nothing noticing. Spelled out as literals for that reason.
    assert_eq!(
        KEYSET_SAFE_RECORD_FIELDS,
        [
            "id",
            "window_start",
            "window_end",
            "tenant_id",
            "resource_id",
            "resource_type",
            "origin",
            "accepted_at",
        ],
    );
}

#[test]
fn every_admissible_order_key_resolves_to_a_column() {
    // Being on the allowlist is necessary but not sufficient: the plugin
    // resolves an order key through `UsageRecordFilterField`, so a name
    // admissible here but absent from the filterable schema is an
    // unmappable order key the gateway happily forwards.
    //
    // Two independently maintained spellings of one vocabulary, and this is
    // the only thing checking they agree in this direction; the converse —
    // a name on the filterable schema that must never be an order key — is
    // `derivation_not_presence_separates_entry_type_from_origin` below, and
    // neither guard catches the other's case.
    //
    // This replaces a test that re-asserted the same literals the anchor
    // above pins, through a predicate that reads that very constant: it
    // could not fail unless the anchor failed first.
    //
    for field in KEYSET_SAFE_RECORD_FIELDS {
        assert!(
            is_keyset_safe_record_field(field),
            "`{field}` is on the allowlist, so the predicate reading it must agree",
        );
        assert!(
            UsageRecordFilterField::from_name(field).is_some(),
            "`{field}` is admissible as an order key but has no field-to-column \
             mapping on the filterable schema, so a plugin cannot resolve it",
        );
    }
}

#[test]
fn derivation_not_presence_separates_entry_type_from_origin() {
    // The converse of the guard above, and the half it cannot supply.
    // Resolving to a column is necessary but not sufficient: `entry_type`
    // is on the filterable schema (the wire contract's `$filter` parameter
    // names it) and therefore resolves through `from_name`, while being a
    // function of the optional `invalidates` that the SDK guarantees no
    // key for. The guard above passes on it. This one does not.
    //
    // `origin` is the positive control, and it is what keeps the ground
    // honest: it too has a value on every entry, and it too resolves
    // through `from_name`, so neither presence nor column resolution can
    // be what refuses `entry_type`. Being a field of the record rather
    // than a function of one is the whole of the difference.
    for field in ["entry_type", "origin"] {
        assert!(
            UsageRecordFilterField::from_name(field).is_some(),
            "`{field}` is a filterable field per the wire contract",
        );
    }
    assert!(
        !is_keyset_safe_record_field("entry_type"),
        "`entry_type` is a function of the optional `invalidates`, so the SDK \
         promises no keyset key over it however present the value is",
    );
    assert!(
        is_keyset_safe_record_field("origin"),
        "`origin` is a field of the record itself, so the SDK does promise a \
         keyset key over it",
    );
}

#[test]
fn the_order_key_refusal_names_the_derived_ground_alongside_the_optional_one() {
    // The refusal detail is caller-facing and interpolates
    // `KEYSET_SAFE_RECORD_FIELDS` verbatim, so it is the wire contract for
    // what may be ordered by. `entry_type` makes a third refusal reachable:
    // a recognised filter field that is neither domain-optional nor
    // unrecognised, refused because it is derived. A detail naming only
    // the optional and unknown grounds would misdescribe it, and this is
    // the only thing pinning that the wording covers the case. Lives here
    // rather than beside the constructor because it is a claim about the
    // keyset vocabulary this module owns; `error.rs` carries no sibling
    // test file.
    let err = UsageCollectorError::inadmissible_order_key("entry_type");
    let UsageCollectorError::InvalidArgument {
        ref field,
        ref detail,
        ..
    } = err
    else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "$orderby");
    assert!(
        detail.contains("entry_type"),
        "the refusal must echo the key the caller sent; got {detail}",
    );
    assert!(
        detail.contains("derived"),
        "a derived key is refused on its own ground, not as an optional or \
         unrecognised one; got {detail}",
    );
    assert!(
        detail.contains("resource_type"),
        "the refusal must name the admissible set so the caller is told what \
         they may order by; got {detail}",
    );
}

#[test]
fn created_at_is_no_longer_a_record_field() {
    // An entry carries a covered period, not a creation instant, so
    // `created_at` has to fail closed on both halves of the read surface
    // rather than resolve to something: it is not an admissible order key,
    // and it is not on the filterable-field schema at all. A stale
    // `$orderby=created_at` must be rejected, never silently reinterpreted.
    assert!(
        !is_keyset_safe_record_field("created_at"),
        "`created_at` is not a record attribute and must not be an order key",
    );
    let err = toolkit_odata::filter::parse_odata_filter::<UsageRecordFilterField>(
        "created_at eq 2026-01-01T00:00:00Z",
    )
    .expect_err("created_at is no longer on the filterable-field schema");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "created_at"
        ),
        "expected UnknownField(\"created_at\"), got {err:?}",
    );
}

#[test]
fn the_covered_period_bounds_resolve_on_the_filterable_field_schema() {
    // The bounds are on this schema although a `$filter` may never name
    // them, because the schema is also the plugin's field-to-column
    // mapping and the `$orderby` / cursor-token vocabulary: `window_end`
    // has to resolve to a column for the canonical `(window_end, id)`
    // keyset to mean anything. Parsing succeeding here is therefore the
    // intended state, and the host crate's reserved-filter-field guard —
    // not an `UnknownField` from this schema — is what keeps a predicate
    // off them.
    for expr in [
        "window_start eq 2026-01-01T00:00:00Z",
        "window_end eq 2026-01-01T00:00:00Z",
    ] {
        toolkit_odata::filter::parse_odata_filter::<UsageRecordFilterField>(expr).unwrap_or_else(
            |e| panic!("`{expr}` must resolve against the filterable-field schema: {e:?}"),
        );
    }
}

#[test]
fn keyset_unsafe_record_fields_are_the_domain_optional_ones() {
    // `subject_ref` (→ subject_id, subject_type) is `Option`al on
    // `UsageRecord`, and the `invalidates` filter field reads a target that
    // is present only on an invalidation, so all three can be absent and
    // their columns are nullable. A row-value tuple comparison with a NULL
    // leading key evaluates to NULL in Postgres, silently dropping NULL
    // rows from the page — so they are NOT keyset-safe.
    for field in ["subject_id", "subject_type", "invalidates"] {
        assert!(
            !is_keyset_safe_record_field(field),
            "`{field}` is a domain-optional attribute and must NOT be keyset-safe",
        );
    }
}

#[test]
fn keyset_safe_record_field_is_fail_closed_for_unknown_names() {
    // Fail-closed allowlist: an unknown field (or a future field someone
    // forgets to classify) is treated as unsafe rather than silently allowed.
    assert!(!is_keyset_safe_record_field("value"));
    assert!(!is_keyset_safe_record_field("definitely_not_a_field"));
    assert!(!is_keyset_safe_record_field(""));
    // Exact match, not a prefix or case-folded one: the allowlist is the
    // whole gate on the `$orderby` surface.
    assert!(!is_keyset_safe_record_field("window_en"));
    assert!(!is_keyset_safe_record_field("window_endd"));
    assert!(!is_keyset_safe_record_field("WINDOW_END"));
}

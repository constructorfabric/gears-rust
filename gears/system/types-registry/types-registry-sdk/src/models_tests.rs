use std::num::NonZeroU8;

use gts::GtsId;
use uuid::Uuid;

use super::{
    BatchGetEntitiesRequest, BatchGetItem, Cursor, EntityField, EntityFilter, EntityKey,
    EntityKind, FieldSelection, IdempotencyKey, LifecycleFilter, ListEntitiesRequest, PageRequest,
    Projection, ReadOptions, Validator,
};

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid GTS identifier")
}

#[test]
fn default_and_an_explicit_default_selection_differ_but_normalize_alike() {
    let explicit = Projection::Select(FieldSelection::with(&EntityField::DEFAULT));
    // Different requests: the kind-narrowed list helpers give `Default` documents.
    assert_ne!(Projection::Default, explicit);
    assert_eq!(Projection::Default.normalized(), explicit.normalized());
    assert_ne!(
        Projection::Default.normalized(),
        FieldSelection::full(),
        "the default selection is document-free"
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

// kind-narrowed read models.

mod narrowed {
    use gts::{GtsId, GtsInstanceId, GtsTypeId};
    use serde_json::json;
    use uuid::Uuid;

    use crate::models::{Entity, EntityKind, Instance, LifecycleStatus, Provenance, TypeSchema};

    const TYPE: &str = "gts.cf.test.pkg.thing.v1~";
    const INSTANCE: &str = "gts.cf.test.pkg.thing.v1~cf.test.pkg.one.v1";

    fn snapshot(gts_id: &str, kind: EntityKind) -> Entity {
        Entity {
            gts_id: GtsId::try_new(gts_id).expect("valid"),
            gts_uuid: Uuid::from_u128(7),
            kind,
            lifecycle_status: LifecycleStatus::Active,
            origin: None,
            content: Some(json!({ "n": 1 })),
            resolved_schema: None,
            effective_traits: None,
            effective_traits_schema: None,
            provenance: None,
        }
    }

    #[test]
    fn a_type_schema_snapshot_becomes_a_type_schema_with_every_field() {
        let mut from = snapshot(TYPE, EntityKind::TypeSchema);
        from.resolved_schema = Some(serde_json::Value::Null);
        let schema = TypeSchema::try_from(from).expect("a Type Schema");

        assert_eq!(schema.type_id, GtsTypeId::try_new(TYPE).expect("valid"));
        assert_eq!(schema.type_uuid, Uuid::from_u128(7));
        assert_eq!(schema.content, Some(json!({ "n": 1 })));
        assert_eq!(
            schema.resolved_schema,
            Some(serde_json::Value::Null),
            "a selected null stays selected"
        );
        assert_eq!(
            schema.effective_traits, None,
            "an unselected one stays unselected"
        );
        assert_eq!(schema.provenance, None);
    }

    #[test]
    fn a_type_schema_keeps_its_selected_provenance() {
        for skipped in [true, false] {
            let provenance = Some(Provenance {
                compatibility_check_skipped: skipped,
            });
            let mut from = snapshot(TYPE, EntityKind::TypeSchema);
            from.provenance = provenance;
            let schema = TypeSchema::try_from(from).expect("a Type Schema");
            assert_eq!(schema.provenance, provenance);
        }
    }

    #[test]
    fn an_instance_snapshot_names_the_type_it_conforms_to() {
        let instance =
            Instance::try_from(snapshot(INSTANCE, EntityKind::Instance)).expect("an Instance");

        assert_eq!(
            instance.id,
            GtsInstanceId::try_new(INSTANCE).expect("valid")
        );
        assert_eq!(instance.type_id, GtsTypeId::try_new(TYPE).expect("valid"));
        assert_eq!(instance.content, Some(json!({ "n": 1 })));
    }

    #[test]
    fn the_other_kind_is_handed_back_unchanged() {
        let instance = snapshot(INSTANCE, EntityKind::Instance);
        assert_eq!(TypeSchema::try_from(instance.clone()), Err(instance));
        let schema = snapshot(TYPE, EntityKind::TypeSchema);
        assert_eq!(Instance::try_from(schema.clone()), Err(schema));
    }

    #[test]
    fn inconsistent_kind_data_is_refused() {
        // The kind field and the identifier disagree.
        let mislabeled = snapshot(TYPE, EntityKind::Instance);
        assert_eq!(Instance::try_from(mislabeled.clone()), Err(mislabeled));
        let mislabeled = snapshot(INSTANCE, EntityKind::TypeSchema);
        assert_eq!(TypeSchema::try_from(mislabeled.clone()), Err(mislabeled));

        // An Instance has no derived form to carry.
        let mut materialized = snapshot(INSTANCE, EntityKind::Instance);
        materialized.effective_traits = Some(json!({}));
        assert_eq!(Instance::try_from(materialized.clone()), Err(materialized));

        // Nor public provenance, which has no field applicable to an Instance.
        for skipped in [true, false] {
            let mut flagged = snapshot(INSTANCE, EntityKind::Instance);
            flagged.provenance = Some(Provenance {
                compatibility_check_skipped: skipped,
            });
            assert_eq!(Instance::try_from(flagged.clone()), Err(flagged));
        }
    }
}

// publisher version.

/// Publisher version precedence.
mod publisher_version {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use crate::models::{PublisherVersion, PublisherVersionError};

    fn v(s: &str) -> PublisherVersion {
        s.parse().expect("valid publisher version")
    }

    fn hash_of(version: &PublisherVersion) -> u64 {
        let mut hasher = DefaultHasher::new();
        version.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn numeric_components_compare_as_numbers_not_strings() {
        assert!(v("0.2.0") < v("0.10.0"));
    }

    #[test]
    fn a_prerelease_precedes_its_release() {
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
    }

    #[test]
    fn build_metadata_does_not_count_for_equality_ordering_or_hashing() {
        let a = v("1.0.0+a");
        let b = v("1.0.0+b");

        assert_eq!(a, b);
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn display_keeps_the_declared_text_including_build_metadata() {
        assert_eq!(v("1.2.3-rc.1+build.7").to_string(), "1.2.3-rc.1+build.7");
    }

    #[test]
    fn invalid_semver_is_refused() {
        for input in ["", "1.0", "v1.0.0", "1.0.0 ", "01.0.0", "latest"] {
            assert!(
                matches!(
                    input.parse::<PublisherVersion>(),
                    Err(PublisherVersionError::Invalid { .. })
                ),
                "{input:?} must be refused as invalid"
            );
        }
    }

    #[test]
    fn input_at_the_length_bound_is_accepted_and_above_it_refused() {
        let at_bound = format!(
            "1.0.0-{}",
            "a".repeat(PublisherVersion::MAX_LEN - "1.0.0-".len())
        );
        assert_eq!(at_bound.len(), PublisherVersion::MAX_LEN);
        assert!(at_bound.parse::<PublisherVersion>().is_ok());

        let above = format!("{at_bound}a");
        assert_eq!(
            above.parse::<PublisherVersion>(),
            Err(PublisherVersionError::TooLong {
                len: PublisherVersion::MAX_LEN + 1,
                max: PublisherVersion::MAX_LEN,
            })
        );
    }

    #[test]
    fn try_from_enforces_the_same_rules_as_parse() {
        assert_eq!(PublisherVersion::try_from("1.0.0"), Ok(v("1.0.0")));
        assert!(
            PublisherVersion::try_from("x".repeat(PublisherVersion::MAX_LEN + 1).as_str()).is_err()
        );
    }

    proptest::proptest! {
        /// Eq iff Ord is Equal; equal precedence hashes equally, regardless of build metadata.
        #[test]
        fn eq_ord_and_hash_agree(
            a in version_text(),
            b in version_text(),
        ) {
            use std::hash::BuildHasher as _;

            let (a, b) = (v(&a), v(&b));
            let hasher = std::collections::hash_map::RandomState::new();
            proptest::prop_assert_eq!(a == b, a.cmp(&b) == std::cmp::Ordering::Equal);
            if a == b {
                proptest::prop_assert_eq!(hasher.hash_one(&a), hasher.hash_one(&b));
            }
        }
    }

    /// Small `SemVer` space with prereleases/build metadata makes equal-precedence pairs common.
    fn version_text() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        (
            0_u64..3,
            0_u64..3,
            0_u64..3,
            proptest::option::of(prop_oneof!["alpha", "rc\\.1", "rc\\.2", "1"]),
            proptest::option::of(prop_oneof!["a", "b", "sha\\.1"]),
        )
            .prop_map(|(major, minor, patch, pre, build)| {
                let mut text = format!("{major}.{minor}.{patch}");
                if let Some(pre) = pre {
                    text.push('-');
                    text.push_str(&pre);
                }
                if let Some(build) = build {
                    text.push('+');
                    text.push_str(&build);
                }
                text
            })
    }
}

fn pattern(s: &str) -> gts::GtsIdPattern {
    gts::GtsIdPattern::try_new(s).expect("valid GTS pattern")
}

/// Every filter, projection and page field set away from its default.
fn populated_query() -> ListEntitiesRequest {
    ListEntitiesRequest {
        filter: EntityFilter {
            pattern: Some(pattern("gts.cf.example.*")),
            max_chain_depth: NonZeroU8::new(2),
            kind: Some(EntityKind::TypeSchema),
            lifecycle: LifecycleFilter::All,
        },
        projection: Projection::Select(FieldSelection::full()),
        page: PageRequest {
            limit: Some(7),
            cursor: Some(Cursor::from_token("c1")),
        },
    }
}

#[test]
fn each_setter_builds_its_own_field_of_the_literal() {
    let built = ListEntitiesRequest::new()
        .with_cursor(Cursor::from_token("c1"))
        .with_lifecycle(LifecycleFilter::All)
        .with_projection(FieldSelection::full())
        .with_kind(EntityKind::TypeSchema)
        .with_limit(7)
        .with_max_chain_depth(NonZeroU8::new(2).expect("non-zero"))
        .with_pattern(pattern("gts.cf.example.*"));
    assert_eq!(built, populated_query());
    assert_eq!(
        ListEntitiesRequest::matching(pattern("gts.cf.example.*")),
        ListEntitiesRequest {
            filter: EntityFilter {
                pattern: Some(pattern("gts.cf.example.*")),
                ..EntityFilter::default()
            },
            ..ListEntitiesRequest::default()
        },
        "matching narrows the pattern only: any kind, active, default projection, first page"
    );
}

#[test]
fn narrowing_a_built_query_again_keeps_its_other_settings() {
    let mut expected = populated_query();
    expected.filter.pattern = Some(pattern("gts.cf.other.*"));
    expected.page.cursor = Some(Cursor::from_token("c2"));
    assert_eq!(
        populated_query()
            .with_pattern(pattern("gts.cf.other.*"))
            .with_cursor(Cursor::from_token("c2")),
        expected,
        "kind, depth, lifecycle, projection and limit survive"
    );

    let mut expected = populated_query();
    expected.page.limit = Some(0);
    assert_eq!(
        populated_query().with_limit(0),
        expected,
        "the limit is passed through unclamped and the cursor kept"
    );
}

#[test]
fn an_explicit_selection_stays_explicit_through_the_builders() {
    let light = Projection::Select(FieldSelection::light());
    assert_eq!(
        ListEntitiesRequest::new()
            .with_projection(FieldSelection::light())
            .projection,
        light
    );
    assert_eq!(
        ReadOptions::new()
            .with_projection(FieldSelection::with(&EntityField::DEFAULT))
            .projection,
        Projection::Select(FieldSelection::with(&EntityField::DEFAULT)),
        "a selection equal to the default fields is not `Default`"
    );
    assert_eq!(ListEntitiesRequest::new().projection, Projection::Default);
}

#[test]
fn read_options_build_the_cache_policy_and_projection_independently() {
    assert_eq!(ReadOptions::new(), ReadOptions::default());
    assert_eq!(
        ReadOptions::new()
            .with_fresh(true)
            .with_projection(FieldSelection::full()),
        ReadOptions {
            projection: Projection::Select(FieldSelection::full()),
            fresh: true,
        }
    );
}

#[test]
fn a_batch_read_keeps_its_keys_in_order_and_conditions_only_the_asked_key() {
    let first = EntityKey::from(id("gts.cf.example.batch.thing.v1~"));
    let second = EntityKey::from(Uuid::nil());
    let validator = Validator::from_bytes(b"etag".to_vec());
    let request = BatchGetEntitiesRequest::new([
        BatchGetItem::new(first.clone()),
        BatchGetItem::new(second.clone()).with_if_none_match(validator.clone()),
    ]);
    assert_eq!(
        request,
        BatchGetEntitiesRequest {
            items: vec![
                BatchGetItem {
                    key: first.clone(),
                    if_none_match: None,
                },
                BatchGetItem {
                    key: second,
                    if_none_match: Some(validator),
                },
            ],
            projection: Projection::Default,
            fresh: false,
        }
    );
    let keyed = BatchGetEntitiesRequest::new([first.clone()].map(BatchGetItem::new))
        .with_fresh(true)
        .with_projection(FieldSelection::light());
    assert_eq!(keyed.items, vec![BatchGetItem::from(first)]);
    assert!(keyed.fresh);
    assert_eq!(
        keyed.projection,
        Projection::Select(FieldSelection::light())
    );
}

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "malformed test fixtures must fail the test immediately"
)]

use std::num::NonZeroU8;

use gts::{GtsId, GtsIdPattern};
use serde_json::{Value, json};
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use uuid::Uuid;

use super::{
    AcceptedWire, BatchGetBody, DeleteBody, LookupsWire, OperationWire, PageWire, SubmitBody,
    discovery_query, parse, validator,
};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, Cursor, DeleteEntitiesRequest, DeleteItem,
    DeletionOutcome, EntityField, EntityFilter, EntityKey, EntityKind, EntityLookup,
    FieldSelection, LifecycleFilter, LifecycleStatus, ListEntitiesRequest, Operation,
    OperationStatus, Origin, PageRequest, Projection, PublisherContext, RegisterEntitiesRequest,
    RegisterItem, RegistrationOutcome, Validator,
};

const TYPE: &str = "gts.cf.test.pkg.thing.v1~";
const INSTANCE: &str = "gts.cf.test.pkg.thing.v1~cf.test.pkg.one.v1";
const OPERATION: &str = "0190f1a2-0000-7000-8000-000000000001";
const UUID: &str = "a1a2a3a4-b1b2-c1c2-d1d2-d3d4d5d6d7d8";
const AT: &str = "2026-10-01T12:00:00Z";
const LATER: &str = "2026-10-02T12:00:00Z";

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid GTS identifier")
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "dto-test".to_owned(),
        version: "1.0.0".parse().expect("version"),
    }
}

fn decode_operation(body: &Value) -> Result<Operation, CanonicalError> {
    parse::<OperationWire>("operation", body.to_string().as_bytes())?.decode()
}

fn is_internal(result: &Result<impl std::fmt::Debug, CanonicalError>) -> bool {
    matches!(result, Err(CanonicalError::Internal { .. }))
}

fn operation(kind: &str, dry_run: bool, items: &Value) -> Value {
    operation_in("completed", kind, dry_run, items)
}

/// An operation at `status`, with the timestamps the registry gives that status.
fn operation_in(status: &str, kind: &str, dry_run: bool, items: &Value) -> Value {
    json!({
        "operation_id": OPERATION,
        "kind": kind,
        "dry_run": dry_run,
        "status": status,
        "created_at": AT,
        "started_at": if status == "pending" { Value::Null } else { json!(AT) },
        "completed_at": if status == "completed" { json!(AT) } else { Value::Null },
        "items": items,
    })
}

// ---- validators -----------------------------------------------------------------

#[test]
fn a_validator_is_the_same_token_from_either_transport() {
    let received = validator("\"v1:abc\"").expect("a strong entity-tag");
    assert_eq!(received, Validator::from_bytes("v1:abc"));

    let body = BatchGetBody::encode(&BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: EntityKey::from(id(TYPE)),
            if_none_match: Some(received),
        }],
        projection: Projection::Default,
        fresh: false,
    })
    .expect("encodable");
    assert_eq!(
        serde_json::to_value(body).unwrap()["items"][0]["if_none_match"],
        json!("\"v1:abc\""),
        "sent back with its quotes"
    );
}

#[test]
fn an_entity_tag_the_registry_never_issues_is_refused() {
    for tag in ["v1", "\"v1", "W/\"v1\"", "\"\"", "\"a b\"", "\"a\"b\""] {
        assert!(is_internal(&validator(tag)), "{tag:?} must be refused");
    }
}

#[test]
fn a_validator_no_entity_tag_can_carry_is_sent_as_no_condition() {
    for token in [&b""[..], b"a b", b"caf\xc3\xa9", b"q\""] {
        let body = BatchGetBody::encode(&BatchGetEntitiesRequest {
            items: vec![BatchGetItem {
                key: EntityKey::from(id(TYPE)),
                if_none_match: Some(Validator::from_bytes(token)),
            }],
            projection: Projection::Default,
            fresh: false,
        })
        .expect("encodable");
        let item = &serde_json::to_value(body).unwrap()["items"][0];
        assert!(item.get("if_none_match").is_none(), "{token:?}: {item}");
    }
}

#[test]
fn an_oversized_validator_is_refused_as_the_local_client_refuses_it() {
    for token in [vec![b'a'; 1025], vec![0xff; 1025]] {
        let encoded = BatchGetBody::encode(&BatchGetEntitiesRequest {
            items: vec![BatchGetItem {
                key: EntityKey::from(id(TYPE)),
                if_none_match: Some(Validator::from_bytes(token)),
            }],
            projection: Projection::Default,
            fresh: false,
        });
        let Err(CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        }) = encoded
        else {
            panic!("refused, not sent: {encoded:?}");
        };
        assert_eq!(field_violations[0].field, "if_none_match");
    }

    let at_bound = BatchGetBody::encode(&BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: EntityKey::from(id(TYPE)),
            if_none_match: Some(Validator::from_bytes(vec![b'a'; 1024])),
        }],
        projection: Projection::Default,
        fresh: false,
    });
    assert!(at_bound.is_ok());
}

// ---- requests ----------------------------------------------------------------------

#[test]
fn a_submission_carries_only_what_the_handler_reads() {
    let body = SubmitBody::encode(RegisterEntitiesRequest {
        items: vec![
            RegisterItem {
                gts_id: id(TYPE),
                content: json!({"type": "object"}),
                expected_resource_version: None,
                skip_compatibility_check: false,
            },
            RegisterItem {
                gts_id: id(INSTANCE),
                content: Value::Null,
                expected_resource_version: Some(3),
                skip_compatibility_check: true,
            },
        ],
        dry_run: true,
        publisher: publisher(),
    })
    .expect("encodable");
    assert_eq!(
        serde_json::to_value(body).unwrap(),
        json!({
            "items": [
                {"gts_id": TYPE, "content": {"type": "object"}},
                {
                    "gts_id": INSTANCE,
                    "content": null,
                    "expected_resource_version": 3,
                    "skip_compatibility_check": true,
                },
            ],
            "dry_run": true,
        })
    );
}

#[test]
fn a_version_the_wire_cannot_carry_is_refused_before_sending() {
    let submit = SubmitBody::encode(RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: id(TYPE),
            content: json!({}),
            expected_resource_version: Some(u64::MAX),
            skip_compatibility_check: false,
        }],
        dry_run: false,
        publisher: publisher(),
    });
    assert!(matches!(
        submit,
        Err(CanonicalError::InvalidArgument { .. })
    ));

    let delete = DeleteBody::encode(DeleteEntitiesRequest {
        items: vec![DeleteItem {
            key: EntityKey::from(id(TYPE)),
            expected_resource_version: u64::MAX,
        }],
        dry_run: false,
        publisher: publisher(),
    });
    assert!(matches!(
        delete,
        Err(CanonicalError::InvalidArgument { .. })
    ));
}

#[test]
fn a_deletion_names_each_key_canonically() {
    let uuid = Uuid::parse_str(&UUID.to_uppercase()).unwrap();
    let body = DeleteBody::encode(DeleteEntitiesRequest {
        items: vec![
            DeleteItem {
                key: EntityKey::from(id(TYPE)),
                expected_resource_version: 2,
            },
            DeleteItem {
                key: EntityKey::GtsUuid(uuid),
                expected_resource_version: 5,
            },
        ],
        dry_run: false,
        publisher: publisher(),
    })
    .expect("encodable");
    assert_eq!(
        serde_json::to_value(body).unwrap(),
        json!({
            "items": [
                {"entity_key": TYPE, "expected_resource_version": 2},
                {"entity_key": UUID, "expected_resource_version": 5},
            ],
            "dry_run": false,
        })
    );
}

#[test]
fn a_batch_read_sends_the_selection_and_not_the_cache_flag() {
    let body = BatchGetBody::encode(&BatchGetEntitiesRequest {
        items: vec![BatchGetItem::from(EntityKey::from(id(TYPE)))],
        projection: Projection::Select(FieldSelection::with(&[EntityField::Content])),
        fresh: true,
    })
    .expect("encodable");
    let body = serde_json::to_value(body).unwrap();
    assert_eq!(body["items"], json!([{"entity_key": TYPE}]));
    assert_eq!(
        body["$select"],
        json!("gts_id,gts_uuid,kind,lifecycle_status,content")
    );
    assert!(body.get("fresh").is_none());

    let default = BatchGetBody::encode(&BatchGetEntitiesRequest {
        items: vec![],
        projection: Projection::Default,
        fresh: false,
    })
    .expect("encodable");
    assert!(
        serde_json::to_value(default)
            .unwrap()
            .get("$select")
            .is_none()
    );
}

#[test]
fn discovery_sends_every_filter_and_always_the_lifecycle() {
    let bare = discovery_query(&ListEntitiesRequest::default());
    assert_eq!(bare, vec![("lifecycle_status", "active".to_owned())]);

    let full = discovery_query(&ListEntitiesRequest {
        filter: EntityFilter {
            pattern: Some(GtsIdPattern::try_new("gts.cf.test.pkg.*").expect("pattern")),
            max_chain_depth: NonZeroU8::new(2),
            kind: Some(EntityKind::TypeSchema),
            lifecycle: LifecycleFilter::All,
        },
        projection: Projection::Select(FieldSelection::with(&[EntityField::EffectiveTraitsSchema])),
        page: PageRequest {
            limit: Some(10),
            cursor: Some(Cursor::from_token("c1")),
        },
    });
    let names: Vec<_> = full.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "pattern",
            "depth",
            "kind",
            "lifecycle_status",
            "limit",
            "cursor",
            "$select"
        ]
    );
    assert_eq!(full[0].1, "gts.cf.test.pkg.*");
    assert_eq!(full[1].1, "2");
    assert_eq!(full[2].1, "type_schema");
    assert_eq!(full[3].1, "all");
    assert_eq!(full[5].1, "c1");
    assert_eq!(
        full[6].1,
        "gts_id,gts_uuid,kind,lifecycle_status,effective_traits_schema"
    );
}

// ---- operations ----------------------------------------------------------------------

#[test]
fn a_receipt_names_the_operation_and_whether_it_was_replayed() {
    let receipt: AcceptedWire = parse(
        "receipt",
        json!({"operation_id": OPERATION, "status": "running", "replayed": true})
            .to_string()
            .as_bytes(),
    )
    .expect("decodes");
    assert_eq!(receipt.operation_id, Uuid::parse_str(OPERATION).unwrap());
    assert_eq!(receipt.status(), OperationStatus::Running);
    assert!(receipt.replayed);

    for body in [
        json!({"operation_id": OPERATION, "status": "running"}),
        json!({"operation_id": OPERATION, "status": "queued", "replayed": false}),
    ] {
        let receipt = parse::<AcceptedWire>("receipt", body.to_string().as_bytes());
        assert!(is_internal(&receipt), "{body} must be refused");
    }
}

#[test]
fn a_registration_operation_decodes_every_item_state() {
    let body = operation_in(
        "running",
        "registration",
        false,
        &json!([
            {"gts_id": TYPE, "status": "succeeded", "resource_version": 4, "error": null},
            {"gts_id": INSTANCE, "status": "unchanged", "resource_version": 2, "error": null},
            {
                "gts_id": "gts.cf.test.pkg.other.v1~",
                "status": "pending",
                "resource_version": null,
                "error": null,
            },
            {
                "gts_id": "gts.cf.test.pkg.more.v1~",
                "status": "running",
                "resource_version": null,
                "error": null,
            },
        ]),
    );
    let Operation::Registration(op) = decode_operation(&body).expect("decodes") else {
        panic!("a registration");
    };
    assert_eq!(op.operation_id, Uuid::parse_str(OPERATION).unwrap());
    assert_eq!(op.status, OperationStatus::Running);
    assert_eq!(op.items[0].gts_id, id(TYPE));
    assert!(matches!(
        op.items[0].outcome,
        RegistrationOutcome::Succeeded {
            resource_version: 4
        }
    ));
    assert!(matches!(
        op.items[1].outcome,
        RegistrationOutcome::Unchanged {
            resource_version: 2
        }
    ));
    assert!(matches!(op.items[2].outcome, RegistrationOutcome::Pending));
    assert!(matches!(op.items[3].outcome, RegistrationOutcome::Running));
}

#[test]
fn an_operation_reports_its_progress() {
    for (status, item_status, expected) in [
        ("pending", "pending", OperationStatus::Pending),
        ("running", "running", OperationStatus::Running),
        ("completed", "failed", OperationStatus::Completed),
    ] {
        let error = (item_status == "failed")
            .then(|| json!({"reason": "already_exists", "message": "m", "context": {}}));
        let body = operation_in(
            status,
            "deletion",
            false,
            &json!([{
                "entity_key": TYPE,
                "status": item_status,
                "resource_version": null,
                "error": error,
            }]),
        );
        let op = decode_operation(&body).expect("decodes");
        assert_eq!(op.status(), expected, "{status}");
        let Operation::Deletion(op) = op else {
            panic!("a deletion");
        };
        let outcome = &op.items[0].outcome;
        match item_status {
            "pending" => assert!(matches!(outcome, DeletionOutcome::Pending)),
            "running" => assert!(matches!(outcome, DeletionOutcome::Running)),
            _ => {
                let DeletionOutcome::Failed { error } = outcome else {
                    panic!("a failure: {outcome:?}");
                };
                assert_eq!(error.resource_name(), Some(TYPE));
            }
        }
    }
}

#[test]
fn a_committed_success_carries_its_version_and_a_dry_run_none() {
    let deleted = operation(
        "deletion",
        false,
        &json!([{"entity_key": UUID, "status": "succeeded", "resource_version": 5, "error": null}]),
    );
    let Operation::Deletion(op) = decode_operation(&deleted).expect("decodes") else {
        panic!("a deletion");
    };
    assert!(matches!(
        op.items[0].outcome,
        DeletionOutcome::Succeeded {
            resource_version: 5
        }
    ));

    let would_register = operation(
        "registration",
        true,
        &json!([{"gts_id": TYPE, "status": "succeeded", "resource_version": null, "error": null}]),
    );
    let Operation::Registration(op) = decode_operation(&would_register).expect("decodes") else {
        panic!("a registration");
    };
    assert!(matches!(
        op.items[0].outcome,
        RegistrationOutcome::WouldSucceed
    ));
}

#[test]
fn a_dry_run_succeeds_without_a_version() {
    let body = operation(
        "deletion",
        true,
        &json!([{"entity_key": UUID, "status": "succeeded", "resource_version": null, "error": null}]),
    );
    let Operation::Deletion(op) = decode_operation(&body).expect("decodes") else {
        panic!("a deletion");
    };
    assert_eq!(
        op.items[0].entity_key,
        EntityKey::GtsUuid(Uuid::parse_str(UUID).unwrap())
    );
    assert!(matches!(op.items[0].outcome, DeletionOutcome::WouldSucceed));
}

#[test]
fn an_unknown_failure_reason_and_its_context_survive() {
    let body = operation(
        "registration",
        false,
        &json!([{
            "gts_id": TYPE,
            "status": "failed",
            "resource_version": null,
            "error": {
                "reason": "future_reason",
                "message": "from a newer registry",
                "context": {"future_field": "value", "count": 3},
            },
        }]),
    );
    let Operation::Registration(op) = decode_operation(&body).expect("decodes") else {
        panic!("a registration");
    };
    let RegistrationOutcome::Failed { error } = &op.items[0].outcome else {
        panic!("a failure: {:?}", op.items[0].outcome);
    };
    assert_eq!(
        AdmissionFailure::from_canonical(error),
        Some(
            AdmissionFailure::new(
                AdmissionFailureReason::from_wire("future_reason"),
                "from a newer registry",
            )
            .with_context("future_field", "value")
            .with_context("count", "3")
        ),
        "the local client's failure for the same record"
    );
    assert_eq!(error.resource_name(), Some(TYPE), "names the item");
}

#[test]
fn a_malformed_operation_is_refused_not_defaulted() {
    // The server always sends both members, `null` when they do not apply.
    let registration = |mut item: Value| {
        let item_ref = item.as_object_mut().unwrap();
        for member in ["resource_version", "error"] {
            item_ref.entry(member).or_insert(Value::Null);
        }
        operation("registration", false, &json!([item]))
    };
    let refused = [
        // A member the registry always sends, omitted.
        operation(
            "registration",
            true,
            &json!([{"gts_id": TYPE, "status": "succeeded", "error": null}]),
        ),
        operation(
            "registration",
            true,
            &json!([{"gts_id": TYPE, "status": "succeeded", "resource_version": null}]),
        ),
        // States with data they never carry, or without data they always carry.
        registration(json!({"gts_id": TYPE, "status": "succeeded"})),
        registration(json!({"gts_id": TYPE, "status": "succeeded", "resource_version": 0})),
        registration(json!({"gts_id": TYPE, "status": "unchanged"})),
        registration(json!({"gts_id": TYPE, "status": "pending", "resource_version": 1})),
        registration(json!({"gts_id": TYPE, "status": "failed"})),
        registration(json!({"gts_id": TYPE, "status": "succeeded", "resource_version": -1})),
        // A dry run allocates no version.
        operation(
            "registration",
            true,
            &json!([{"gts_id": TYPE, "status": "succeeded", "resource_version": 1, "error": null}]),
        ),
        // A deletion is never unchanged.
        operation(
            "deletion",
            false,
            &json!([{"entity_key": TYPE, "status": "unchanged", "resource_version": 1, "error": null}]),
        ),
        // Unknown vocabulary, missing members, bad keys.
        registration(json!({"gts_id": TYPE, "status": "skipped"})),
        registration(json!({"gts_id": "not-an-id", "status": "pending"})),
        operation("purge", false, &json!([])),
        json!({"operation_id": OPERATION, "kind": "registration", "dry_run": false, "status": "completed"}),
        registration(json!({
            "gts_id": TYPE,
            "status": "failed",
            "error": {"reason": "x", "message": "m"},
        })),
    ];
    for body in refused {
        assert!(
            is_internal(&decode_operation(&body)),
            "{body} must be refused"
        );
    }
}

// ---- entities, lookups, pages ---------------------------------------------------------

fn entity(extra: &Value) -> Value {
    let mut entity = json!({
        "gts_id": TYPE,
        "gts_uuid": UUID,
        "kind": "type_schema",
        "lifecycle_status": "active",
    });
    entity
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    entity
}

fn decode_page(body: &Value) -> Result<crate::models::ListEntitiesResponse, CanonicalError> {
    parse::<PageWire>("page", body.to_string().as_bytes())?.decode()
}

#[test]
fn a_projected_snapshot_keeps_omission_and_null_apart() {
    let page = decode_page(&json!({
        "items": [entity(&json!({
            "origin": {
                "type": "managed",
                "resource_version": 7,
                "created_at": AT,
                "updated_at": LATER,
            },
            "content": null,
            "resolved_schema": {"type": "object"},
            "effective_traits": {"flag": true},
            "provenance": {"compatibility_check_skipped": true},
        })), entity(&json!({
            "lifecycle_status": "deleted",
            "content": {"a": 1},
            "effective_traits": null,
            "effective_traits_schema": {"type": "object", "title": "traits"},
            "provenance": {"compatibility_check_skipped": false},
        }))],
        "page_info": {"next_cursor": "next-1", "limit": 50},
    }))
    .expect("decodes");

    let entity = &page.items[0];
    assert_eq!(entity.gts_id, id(TYPE));
    assert_eq!(entity.gts_uuid, Uuid::parse_str(UUID).unwrap());
    assert_eq!(entity.kind, EntityKind::TypeSchema);
    assert_eq!(entity.lifecycle_status, LifecycleStatus::Active);
    let rfc3339 = &time::format_description::well_known::Rfc3339;
    assert_eq!(
        entity.origin,
        Some(Origin::Managed {
            resource_version: 7,
            created_at: time::OffsetDateTime::parse(AT, rfc3339).unwrap(),
            updated_at: time::OffsetDateTime::parse(LATER, rfc3339).unwrap(),
        })
    );
    assert_eq!(entity.content, Some(Value::Null), "selected and null");
    assert_eq!(entity.resolved_schema, Some(json!({"type": "object"})));
    assert_eq!(entity.effective_traits, Some(json!({"flag": true})));
    assert_eq!(entity.effective_traits_schema, None, "not selected");
    assert!(entity.provenance.unwrap().compatibility_check_skipped);

    let other = &page.items[1];
    assert_eq!(other.lifecycle_status, LifecycleStatus::Deleted);
    assert_eq!(other.origin, None, "not selected");
    assert_eq!(other.content, Some(json!({"a": 1})));
    assert_eq!(other.resolved_schema, None, "not selected");
    assert_eq!(
        other.effective_traits,
        Some(Value::Null),
        "selected and null"
    );
    assert_eq!(
        other.effective_traits_schema,
        Some(json!({"type": "object", "title": "traits"}))
    );
    assert!(!other.provenance.unwrap().compatibility_check_skipped);
    assert_eq!(page.next, Some(Cursor::from_token("next-1")));
}

#[test]
fn the_last_page_has_no_cursor_and_an_unknown_origin_is_refused() {
    let last = decode_page(&json!({"items": [], "page_info": {"limit": 50}})).expect("decodes");
    assert_eq!(last.next, None);

    let external = json!({
        "items": [entity(&json!({"origin": {"type": "external"}}))],
        "page_info": {"limit": 50},
    });
    assert!(is_internal(&decode_page(&external)));
}

fn decode_lookups(
    items: &Value,
) -> Result<crate::models::BatchGetEntitiesResponse, CanonicalError> {
    parse::<LookupsWire>("lookups", json!({"items": items}).to_string().as_bytes())?.decode()
}

#[test]
fn each_lookup_decodes_with_its_validator() {
    let lookups = decode_lookups(&json!([
        {"entity_key": TYPE, "status": "found", "etag": "\"t1\"", "entity": entity(&json!({}))},
        {"entity_key": UUID, "status": "unchanged", "etag": "\"t2\""},
        {"entity_key": INSTANCE, "status": "not_found"},
    ]))
    .expect("decodes")
    .0;

    let Some(EntityLookup::Found { entity, etag }) = lookups.get(&EntityKey::from(id(TYPE))) else {
        panic!("found");
    };
    assert_eq!(entity.gts_id, id(TYPE));
    assert_eq!(etag, &Validator::from_bytes("t1"));
    assert_eq!(
        lookups.get(&EntityKey::GtsUuid(Uuid::parse_str(UUID).unwrap())),
        Some(&EntityLookup::Unchanged {
            etag: Validator::from_bytes("t2")
        })
    );
    assert_eq!(
        lookups.get(&EntityKey::from(id(INSTANCE))),
        Some(&EntityLookup::NotFound)
    );
}

#[test]
fn an_inconsistent_or_repeated_lookup_is_refused() {
    let refused = [
        json!([{"entity_key": TYPE, "status": "found", "etag": "\"t\""}]),
        json!([{"entity_key": TYPE, "status": "found", "entity": entity(&json!({}))}]),
        json!([{"entity_key": TYPE, "status": "unchanged"}]),
        json!([{"entity_key": TYPE, "status": "not_found", "etag": "\"t\""}]),
        json!([{"entity_key": TYPE, "status": "unchanged", "etag": "t"}]),
        json!([
            {"entity_key": TYPE, "status": "not_found"},
            {"entity_key": TYPE, "status": "not_found"},
        ]),
    ];
    for items in refused {
        assert!(
            is_internal(&decode_lookups(&items)),
            "{items} must be refused"
        );
    }
}

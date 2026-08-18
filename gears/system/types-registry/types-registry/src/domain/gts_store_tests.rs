//! In-source tests for the pure half of [`super`]: everything that needs no
//! database. The database-backed half — closure containment, the candidate
//! overlay against real rows, rebuild-sees-the-new-revision — is
//! `tests/gts_store_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;
use toolkit_gts::gts_id;

use super::{StoreBuildError, UnitDocument, build_store};

const BASE: &str = gts_id!("acme.crm.customer.type.v1~");
const DERIVED: &str = gts_id!("acme.crm.customer.type.v1~acme.crm.premium.type.v1~");
const OTHER: &str = gts_id!("acme.crm.order.type.v1~");

fn schema(id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn doc(id: &str) -> UnitDocument {
    UnitDocument {
        gts_id: id.to_owned(),
        content: schema(id),
    }
}

/// The load-order criterion. Registration order leaves no trace in the store's
/// contents — the map is unordered and `register_schema` checks nothing — so the
/// order is asserted through the builder's own report. Given deliberately
/// shuffled input the base still precedes everything derived from it, because a
/// base identifier is a byte prefix of its derived identifiers.
#[test]
fn shuffled_input_is_registered_in_gts_id_order() {
    let unit = build_store(vec![doc(DERIVED), doc(OTHER), doc(BASE)]).expect("build");

    assert_eq!(unit.load_order(), [BASE, DERIVED, OTHER]);

    let base_at = unit
        .load_order()
        .iter()
        .position(|id| id == BASE)
        .expect("base is loaded");
    let derived_at = unit
        .load_order()
        .iter()
        .position(|id| id == DERIVED)
        .expect("derived is loaded");
    assert!(
        base_at < derived_at,
        "a derived schema must never load before its base",
    );
}

/// A document with no `$schema` registers as an *Instance* inside `gts-rust` —
/// `GtsEntity::new` overwrites the `is_schema: true` that `register_schema`
/// passes with `has_schema_field()` — after which `$ref`s at it stay unresolved
/// with no error raised anywhere. Refusing it here is what turns that silent
/// corruption into a named failure.
#[test]
fn a_document_without_a_dialect_is_refused_by_name() {
    let mut without = doc(BASE);
    without
        .content
        .as_object_mut()
        .expect("object")
        .remove("$schema");

    let err = build_store(vec![without]).expect_err("a dialect-less document must be refused");
    match err {
        StoreBuildError::MissingDialect { gts_id } => assert_eq!(gts_id, BASE),
        other => panic!("expected MissingDialect, got {other}"),
    }
}

/// An empty `$schema` is the same case: `has_schema_field` requires a *non-empty*
/// string, so a document carrying `"$schema": ""` would register as an Instance
/// too. Tested separately because a presence-only check would pass it.
#[test]
fn an_empty_dialect_string_is_refused_like_an_absent_one() {
    let mut empty = doc(BASE);
    empty.content["$schema"] = json!("");

    let err = build_store(vec![empty]).expect_err("an empty dialect must be refused");
    assert!(matches!(err, StoreBuildError::MissingDialect { .. }));
}

/// `register_schema` overwrites an existing identifier without complaint, so two
/// documents under one identifier would make the store's contents depend on load
/// order. The builder refuses; merging is the loader's explicit decision.
#[test]
fn a_repeated_identifier_is_refused_rather_than_silently_overwritten() {
    let mut second = doc(BASE);
    second.content["title"] = json!("the other one");

    let err = build_store(vec![doc(BASE), second]).expect_err("a duplicate must be refused");
    match err {
        StoreBuildError::Duplicate { gts_id } => assert_eq!(gts_id, BASE),
        other => panic!("expected Duplicate, got {other}"),
    }
}

/// **Inverted at T10.** This case used to assert that an Instance identifier was
/// refused as out of scope; the store now carries both kinds, so it asserts the
/// opposite — the Instance registers, alongside the Type Schema it conforms to.
///
/// The `~`-terminated base is in the document set because an Instance is only
/// meaningful with it: `validate_instance` resolves the instance's `type_id` *out of
/// the store*. Registering the Instance alone would succeed here and fail two layers
/// later, which is the failure mode the load order exists to make impossible.
#[test]
fn an_instance_registers_beside_the_type_it_conforms_to() {
    let instance = UnitDocument {
        gts_id: format!("{BASE}acme.crm.customers.acme_corp.v1"),
        content: serde_json::json!({ "name": "ACME" }),
    };

    let unit = build_store(vec![doc(BASE), instance])
        .expect("an Instance and its type must both register");

    assert_eq!(
        unit.load_order(),
        &[
            BASE.to_owned(),
            format!("{BASE}acme.crm.customers.acme_corp.v1")
        ]
    );
}

/// A one-segment Instance identifier conforms to nothing, and **`gts-id` owns that
/// rule** — `GtsId::try_new` refuses the shape before this module can ask
/// `get_type_id()` about it. Asserted as the library's verdict rather than as one of
/// ours, exactly as `an_unparsable_identifier_carries_the_library_verdict` does:
/// re-deciding a grammar `gts-rust` already decides is the local approximation
/// `constraint-gts-implementation` forbids.
///
/// This is also why [`StoreBuildError::InstanceWithoutType`] is documented as
/// unreachable-by-construction rather than deleted — it is the branch that would
/// fire if that library rule ever relaxed.
#[test]
fn a_single_segment_instance_is_refused_by_the_library() {
    let orphan = UnitDocument {
        gts_id: "gts.acme.crm.customer.thing.v1".to_owned(),
        content: serde_json::json!({ "name": "ACME" }),
    };

    let err = build_store(vec![orphan]).expect_err("an Instance with no type must be refused");
    match err {
        StoreBuildError::Register { gts_id, source } => {
            assert_eq!(gts_id, "gts.acme.crm.customer.thing.v1");
            assert!(
                source
                    .to_string()
                    .contains("Single-segment instance IDs are prohibited"),
                "expected the library's single-segment verdict, got: {source}"
            );
        }
        other => panic!("expected Register carrying the library verdict, got {other}"),
    }
}

/// The dialect gate is asked of Type Schemas only (T10). An Instance has no
/// `$schema` by definition — that absence is what makes it an Instance — so
/// applying the gate to both kinds would refuse every Instance ever registered.
#[test]
fn the_dialect_gate_does_not_apply_to_an_instance() {
    let instance = UnitDocument {
        gts_id: format!("{BASE}acme.crm.customers.no_dialect.v1"),
        content: serde_json::json!({ "name": "no $schema here" }),
    };

    build_store(vec![doc(BASE), instance])
        .expect("an Instance without $schema is an Instance, not a dialect-less schema");
}

/// An identifier that ends with `~` but does not parse is `gts-rust`'s verdict,
/// not ours: the builder reports it with the underlying [`gts::StoreError`]
/// rather than pre-judging the grammar (`constraint-gts-implementation`).
#[test]
fn an_unparsable_identifier_carries_the_library_verdict() {
    let bad = UnitDocument {
        gts_id: "gts.too.few.tokens~".to_owned(),
        content: schema(BASE),
    };

    let err = build_store(vec![bad]).expect_err("an unparsable identifier must be refused");
    match err {
        StoreBuildError::Register { gts_id, .. } => assert_eq!(gts_id, "gts.too.few.tokens~"),
        other => panic!("expected Register, got {other}"),
    }
}

/// The store holds exactly what was handed to it, and a stranger is absent. This
/// is the pure-input form of the closure-containment criterion; its database form
/// is in `tests/gts_store_test.rs`.
#[test]
fn the_store_holds_the_documents_it_was_given_and_nothing_else() {
    let mut unit = build_store(vec![doc(BASE), doc(DERIVED)]).expect("build");
    let store = unit.store_mut();

    assert!(store.get(BASE).is_some());
    assert!(store.get(DERIVED).is_some());
    assert!(store.get(OTHER).is_none());
    assert_eq!(store.items().count(), 2);
}

/// An empty unit is a store, not an error: a candidate set can be empty in a dry
/// run over nothing, and `build_store` is the only place that would have to
/// special-case it.
#[test]
fn an_empty_document_set_yields_an_empty_store() {
    let mut unit = build_store(Vec::new()).expect("build");
    assert!(unit.load_order().is_empty());
    assert!(unit.missing_candidates().is_empty());
    assert_eq!(unit.store_mut().items().count(), 0);
}

/// `register_schema` is what makes a registered document a *schema*, and only
/// then do references to it resolve. Pinned because the whole dialect guard above
/// exists to protect this property.
#[test]
fn a_registered_document_is_visible_as_a_schema() {
    let mut unit = build_store(vec![doc(BASE)]).expect("build");
    let entity = unit.store_mut().get(BASE).expect("registered").clone();
    assert!(entity.is_schema);
}

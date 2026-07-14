//! Integration tests for the `cf-gears-toolkit-gts-macros` wrappers.
//!
//! Proc-macros can't be tested from inside the crate that defines them,
//! so the tests live in the consumer crate. The wrappers are thin
//! pass-throughs to upstream `gts-macros` plus exactly one extra
//! emission — an `inventory::submit!` block. These tests only verify
//! that the inventory submission lands; everything else (id validation,
//! prefix const-asserts, schema JSON shape, `pub static` binding
//! emission) is upstream's contract and is covered by upstream's own
//! tests.

use schemars::JsonSchema;
use toolkit_gts::{
    GtsInstanceId, GtsSchema, GtsTraitsSchema, InventoryInstance, InventoryTypeSchema, gts_id,
    gts_instance, gts_instance_raw, gts_type_schema, gts_uri,
};

// =====================================================================
//                              Test types
// =====================================================================

/// Base test schema. The wrapper's only job here is to:
/// 1. Forward the attribute to `gts_macros::struct_to_gts_schema`
///    (so `TestThingV1::SCHEMA_ID` and the schema accessor exist).
/// 2. Submit an `InventoryTypeSchema` entry with the same `type_id`.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("test.cf.toolkit_gts.thing.v1~"),
    description = "Test base type for toolkit-gts wrapper integration tests",
    properties = "id,name",
    base = true
)]
pub struct TestThingV1 {
    pub id: GtsInstanceId,
    pub name: String,
}

// `gts_instance_raw!` — submits one inventory entry; the value itself
// is built lazily by the closure inside `payload_fn`.
gts_instance_raw!({
    "id": gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.raw.v1"),
    "name": "raw",
});

// `gts_instance!` (typed) — same: one inventory entry, value built lazily.
gts_instance! {
    TestThingV1 {
        id: gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.typed.v1"),
        name: "typed".to_owned(),
    }
}

// `gts_instance!` with `#[gts_static(NAME)]` — additionally emits a
// `pub static NAME: LazyLock<TestThingV1>` via upstream alongside the
// inventory submission. The wrapper's own job is just the inventory
// part; the static binding is upstream's emission.
gts_instance! {
    #[gts_static(NAMED_INSTANCE)]
    TestThingV1 {
        id: gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.named.v1"),
        name: "named".to_owned(),
    }
}

// ---------------------------------------------------------------------
// gts-rust 0.10.0 traits & modifiers
//
// The wrapper forwards the attribute token stream verbatim, so the new
// upstream params (`traits_schema`, `traits`, `gts_abstract`,
// `gts_final`) and the `#[derive(GtsTraitsSchema)]` on the inline-traits
// carrier flow straight through. These declarations are the regression
// guard: if a future wrapper change started filtering attributes (rather
// than passing them through), this test target would fail to compile.
// ---------------------------------------------------------------------

#[derive(JsonSchema, serde::Serialize, GtsTraitsSchema)]
pub struct EventTraits {
    #[schemars(extend("x-gts-ref" = gts_id!("x.core.events.topic.v1~")))]
    pub topic_ref: String,
}

/// Abstract base carrying an inline traits schema — exercises the new
/// `traits_schema = inline(...)` and `gts_abstract = true` params via the
/// wrapper.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("test.cf.toolkit_gts.event.v1~"),
    description = "Abstract base event with inline traits schema",
    properties = "id,payload",
    traits_schema = inline(EventTraits),
    gts_abstract = true
)]
pub struct EventV1<P> {
    pub id: GtsInstanceId,
    pub payload: P,
}

/// Final derived type supplying concrete trait values — exercises the new
/// `traits = ...` and `gts_final = true` params via the wrapper.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("test.cf.toolkit_gts.event.v1~test.cf.toolkit_gts.order_placed.v1~"),
    description = "Final order-placed event",
    properties = "order_id",
    traits = serde_json::json!({ "topic_ref": gts_id!("x.core.events.topic.v1~test.cf._.orders.v1") }),
    gts_final = true
)]
pub struct OrderPlacedV1 {
    pub order_id: String,
}

// =====================================================================
//                                Tests
// =====================================================================

const TYPE_ID: &str = gts_id!("test.cf.toolkit_gts.thing.v1~");
const EVENT_BASE_ID: &str = gts_id!("test.cf.toolkit_gts.event.v1~");
const ORDER_PLACED_ID: &str =
    gts_id!("test.cf.toolkit_gts.event.v1~test.cf.toolkit_gts.order_placed.v1~");
const RAW_ID: &str = gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.raw.v1");
const TYPED_ID: &str = gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.typed.v1");
const NAMED_ID: &str = gts_id!("test.cf.toolkit_gts.thing.v1~test.cf.toolkit_gts.named.v1");
const TYPE_URI: &str = gts_uri!("test.cf.toolkit_gts.thing.v1~");

fn schema_ids() -> Vec<&'static str> {
    inventory::iter::<InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        .collect()
}

fn instance_ids() -> Vec<&'static str> {
    inventory::iter::<InventoryInstance>
        .into_iter()
        .map(|e| e.instance_id)
        .collect()
}

fn find_instance(id: &str) -> &'static InventoryInstance {
    inventory::iter::<InventoryInstance>
        .into_iter()
        .find(|e| e.instance_id == id)
        .unwrap_or_else(|| panic!("instance {id} not in inventory; got: {:?}", instance_ids()))
}

#[test]
fn gts_uri_macro_prepends_uri_and_configured_id_prefix() {
    assert_eq!(TYPE_URI, gts_uri!("test.cf.toolkit_gts.thing.v1~"));
    assert!(TYPE_URI.ends_with(TYPE_ID));
}

#[test]
#[allow(unknown_lints, de0904_no_hardcoded_gts_prefix)]
fn gts_id_macro_detects_already_prefixed_literal() {
    // gts_id! on a literal that already starts with GTS_ID_PREFIX should
    // emit it as-is, without doubling the prefix.
    let already_prefixed: &str = gts_id!("gts.test.cf.toolkit_gts.thing.v1~");
    assert_eq!(already_prefixed, TYPE_ID);
}

#[test]
fn gts_uri_macro_detects_already_prefixed_literal() {
    // A literal that already includes GTS_ID_PREFIX should not get it doubled.
    let already_prefixed: &str = concat!(gts_id!("test.cf.toolkit_gts.thing.v1~"));
    let uri_from_prefixed: String = gts_uri!(already_prefixed);
    let uri_from_suffix: &str = gts_uri!("test.cf.toolkit_gts.thing.v1~");
    assert_eq!(uri_from_prefixed.as_str(), uri_from_suffix);
}

#[test]
fn gts_uri_macro_detects_already_uri_prefixed_literal() {
    // A literal that already starts with GTS_ID_URI_PREFIX should be
    // emitted as-is.
    let already_uri: &str = gts_uri!("gts://gts.test.cf.toolkit_gts.thing.v1~");
    assert_eq!(already_uri, TYPE_URI);
}

#[test]
fn gts_uri_macro_runtime_expr_with_uri_prefix() {
    // Runtime expression that already starts with GTS_ID_URI_PREFIX —
    // returned as-is.
    let uri_in: String = TYPE_URI.to_owned();
    let uri: String = gts_uri!(uri_in);
    assert_eq!(uri.as_str(), TYPE_URI);
}

#[test]
fn gts_uri_macro_runtime_expr_with_prefix() {
    // Runtime expression that already starts with GTS_ID_PREFIX — only URI
    // prefix should be prepended.
    let id: String = TYPE_ID.to_owned();
    let uri: String = gts_uri!(id);
    assert_eq!(uri.as_str(), TYPE_URI);
}

#[test]
fn gts_uri_macro_runtime_expr_without_prefix() {
    // Runtime expression without GTS_ID_PREFIX — both URI and ID prefix
    // should be prepended.
    let suffix: String = "test.cf.toolkit_gts.thing.v1~".to_owned();
    let uri: String = gts_uri!(suffix);
    assert_eq!(uri.as_str(), TYPE_URI);
}

#[test]
fn gts_type_schema_wrapper_registers_inventory_schema() {
    // Wrapper contract: `#[gts_type_schema(...)]` adds an `InventoryTypeSchema`
    // entry whose `type_id` matches the attribute literal. Upstream gives
    // us `TestThingV1::TYPE_ID` for free — we just check the wrapper's
    // contribution lined up with it.
    let ids = schema_ids();
    assert!(
        ids.contains(&TYPE_ID),
        "TestThingV1's schema not registered; got: {ids:?}"
    );
    assert_eq!(
        TestThingV1::TYPE_ID,
        TYPE_ID,
        "wrapper's type_id literal must match upstream's TYPE_ID const",
    );
}

#[test]
fn gts_type_schema_wrapper_schema_fn_returns_well_formed_json() {
    // The wrapper plugs `gts_schema_with_refs_as_string` (upstream's
    // generated accessor) into `InventoryTypeSchema::schema_fn`. The string
    // must parse as JSON.
    let entry = inventory::iter::<InventoryTypeSchema>
        .into_iter()
        .find(|e| e.type_id == TYPE_ID)
        .expect("test schema present");
    let s = (entry.schema_fn)();
    let v: serde_json::Value =
        serde_json::from_str(&s).expect("schema_fn output must parse as JSON");
    assert!(v.is_object(), "schema must be a JSON object");
}

#[test]
fn gts_instance_raw_wrapper_registers_inventory_instance() {
    let entry = find_instance(RAW_ID);
    assert_eq!(
        entry.type_id, TYPE_ID,
        "raw wrapper must derive type_id from the instance_id prefix"
    );
    let payload = (entry.payload_fn)();
    assert_eq!(
        payload["id"], RAW_ID,
        "upstream auto-injects `id` into the JSON payload"
    );
    assert_eq!(payload["name"], "raw");
}

#[test]
fn gts_instance_typed_wrapper_registers_inventory_instance() {
    let entry = find_instance(TYPED_ID);
    assert_eq!(entry.type_id, TYPE_ID);
    let payload = (entry.payload_fn)();
    assert_eq!(payload["id"], TYPED_ID);
    assert_eq!(payload["name"], "typed");
}

#[test]
fn gts_instance_with_gts_static_emits_both_inventory_and_typed_static() {
    // The wrapper's own contribution is the inventory entry; the static
    // binding `NAMED_INSTANCE` is upstream's emission. Verify both showed
    // up so the wrapper isn't accidentally suppressing one.
    let entry = find_instance(NAMED_ID);
    assert_eq!(entry.type_id, TYPE_ID);
    let payload = (entry.payload_fn)();
    assert_eq!(payload["id"], NAMED_ID);

    // Typed runtime accessor: the macro-emitted static carries the same id.
    let inst: &TestThingV1 = &NAMED_INSTANCE;
    assert_eq!(inst.id.as_ref(), NAMED_ID);
    assert_eq!(inst.name, "named");
}

#[test]
fn gts_type_schema_wrapper_forwards_traits_and_modifiers() {
    // Regression guard for gts-rust 0.10.0: the wrapper must pass the new
    // `traits_schema` / `traits` / `gts_abstract` / `gts_final` params
    // through to upstream unchanged. Compilation of this target already
    // proves forwarding; here we additionally check both types reached the
    // inventory and still expose a well-formed schema accessor.
    let ids = schema_ids();
    assert!(
        ids.contains(&EVENT_BASE_ID),
        "abstract base (traits_schema/gts_abstract) not registered; got: {ids:?}"
    );
    assert!(
        ids.contains(&ORDER_PLACED_ID),
        "final derived type (traits/gts_final) not registered; got: {ids:?}"
    );

    // The macro-generated accessor still backs `schema_fn` for trait-bearing
    // types, and `gts_abstract` const-asserts agree (TYPE_ID matches).
    assert_eq!(EventV1::<()>::TYPE_ID, EVENT_BASE_ID);
    assert_eq!(OrderPlacedV1::TYPE_ID, ORDER_PLACED_ID);

    let entry = inventory::iter::<InventoryTypeSchema>
        .into_iter()
        .find(|e| e.type_id == ORDER_PLACED_ID)
        .expect("final derived schema present");
    let v: serde_json::Value =
        serde_json::from_str(&(entry.schema_fn)()).expect("schema_fn output must parse as JSON");
    assert!(v.is_object(), "schema must be a JSON object");
}

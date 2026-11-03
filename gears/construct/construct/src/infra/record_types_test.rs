//! Record type checks against the SDK's record types, served by a mock types
//! registry in `ClientHub`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::{Value, json};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};
use types_registry_sdk::{
    GtsInstance, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
    TypesRegistryClient,
};
use uuid::Uuid;

use super::record_types::RegistryRecordTypes;
use crate::domain::error::DomainError;
use crate::domain::record_intake::{
    Place, RecordTypes, Refusal, RefusalReason, TypeTraits, check_type_rules_of,
};
use crate::test_support::{
    chat_record, example_records, hub_with_record_types, sdk_record_type_schemas,
};

const CHAT: &str = construct_sdk::gts::CHAT_MESSAGE_TYPE;

#[resource_error("gts.cf.core.types_registry.entity.v1~")]
struct RegistryError;

fn types() -> RegistryRecordTypes {
    RegistryRecordTypes::new(hub_with_record_types()).expect("record types")
}

/// What intake's type check gives for a record: the adapter's checks, then
/// the rules a generated schema cannot carry, which the domain runs.
async fn checked(types: &RegistryRecordTypes, record: &Value) -> Result<TypeTraits, DomainError> {
    let traits = types.check(record).await?;
    check_type_rules_of(&traits, record)?;
    Ok(traits)
}

/// The refusal intake's type check gives, failing the test on any other
/// outcome.
async fn refused_by(types: &RegistryRecordTypes, record: &Value) -> Refusal {
    match checked(types, record).await {
        Err(DomainError::Refused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

async fn refusal(record: &Value) -> Refusal {
    refused_by(&types(), record).await
}

fn chat() -> Value {
    chat_record(
        Uuid::new_v4(),
        "chat_engine/thread-42/msg-7",
        "2026-09-10T09:13:00Z",
    )
}

#[tokio::test]
async fn a_valid_record_of_every_derived_type_passes() {
    let types = types();
    for record in example_records() {
        let result = types.check(&record).await;
        assert!(result.is_ok(), "{}: {result:?}", record["type"]);
    }
}

#[tokio::test]
async fn a_broken_envelope_is_refused_before_the_type_is_looked_up() {
    let mut record = chat();
    record.as_object_mut().unwrap().remove("provenance");
    // No types registry at all: the envelope check needs none.
    let types = RegistryRecordTypes::new(Arc::new(ClientHub::new())).expect("record types");

    let refused = refused_by(&types, &record).await;

    assert_eq!(refused.reason, RefusalReason::SchemaViolation);
    assert_eq!(refused.place, Place::pointer("/provenance"));
    assert_eq!(refused.rule, "required");
    assert_eq!(
        refused.type_id, None,
        "the envelope has not accepted the type"
    );
}

#[tokio::test]
async fn a_body_that_is_not_an_object_is_refused_as_a_whole() {
    for body in [json!([]), json!("x"), json!(null), json!(7)] {
        let refused = refusal(&body).await;
        assert_eq!(refused.reason, RefusalReason::SchemaViolation, "{body}");
        assert_eq!(refused.place, Place::Whole, "{body}");
        assert_eq!(refused.rule, "type", "{body}");
    }
}

#[tokio::test]
async fn an_unexpected_member_is_named() {
    let mut record = chat();
    record["tenant_id"] = json!(Uuid::new_v4().to_string());

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/tenant_id"));
    assert_eq!(refused.rule, "additionalProperties");
}

#[tokio::test]
async fn an_unexpected_member_with_a_slash_or_tilde_is_named_as_one_member() {
    let mut record = chat();
    record["a/b~c"] = json!("x");

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/a~1b~0c"));
    assert_eq!(refused.rule, "additionalProperties");
}

#[tokio::test]
async fn a_missing_payload_member_of_the_derived_type_is_named() {
    let mut record = chat();
    record["payload"].as_object_mut().unwrap().remove("text");

    let refused = refusal(&record).await;

    assert_eq!(refused.reason, RefusalReason::SchemaViolation);
    assert_eq!(refused.place, Place::pointer("/payload/text"));
    assert_eq!(refused.rule, "required");
    assert_eq!(refused.type_id.as_deref(), Some(CHAT));
}

#[tokio::test]
async fn a_passing_record_gets_whether_its_type_requires_a_subject() {
    let types = types();
    for record in example_records() {
        let type_id = record["type"].as_str().unwrap().to_owned();

        let rules = types.check(&record).await.expect("the example passes");

        assert_eq!(
            rules.subject_required(),
            type_id != construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE,
            "{type_id}"
        );
    }
}

/// A type registered outside the SDK, deriving from `parent` and stating no
/// traits of its own, or the given ones.
fn registered_elsewhere(type_id: &str, parent: &str, traits: Option<Value>) -> GtsTypeSchema {
    let parent = sdk_record_type_schemas()
        .into_iter()
        .find(|schema| schema.type_id.as_ref() == parent)
        .expect("an SDK type");
    let mut schema =
        json!({ "allOf": [{ "$ref": format!("gts://{parent}", parent = parent.type_id) }] });
    if let Some(traits) = traits {
        schema["x-gts-traits"] = traits;
    }
    GtsTypeSchema::try_new(
        GtsTypeId::new(type_id),
        schema,
        None,
        Some(Arc::new(parent)),
    )
    .expect("a derived type")
}

async fn rules_for(extra: GtsTypeSchema, record: &Value) -> bool {
    types_with_one_more(extra)
        .check(record)
        .await
        .expect("the record passes")
        .subject_required()
}

#[tokio::test]
async fn a_type_derived_from_a_person_type_requires_a_subject() {
    let derived = "gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~acme.chat.export.message.v1~";
    let mut record = chat();
    record["type"] = json!(derived);

    assert!(rules_for(registered_elsewhere(derived, CHAT, None), &record).await);
}

#[tokio::test]
async fn a_type_derived_from_the_course_catalog_inherits_that_no_subject_is_needed() {
    let catalog = construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE;
    let derived = format!("{catalog}acme.lms.catalog.course.v1~");
    let mut record = example(catalog);
    record["type"] = json!(derived);

    assert!(!rules_for(registered_elsewhere(&derived, catalog, None), &record).await);
}

#[tokio::test]
async fn a_new_record_type_that_states_no_traits_requires_a_subject() {
    let new_type = "gts.cf.connectors.core.record.v1~acme.crm.contact.note.v1~";
    let mut record = chat();
    record["type"] = json!(new_type);

    let base = construct_sdk::gts::RECORD_BASE_TYPE;
    assert!(rules_for(registered_elsewhere(new_type, base, None), &record).await);
}

#[tracing_test::traced_test]
#[tokio::test]
async fn a_malformed_subject_trait_requires_a_subject_and_is_logged() {
    // Derived from the catalog, whose `false` must not stand in for the
    // malformed value.
    let catalog = construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE;
    let derived = format!("{catalog}acme.lms.catalog.course.v1~");
    let mut record = example(catalog);
    record["type"] = json!(derived);
    let traits = json!({ "subject_required": "no" });

    assert!(
        rules_for(
            registered_elsewhere(&derived, catalog, Some(traits)),
            &record
        )
        .await
    );
    assert!(logs_contain(
        "record type declares a trait that is not a boolean"
    ));
    assert!(logs_contain(&derived), "the log names the type");
}

#[tokio::test]
async fn a_type_derived_from_a_person_type_cannot_turn_the_subject_off() {
    let derived = format!("{CHAT}acme.chat.export.message.v1~");
    let mut record = chat();
    record["type"] = json!(derived);
    let traits = json!({ "subject_required": false });

    assert!(rules_for(registered_elsewhere(&derived, CHAT, Some(traits)), &record).await);
}

#[tokio::test]
async fn a_type_derived_from_the_course_catalog_can_require_a_subject() {
    let catalog = construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE;
    let derived = format!("{catalog}acme.lms.catalog.enrolment.v1~");
    let mut record = example(catalog);
    record["type"] = json!(derived);
    let traits = json!({ "subject_required": true });

    assert!(
        rules_for(
            registered_elsewhere(&derived, catalog, Some(traits)),
            &record
        )
        .await
    );
}

/// One valid example record of `type_id`.
fn example(type_id: &str) -> Value {
    example_records()
        .into_iter()
        .find(|record| record["type"] == type_id)
        .expect("an example of every derived type")
}

/// A record type, the member to set (or, with `None`, to remove), and the
/// rule its place then breaks.
type BrokenConstraint = (&'static str, &'static str, Option<Value>, &'static str);

/// A chained type id that matches the envelope's pattern but is longer than
/// its cap of 1024 characters.
fn long_type_id() -> Value {
    let segments = "acme.x.y.z.v1~".repeat(80);
    json!(format!("{CHAT}{segments}"))
}

#[tokio::test]
async fn every_generated_constraint_refuses_a_record_that_breaks_it() {
    use construct_sdk::gts::{
        ENRICHMENT_FACT_CANDIDATE_TYPE as FACT, MASTERY_COURSE_CATALOG_TYPE as CATALOG,
        MASTERY_STUDENT_MASTERY_TYPE as MASTERY,
    };
    let long = |n: usize| Some(json!("x".repeat(n)));
    let empty = || Some(json!(""));
    let extra = || Some(json!(1));
    let cases: Vec<BrokenConstraint> = vec![
        // The envelope.
        (CHAT, "/provenance", long(1025), "maxLength"),
        (CHAT, "/provenance", empty(), "minLength"),
        (CHAT, "/version", long(257), "maxLength"),
        (CHAT, "/type", Some(long_type_id()), "maxLength"),
        (CHAT, "/subject_id", Some(json!("not a uuid")), "pattern"),
        // Chat message.
        (CHAT, "/payload/message_id", empty(), "minLength"),
        // Fact candidate.
        (FACT, "/payload/field", Some(json!("hobby")), "enum"),
        (FACT, "/payload/field", None, "required"),
        (FACT, "/payload/value", empty(), "minLength"),
        (FACT, "/payload/value", Some(json!("   ")), "pattern"),
        (FACT, "/payload/value", Some(json!([])), "minItems"),
        (FACT, "/payload/value", Some(json!(7)), "type"),
        (FACT, "/payload/value", None, "required"),
        (FACT, "/payload/confidence", Some(json!(1.5)), "maximum"),
        (FACT, "/payload/confidence", Some(json!(-0.1)), "minimum"),
        (
            FACT,
            "/payload/identity_confidence",
            Some(json!(1.5)),
            "maximum",
        ),
        (
            FACT,
            "/payload/identity_confidence",
            Some(json!(-0.1)),
            "minimum",
        ),
        (FACT, "/payload/identity_confidence", None, "required"),
        (FACT, "/payload/trust_tier", Some(json!("verified")), "enum"),
        (FACT, "/payload/evidence", Some(json!("   ")), "pattern"),
        (FACT, "/payload/rationale", Some(json!("")), "minLength"),
        (FACT, "/payload/rationale", Some(json!("   ")), "pattern"),
        (
            FACT,
            "/payload/source_url",
            Some(json!("ftp://x")),
            "pattern",
        ),
        (FACT, "/payload/extra", extra(), "additionalProperties"),
        // Course catalog.
        (CATALOG, "/payload/course_id", empty(), "minLength"),
        (CATALOG, "/payload/course_id", None, "required"),
        (CATALOG, "/payload/concepts", Some(json!([])), "minItems"),
        (CATALOG, "/payload/extra", extra(), "additionalProperties"),
        // Student mastery.
        (MASTERY, "/payload/model_name", empty(), "minLength"),
        (MASTERY, "/payload/model_name", None, "required"),
        (MASTERY, "/payload/model_version", Some(json!(0)), "minimum"),
        (MASTERY, "/payload/concepts", Some(json!([])), "minItems"),
        (
            MASTERY,
            "/payload/concepts/0/score",
            Some(json!(1.2)),
            "maximum",
        ),
        (
            MASTERY,
            "/payload/concepts/0/score",
            Some(json!(-0.1)),
            "minimum",
        ),
        (MASTERY, "/payload/extra", extra(), "additionalProperties"),
        // Items of the payloads' lists.
        (CATALOG, "/payload/sections/0/title", None, "required"),
        (CATALOG, "/payload/sections/0/title", empty(), "minLength"),
        (
            CATALOG,
            "/payload/sections/0/extra",
            extra(),
            "additionalProperties",
        ),
        (CATALOG, "/payload/concepts/0/name", None, "required"),
        (CATALOG, "/payload/concepts/0/name", empty(), "minLength"),
        (
            CATALOG,
            "/payload/concepts/0/extra",
            extra(),
            "additionalProperties",
        ),
        (
            MASTERY,
            "/payload/concepts/0/signal_count",
            None,
            "required",
        ),
        (
            MASTERY,
            "/payload/concepts/0/extra",
            extra(),
            "additionalProperties",
        ),
    ];
    let types = types();
    for (type_id, pointer, value, rule) in cases {
        let mut record = example(type_id);
        let (parent, member) = pointer.rsplit_once('/').expect("a pointer");
        let target = if parent.is_empty() {
            record.as_object_mut()
        } else {
            record.pointer_mut(parent).and_then(Value::as_object_mut)
        }
        .expect("the member's parent is an object");
        match value {
            Some(value) => target.insert(member.to_owned(), value),
            None => target.remove(member),
        };

        let refused = refused_by(&types, &record).await;

        assert_eq!(
            refused.reason,
            RefusalReason::SchemaViolation,
            "{type_id} {pointer}"
        );
        assert_eq!(
            refused.place,
            Place::pointer(pointer),
            "{type_id} {pointer}"
        );
        assert_eq!(refused.rule, rule, "{type_id} {pointer}");
    }
}

#[tokio::test]
async fn a_refusal_does_not_repeat_the_value() {
    let secret = "my secret role value";
    let mut record = chat();
    record["payload"]["role"] = json!(secret);

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/payload/role"));
    assert_eq!(refused.rule, "enum");
    assert_eq!(refused.type_id.as_deref(), Some(CHAT));
    for field in [refused.place.to_string(), refused.rule] {
        assert!(!field.contains(secret), "{field}");
    }
}

#[tokio::test]
async fn a_malformed_type_is_not_echoed() {
    let mut record = chat();
    record["type"] = json!("<script>a type that is not a type id</script>");

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/type"));
    assert_eq!(refused.rule, "pattern");
    assert_eq!(refused.type_id, None);
}

#[tokio::test]
async fn an_unregistered_type_is_refused() {
    let mut record = chat();
    record["type"] = json!("gts.cf.connectors.core.record.v1~cf.construct.chat.unknown.v1~");

    let refused = refusal(&record).await;

    assert_eq!(refused.reason, RefusalReason::UnknownType);
    assert_eq!(refused.place, Place::pointer("/type"));
}

#[tokio::test]
async fn the_base_type_itself_is_refused_by_the_envelope() {
    let mut record = chat();
    record["type"] = json!(construct_sdk::gts::RECORD_BASE_TYPE);

    let refused = refusal(&record).await;

    assert_eq!(refused.reason, RefusalReason::SchemaViolation);
    assert_eq!(refused.place, Place::pointer("/type"));
    assert_eq!(refused.rule, "pattern");
}

/// A types registry with the SDK's record types and one more type schema.
fn types_with_one_more(extra: GtsTypeSchema) -> RegistryRecordTypes {
    let mut schemas = sdk_record_type_schemas();
    schemas.push(extra);
    let hub = Arc::new(ClientHub::new());
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas(schemas));
    hub.register::<dyn TypesRegistryClient>(registry);
    RegistryRecordTypes::new(hub).expect("record types")
}

#[tokio::test]
async fn an_abstract_derived_type_is_refused() {
    let abstract_kind = "gts.cf.connectors.core.record.v1~cf.construct.chat.any_kind.v1~";
    let base = sdk_record_type_schemas().remove(0);
    let schema = GtsTypeSchema::try_new(
        GtsTypeId::new(abstract_kind),
        json!({ "x-gts-abstract": true, "allOf": [{ "$ref": format!("gts://{}", base.type_id) }] }),
        None,
        Some(Arc::new(base)),
    )
    .expect("abstract type");
    let mut record = chat();
    record["type"] = json!(abstract_kind);

    let refused = refused_by(&types_with_one_more(schema), &record).await;

    assert_eq!(refused.reason, RefusalReason::AbstractType);
    assert_eq!(refused.place, Place::pointer("/type"));
}

#[tokio::test]
async fn a_type_that_does_not_derive_from_the_base_is_refused() {
    // A chained type, so it passes the envelope's shape, rooted elsewhere.
    let other = "gts.cf.core.am.user.v1~cf.construct.fake.thing.v1~";
    let mut record = chat();
    record["type"] = json!(other);

    let refused = refused_by(&types_with_one_more(make_test_type_schema(other)), &record).await;

    assert_eq!(refused.reason, RefusalReason::NotARecordType);
    assert_eq!(refused.place, Place::pointer("/type"));
}

#[tokio::test]
async fn without_a_types_registry_the_check_is_unavailable() {
    let types = RegistryRecordTypes::new(Arc::new(ClientHub::new())).expect("record types");

    let result = types.check(&chat()).await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "got {result:?}"
    );
}

/// A types registry that answers every lookup with one error, or never.
struct BrokenRegistry(Option<fn() -> CanonicalError>);

fn unused() -> CanonicalError {
    CanonicalError::internal("not used by record intake".to_owned()).create()
}

#[async_trait]
impl TypesRegistryClient for BrokenRegistry {
    async fn register(&self, _entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unused())
    }

    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unused())
    }

    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        match self.0 {
            Some(error) => Err(error()),
            None => std::future::pending().await,
        }
    }

    async fn get_type_schema_by_uuid(&self, _uuid: Uuid) -> Result<GtsTypeSchema, CanonicalError> {
        Err(unused())
    }

    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        HashMap::new()
    }

    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        HashMap::new()
    }

    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        Err(unused())
    }

    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(unused())
    }

    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        Err(unused())
    }

    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        Err(unused())
    }

    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        HashMap::new()
    }

    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        HashMap::new()
    }

    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        Err(unused())
    }
}

fn types_with(registry: BrokenRegistry) -> RegistryRecordTypes {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(registry));
    RegistryRecordTypes::new(hub)
        .expect("record types")
        .with_lookup_timeout(Duration::from_millis(10))
}

#[tokio::test]
async fn a_registry_that_fails_in_a_retryable_way_is_unavailable() {
    let types = types_with(BrokenRegistry(Some(|| {
        CanonicalError::service_unavailable().create()
    })));

    let result = types.check(&chat()).await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "got {result:?}"
    );
}

#[tokio::test]
async fn a_registry_that_does_not_answer_in_time_is_unavailable() {
    let result = types_with(BrokenRegistry(None)).check(&chat()).await;

    assert!(
        matches!(result, Err(DomainError::Unavailable(_))),
        "got {result:?}"
    );
}

#[tokio::test]
async fn a_registry_that_calls_the_type_id_malformed_refuses_the_record() {
    let types = types_with(BrokenRegistry(Some(|| {
        RegistryError::invalid_argument()
            .with_field_violation("type_id", "not a type id", "INVALID_GTS_ID")
            .create()
    })));

    let refused = refused_by(&types, &chat()).await;

    assert_eq!(refused.reason, RefusalReason::UnknownType);
    assert_eq!(refused.place, Place::pointer("/type"));
}

#[tokio::test]
async fn any_other_registry_failure_is_internal_and_keeps_its_diagnostic() {
    let types = types_with(BrokenRegistry(Some(|| {
        CanonicalError::internal("registry store is corrupt".to_owned()).create()
    })));

    let result = types.check(&chat()).await;

    let Err(DomainError::Internal(message)) = result else {
        panic!("expected an internal error, got {result:?}");
    };
    assert!(message.contains("registry store is corrupt"), "{message}");
}

fn any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-z~._]{0,40}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::hash_map("[a-z_]{1,12}", inner, 0..6)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Untrusted JSON never panics the check: it passes, is refused, or the
    /// check reports why it could not run.
    #[test]
    fn any_json_is_answered_without_a_panic(record in any_json()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let result = runtime.block_on(types().check(&record));
        prop_assert!(!matches!(result, Err(DomainError::Database(_))));
    }
}

/// Every example record, with all its optional payload members present, reads
/// into its typed envelope and writes back to the same JSON, and passes its
/// schema, so the Rust types and the generated schemas describe one wire
/// format. Optional members must be present: `gts-macros` 0.12 reads every
/// payload member as required and writes an absent one as `null` (see
/// `construct_sdk::gts`).
#[tokio::test]
async fn every_example_reads_into_its_typed_envelope_and_back() {
    use construct_sdk::gts::{
        ChatMessageV1, CourseCatalogV1, ENRICHMENT_FACT_CANDIDATE_TYPE, FactCandidateV1,
        MASTERY_COURSE_CATALOG_TYPE, MASTERY_STUDENT_MASTERY_TYPE, RecordV1, StudentMasteryV1,
    };

    fn round_trip<P>(record: &Value) -> Value
    where
        P: toolkit_gts::GtsSchema,
        RecordV1<P>: serde::Serialize + serde::de::DeserializeOwned,
    {
        let typed: RecordV1<P> = serde_json::from_value(record.clone())
            .unwrap_or_else(|err| panic!("{}: {err}", record["type"]));
        serde_json::to_value(&typed).expect("writes")
    }

    let types = types();
    let examples = example_records();
    assert_eq!(examples.len(), 4, "one example per derived type");
    for mut record in examples {
        let type_id = record["type"].as_str().expect("a type").to_owned();
        let payload = &mut record["payload"];
        match type_id.as_str() {
            ENRICHMENT_FACT_CANDIDATE_TYPE => {
                payload["rationale"] = json!("Matched by name and institution.");
            }
            MASTERY_COURSE_CATALOG_TYPE => {
                for concept in payload["concepts"].as_array_mut().expect("concepts") {
                    concept["description"] = json!("A concept of the course.");
                }
            }
            _ => {}
        }
        types
            .check(&record)
            .await
            .unwrap_or_else(|err| panic!("{type_id}: {err:?}"));

        let written = match type_id.as_str() {
            CHAT => round_trip::<ChatMessageV1>(&record),
            ENRICHMENT_FACT_CANDIDATE_TYPE => round_trip::<FactCandidateV1>(&record),
            MASTERY_COURSE_CATALOG_TYPE => round_trip::<CourseCatalogV1>(&record),
            MASTERY_STUDENT_MASTERY_TYPE => round_trip::<StudentMasteryV1>(&record),
            other => panic!("no typed envelope for {other}"),
        };
        assert_eq!(written, record, "{type_id}");
    }
}

/// The fact candidate's `skills` example: a list value and an upper-case
/// `subject_id`, with the digests its identity derivation gives.
fn skills_candidate() -> Value {
    json!({
        "type": construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE,
        "provenance": "sha256:78c5bf9ec52dfaabfd4a6fe45d1624a28ae09f80d0b6aee0c16df2a8d8871cf6",
        "version": "sha256:1ffe2ff0a3c0a6aa53627d942d4cf7d7ac25a713633d75b0bcafa5e26ff95dfa",
        "observed_at": "2026-09-10T07:30:00Z",
        "subject_id": "5D1F2C3A-8B4E-4C6D-9A1B-2C3D4E5F6A7B",
        "payload": {
            "field": "skills",
            "value": [
                "Bioinformatics",
                "Machine Learning",
                "Statistische Genetik",
                "Gr\u{f6}\u{df}enausschlusschromatographie"
            ],
            "source_url": "https://example.org/people/jane-doe",
            "confidence": 0.74,
            "trust_tier": "found",
            "identity_confidence": 0.91
        }
    })
}

#[tokio::test]
async fn a_fact_candidate_with_a_list_value_for_a_list_field_passes() {
    checked(&types(), &skills_candidate())
        .await
        .expect("a skills list passes");
}

/// The rules of the fact candidate that its generated schema cannot carry.
#[tokio::test]
async fn the_fact_candidates_rules_beyond_its_schema_refuse_a_record_that_breaks_them() {
    let fact = construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE;
    let board = || example(fact);
    let mut cases: Vec<(&str, Value, &str, &str)> = Vec::new();

    let mut record = skills_candidate();
    record["payload"]["value"] = json!("Bioinformatics");
    cases.push(("skills as one string", record, "/payload/value", "type"));

    let mut record = board();
    record["payload"]["value"] = json!(["Jane Doe serves on the board."]);
    cases.push(("board as a list", record, "/payload/value", "type"));

    let mut record = board();
    record["provenance"] = json!("board/whatever");
    cases.push(("provenance not a digest", record, "/provenance", "pattern"));

    let mut record = board();
    record["version"] =
        json!("sha256:F0606B676BE2895716327CE8C55FAA64BFBCD9E6231D2A25F081E6E8BB716E79");
    cases.push(("version in upper case", record, "/version", "pattern"));

    let mut record = board();
    record["payload"]["value"] = json!("\u{1c}\u{1d}");
    cases.push((
        "value of separators only",
        record,
        "/payload/value",
        "pattern",
    ));

    let mut record = skills_candidate();
    record["payload"]["value"][1] = json!("\u{1f}");
    cases.push((
        "an item of separators only",
        record,
        "/payload/value/1",
        "pattern",
    ));

    let mut record = board();
    record["payload"]["value"] = json!("\u{1c} ");
    cases.push((
        "a separator and a space",
        record,
        "/payload/value",
        "pattern",
    ));

    let mut record = skills_candidate();
    record["payload"]["value"][2] = json!("\u{1f}\u{2003}\u{1c}");
    cases.push((
        "a separator and an em space",
        record,
        "/payload/value/2",
        "pattern",
    ));

    let types = types();
    for (label, record, place, rule) in cases {
        let refused = refused_by(&types, &record).await;

        assert_eq!(refused.reason, RefusalReason::SchemaViolation, "{label}");
        assert_eq!(refused.place, Place::pointer(place), "{label}");
        assert_eq!(refused.rule, rule, "{label}");
        assert_eq!(refused.type_id.as_deref(), Some(fact), "{label}");
    }
}

#[tokio::test]
async fn a_type_derived_from_the_fact_candidate_keeps_its_rules() {
    let fact = construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE;
    let derived = format!("{fact}acme.enrich.candidate.fact.v1~");
    let mut record = skills_candidate();
    record["type"] = json!(derived);
    record["payload"]["value"] = json!("Bioinformatics");

    let refused = refused_by(
        &types_with_one_more(registered_elsewhere(&derived, fact, None)),
        &record,
    )
    .await;

    assert_eq!(refused.place, Place::pointer("/payload/value"));
    assert_eq!(refused.rule, "type");
}

#[tokio::test]
async fn a_list_item_that_is_not_stripped_is_refused_at_the_item() {
    let mut record = skills_candidate();
    record["payload"]["value"][0] = json!(" Bioinformatics");

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/payload/value/0"));
    assert_eq!(refused.rule, "pattern");
}

#[tokio::test]
async fn a_research_areas_candidate_takes_a_list_and_refuses_one_string() {
    let types = types();
    let mut record = skills_candidate();
    record["payload"]["field"] = json!("research_areas");
    types
        .check(&record)
        .await
        .expect("a list of research areas passes");

    record["payload"]["value"] = json!("Bioinformatics");
    let refused = refused_by(&types, &record).await;

    assert_eq!(refused.place, Place::pointer("/payload/value"));
    assert_eq!(refused.rule, "type");
}

/// The type's schema runs before the rules it cannot carry: a record that
/// breaks both is refused for the schema rule.
#[tokio::test]
async fn the_schema_is_checked_before_the_type_rules() {
    let mut record = example(construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE);
    record["payload"]["value"] = json!("");
    record["provenance"] = json!("not a digest");

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/payload/value"));
    assert_eq!(refused.rule, "minLength");
}

#[test]
fn the_list_form_of_a_fact_candidate_reads_into_its_typed_envelope_and_back() {
    use construct_sdk::gts::{FactCandidateV1, FactValue, RecordV1};

    let mut record = skills_candidate();
    record["payload"]["evidence"] = json!("Listed on the faculty page.");
    record["payload"]["rationale"] = json!("Matched by name and institution.");
    // The envelope writes the canonical form of the subject id.
    record["subject_id"] = json!("5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b");

    let typed: RecordV1<FactCandidateV1> =
        serde_json::from_value(record.clone()).expect("a skills list reads");

    assert!(matches!(typed.payload.value, FactValue::List(ref items) if items.len() == 4));
    assert_eq!(serde_json::to_value(&typed).expect("writes"), record);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The digest form check agrees with the pattern
    /// `^sha256:[0-9a-f]{64}$`, on arbitrary text and near misses.
    #[test]
    fn the_digest_check_matches_its_pattern(
        provenance in prop_oneof![
            "[a-zA-Z0-9:]{0,80}",
            "sha256:[0-9a-fA-F]{63,65}",
            "sha256:[0-9a-g]{64}",
            "(SHA256|sha256|sha512):[0-9a-f]{64}",
        ]
    ) {
        let pattern = jsonschema::validator_for(&json!({
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$"
        }))
        .expect("the pattern compiles");
        let mut record = example(construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE);
        record["provenance"] = json!(provenance);

        let checked = construct_sdk::gts::check_type_rules(
            construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE,
            &record,
        );

        prop_assert_eq!(checked.is_ok(), pattern.is_valid(&json!(provenance)));
    }

    /// The type rules never panic, whatever `field` and `value` a candidate
    /// with valid digests carries, so every case reaches the value checks.
    #[test]
    fn the_type_rules_answer_any_field_and_value_without_a_panic(
        field in any_json(),
        value in any_json(),
    ) {
        let mut record = skills_candidate();
        record["payload"]["field"] = field;
        record["payload"]["value"] = value;
        let _outcome = construct_sdk::gts::check_type_rules(
            construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE,
            &record,
        );
    }
}

#[tokio::test]
async fn an_empty_or_non_string_list_item_is_refused_at_the_item() {
    let types = types();
    for (item, rule) in [(json!(""), "minLength"), (json!(7), "type")] {
        let mut record = skills_candidate();
        record["payload"]["value"][0] = item.clone();

        let refused = refused_by(&types, &record).await;

        assert_eq!(refused.place, Place::pointer("/payload/value/0"), "{item}");
        assert_eq!(refused.rule, rule, "{item}");
    }
}

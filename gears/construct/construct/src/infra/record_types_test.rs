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
use crate::domain::record_intake::{Place, RecordTypes, Refusal, RefusalReason};
use crate::test_support::{
    chat_record, example_records, hub_with_record_types, sdk_record_type_schemas,
};

const CHAT: &str = construct_sdk::gts::CHAT_MESSAGE_TYPE;

#[resource_error("gts.cf.core.types_registry.entity.v1~")]
struct RegistryError;

fn types() -> RegistryRecordTypes {
    RegistryRecordTypes::new(hub_with_record_types()).expect("record types")
}

/// The refusal a check gives, failing the test on any other outcome.
async fn refused_by(types: &RegistryRecordTypes, record: &Value) -> Refusal {
    match types.check(record).await {
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
async fn a_type_that_requires_a_subject_refuses_a_record_without_one() {
    let mut record = chat();
    record.as_object_mut().unwrap().remove("subject_id");

    let refused = refusal(&record).await;

    assert_eq!(refused.place, Place::pointer("/subject_id"));
    assert_eq!(refused.rule, "required");
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

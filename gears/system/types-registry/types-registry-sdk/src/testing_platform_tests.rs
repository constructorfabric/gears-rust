//! The fake's own surface: seeds, documents, faults, strict mode, admission hooks, inspection and
//! wiring. Its read and write contract is [`super::conformance`], which the registry's local
//! client passes too.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use super::{Call, Fault, MockTypesRegistry, Seed};
use crate::contract::{PlatformTypesRegistryApi, TypesRegistryApi};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason, context};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, CandidateStatus, EntityField, EntityKey, EntityLookup,
    FieldSelection, IdempotencyKey, LifecycleStatus, ListEntitiesRequest, Origin, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationItemResult,
};

const BASE: &str = "gts.cf.test.pkg.base.v1~";
const CHILD: &str = "gts.cf.test.pkg.base.v1~cf.test.pkg.child.v1~";
const TOY: &str = "gts.cf.test.pkg.toy.v1~";
const OTHER: &str = "gts.cf.test.pkg.other.v1~";

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn key(id: &str) -> EntityKey {
    EntityKey::GtsId(gts::GtsId::try_new(id).expect("valid identifier"))
}

fn base(traits: &Value) -> Value {
    json!({
        "$id": format!("gts://{BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits-schema": { "type": "object", "properties": traits },
    })
}

fn child() -> Value {
    json!({
        "$id": format!("gts://{CHILD}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [{ "$ref": format!("gts://{BASE}") }],
        "x-gts-traits": { "tier": "gold" },
    })
}

fn tier() -> Value {
    json!({ "tier": { "type": "string", "default": "basic" } })
}

fn tier_and_region() -> Value {
    json!({
        "tier": { "type": "string", "default": "basic" },
        "region": { "type": "string", "default": "eu" },
    })
}

fn documents() -> Projection {
    Projection::Select(FieldSelection::with(&[
        EntityField::ResolvedSchema,
        EntityField::EffectiveTraits,
        EntityField::EffectiveTraitsSchema,
    ]))
}

fn light() -> Projection {
    Projection::Select(FieldSelection::light())
}

async fn get(
    fake: &MockTypesRegistry,
    keys: &[EntityKey],
    projection: Projection,
) -> Result<Vec<EntityLookup>, CanonicalError> {
    let mut answers = PlatformTypesRegistryApi::batch_get_entities(
        fake,
        &ctx(),
        BatchGetEntitiesRequest {
            items: keys.iter().cloned().map(BatchGetItem::from).collect(),
            projection,
            fresh: false,
        },
    )
    .await?
    .0;
    Ok(keys
        .iter()
        .map(|k| answers.remove(k).expect("every key is answered"))
        .collect())
}

async fn traits(fake: &MockTypesRegistry, id: &str) -> Result<Value, CanonicalError> {
    match get(fake, &[key(id)], documents()).await?.remove(0) {
        EntityLookup::Found { entity, .. } => Ok(entity.effective_traits.expect("selected")),
        other => panic!("{id}: {other:?}"),
    }
}

fn create(id: &str, content: Value) -> RegisterItem {
    RegisterItem {
        gts_id: gts::GtsId::try_new(id).expect("valid identifier"),
        content,
        expected_resource_version: None,
        skip_compatibility_check: false,
    }
}

async fn register(
    fake: &MockTypesRegistry,
    key: &str,
    items: Vec<RegisterItem>,
) -> Result<Vec<RegistrationItemResult>, CanonicalError> {
    let request = RegisterEntitiesRequest {
        items,
        dry_run: false,
        publisher: PublisherContext {
            name: "fake-test".to_owned(),
            version: "1.0.0".parse().expect("version"),
        },
    };
    let operation = fake
        .register_entities(&ctx(), IdempotencyKey::new(key).expect("key"), request)
        .await?;
    Ok(operation.items)
}

fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable().create()
}

#[tokio::test]
async fn the_fake_conforms_to_the_contract() {
    super::conformance::run(&MockTypesRegistry::new()).await;
}

// seeds and documents.

#[tokio::test]
async fn seeded_types_resolve_their_documents_in_any_order() {
    let fake = MockTypesRegistry::new();
    fake.seed(CHILD, child());
    fake.seed(BASE, base(&tier()));

    assert_eq!(
        traits(&fake, CHILD).await.expect("resolves"),
        json!({ "tier": "gold" })
    );
}

#[tokio::test]
async fn seeded_schema_documents_do_not_require_entity_reference_targets() {
    let fake = MockTypesRegistry::new();
    let schema = json!({
        "$id": format!("gts://{TOY}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "target": { "type": "string", "x-gts-type-ref": OTHER },
        },
    });
    fake.seed(TOY, schema);

    let lookup = get(&fake, &[key(TOY)], documents())
        .await
        .expect("entity-reference targets need not be registered")
        .remove(0);
    let EntityLookup::Found { entity, .. } = lookup else {
        panic!("{lookup:?}");
    };
    let resolved = entity.resolved_schema.expect("selected");
    assert_eq!(resolved["properties"]["target"]["x-gts-type-ref"], OTHER);
    assert_eq!(entity.effective_traits, Some(json!({})));
    assert!(entity.effective_traits_schema.is_some());
}

#[tokio::test]
async fn a_type_that_does_not_resolve_fails_only_a_read_of_its_documents() {
    let fake = MockTypesRegistry::new();
    fake.seed(TOY, json!({ "not": "a schema" }));

    let light = get(&fake, &[key(TOY)], light()).await.expect("reads");
    assert!(matches!(light[0], EntityLookup::Found { .. }));
    let error = get(&fake, &[key(TOY)], documents())
        .await
        .expect_err("its documents cannot be produced");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_seed_sets_version_and_lifecycle() {
    let fake = MockTypesRegistry::new();
    fake.seed_entity(Seed::new(TOY, json!({})).version(7).deleted());

    let lookup = get(
        &fake,
        &[key(TOY)],
        Projection::Select(FieldSelection::with(&[EntityField::Origin])),
    )
    .await
    .expect("reads")
    .remove(0);
    let EntityLookup::Found { entity, .. } = lookup else {
        panic!("{lookup:?}");
    };
    assert_eq!(entity.lifecycle_status, LifecycleStatus::Deleted);
    assert!(matches!(
        entity.origin,
        Some(Origin::Managed {
            resource_version: 7,
            ..
        })
    ));
    assert_eq!(
        fake.content(TOY),
        None,
        "content() answers active entities only"
    );
}

#[tokio::test]
async fn a_seeded_tombstone_fixes_its_documents_on_its_own_first_successful_read() {
    let fake = MockTypesRegistry::new();
    fake.seed_entity(Seed::new(CHILD, child()).deleted());
    fake.seed(TOY, json!({}));

    // Reading anything else, or the tombstone before it can resolve, fixes nothing.
    get(&fake, &[key(TOY)], light()).await.expect("reads");
    traits(&fake, CHILD).await.expect_err("no parent yet");

    fake.seed(BASE, base(&tier()));
    assert_eq!(
        traits(&fake, CHILD).await.expect("resolves"),
        json!({ "tier": "gold" })
    );

    fake.seed(BASE, base(&tier_and_region()));
    assert_eq!(
        traits(&fake, CHILD).await.expect("still resolves"),
        json!({ "tier": "gold" }),
        "the tombstone kept the documents of its first successful read"
    );
}

#[tokio::test]
async fn seed_inventory_stores_the_linked_declarations() {
    const PLUGIN: &str = "gts.cf.toolkit.plugins.plugin.v1~";
    let fake = MockTypesRegistry::new();

    fake.seed_inventory();

    assert!(
        fake.content(PLUGIN).is_some(),
        "toolkit-gts declares {PLUGIN}"
    );
    let lookup = get(&fake, &[key(PLUGIN)], documents())
        .await
        .expect("a declared type resolves")
        .remove(0);
    assert!(matches!(lookup, EntityLookup::Found { .. }), "{lookup:?}");
}

// faults.

#[tokio::test]
async fn every_matching_rule_counts_a_call_and_the_first_admitting_one_applies() {
    let fake = MockTypesRegistry::new();
    fake.seed(TOY, json!({}));
    fake.inject(Fault::on(Call::BatchGet).first(1).fail(unavailable()));
    fake.inject(
        Fault::on(Call::BatchGet)
            .nth(2)
            .fail(CanonicalError::internal("second").create()),
    );

    let first = get(&fake, &[key(TOY)], light()).await.expect_err("first");
    assert!(
        matches!(first, CanonicalError::ServiceUnavailable { .. }),
        "{first:?}"
    );
    let second = get(&fake, &[key(TOY)], light()).await.expect_err("second");
    assert!(
        matches!(second, CanonicalError::Internal { .. }),
        "{second:?}"
    );
    get(&fake, &[key(TOY)], light())
        .await
        .expect("the third passes");
    assert_eq!(fake.calls(Call::BatchGet), 3, "failed calls are counted");
}

#[tokio::test(start_paused = true)]
async fn a_rule_delays_then_fails_and_the_attempt_is_still_recorded() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register)
            .from(1)
            .delay(Duration::from_secs(10))
            .fail(unavailable()),
    );
    let started = tokio::time::Instant::now();

    register(&fake, "k", vec![create(TOY, json!({}))])
        .await
        .expect_err("fails");

    assert!(started.elapsed() >= Duration::from_secs(10));
    assert_eq!(fake.submissions().len(), 1);
    assert_eq!(fake.registered()[0].gts_id.to_string(), TOY);
}

#[tokio::test]
async fn a_key_rule_matches_an_identifier_and_its_registry_reference() {
    let fake = MockTypesRegistry::new();
    fake.seed(TOY, json!({}));
    fake.seed(OTHER, json!({}));
    let reference = gts::GtsId::try_new(TOY).expect("valid").to_uuid();
    fake.inject(Fault::on(Call::BatchGet).key(reference).fail(unavailable()));

    get(&fake, &[key(TOY)], light())
        .await
        .expect_err("the identifier names the same entity");
    get(&fake, &[key(OTHER), key(TOY)], light())
        .await
        .expect_err("any key of the batch matches");
    get(&fake, &[key(OTHER)], light())
        .await
        .expect("another key passes");
}

#[tokio::test]
async fn an_any_rule_applies_to_every_method() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::any().fail(unavailable()));

    PlatformTypesRegistryApi::list_entities(&fake, &ctx(), ListEntitiesRequest::default())
        .await
        .expect_err("lists fail");
    fake.get_operation(&ctx(), uuid::Uuid::nil())
        .await
        .expect_err("polls fail");
}

#[test]
#[should_panic(expected = "a key selector never matches")]
fn a_key_rule_on_a_method_without_keys_is_refused() {
    MockTypesRegistry::new().inject(Fault::on(Call::List).key(key(TOY)));
}

// admission.

#[tokio::test]
async fn admission_rejects_and_waits_on_dependencies_as_told() {
    let fake = MockTypesRegistry::new().completing_after(0);
    fake.reject(TOY, AdmissionFailureReason::InvalidSchema);
    fake.depends_on(OTHER, BASE);

    let items = register(
        &fake,
        "first",
        vec![create(TOY, json!({})), create(OTHER, json!({}))],
    )
    .await
    .expect("accepted");
    let failure = |i: usize| {
        AdmissionFailure::from_canonical(items[i].outcome.error().expect("failed"))
            .expect("an admission failure")
    };
    assert_eq!(failure(0).reason, AdmissionFailureReason::InvalidSchema);
    assert_eq!(
        failure(1).reason,
        AdmissionFailureReason::DependencyNotFound
    );
    assert_eq!(failure(1).context(context::DEPENDENCY_ID), Some(BASE));

    fake.seed(BASE, base(&tier()));
    let items = register(&fake, "second", vec![create(OTHER, json!({}))])
        .await
        .expect("accepted");
    assert_eq!(items[0].outcome.status(), CandidateStatus::Succeeded);
}

// strict mode.

#[tokio::test]
#[should_panic(expected = "unexpected BatchGet call")]
async fn strict_mode_panics_on_a_method_it_was_not_told_to_allow() {
    let fake = MockTypesRegistry::strict();
    fake.seed(TOY, json!({}));
    let _unreached = get(&fake, &[key(TOY)], light()).await;
}

#[tokio::test]
#[should_panic(expected = "unexpected read of")]
async fn strict_mode_panics_on_a_read_of_an_unexpected_key() {
    let fake = MockTypesRegistry::strict();
    fake.allow(Call::BatchGet);
    let _unreached = get(&fake, &[key(TOY)], light()).await;
}

#[tokio::test]
async fn strict_mode_answers_stored_and_expected_absent_keys() {
    let fake = MockTypesRegistry::strict();
    fake.allow(Call::BatchGet);
    fake.seed(TOY, json!({}));
    fake.expect_absent(key(OTHER));

    let answers = get(&fake, &[key(TOY), key(OTHER)], light())
        .await
        .expect("reads");
    assert!(matches!(answers[0], EntityLookup::Found { .. }));
    assert!(matches!(answers[1], EntityLookup::NotFound));
}

// inspection and wiring.

#[tokio::test]
async fn reads_and_registrations_are_recorded_in_order() {
    let fake = MockTypesRegistry::new();
    get(&fake, &[key(TOY), key(OTHER)], light())
        .await
        .expect("reads");
    register(
        &fake,
        "k",
        vec![create(TOY, json!({})), create(OTHER, json!({}))],
    )
    .await
    .expect("accepted");

    assert_eq!(fake.reads(), [key(TOY), key(OTHER)]);
    let registered: Vec<String> = fake
        .registered()
        .iter()
        .map(|i| i.gts_id.to_string())
        .collect();
    assert_eq!(registered, [TOY, OTHER]);
    assert_eq!(fake.calls(Call::BatchGet), 1);
    assert_eq!(fake.calls(Call::Register), 1);
    assert_eq!(fake.calls(Call::List), 0);
}

#[tokio::test]
async fn install_registers_one_fake_under_both_contracts() {
    let fake = Arc::new(MockTypesRegistry::new());
    let hub = toolkit::ClientHub::new();
    fake.install(&hub);

    let platform = hub
        .get::<dyn PlatformTypesRegistryApi>()
        .expect("the platform contract resolves");
    hub.get::<dyn TypesRegistryApi>()
        .expect("the tenant contract resolves");
    platform
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("lists");
    assert_eq!(fake.calls(Call::List), 1, "the hub serves this fake");
}

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Resource-group types as managed registry entities (T38b RG slice).
// @cpt-dod:cpt-cf-resource-group-dod-testing-managed-types:p1
//!
//! Admits the RG SDK's published base `gts.cf.core.rg.type.v1~` and derived RG
//! types through the real local client and database/outbox admission, then reads
//! the materializations resource-group consumes: `effective_traits` for placement
//! and `resolved_schema` + `content` for the SDK payload validator.

use std::sync::Arc;

use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::{gts_id, gts_uri};
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::local_client::LocalClient;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::infra::outbox::OutboxDispatch;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsTypeId, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions, ReconcileOptions,
    TypeSchema,
};

use resource_group_sdk::gts::ResourceGroupTypeTraitsV1;

const BASE: &str = gts_id!("cf.core.rg.type.v1~");
const USER: &str = gts_id!("cf.test.rg.user.v1~");
const TENANT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~");
const DEPT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~");

struct Harness {
    client: LocalClient,
    _outbox: Box<dyn std::any::Any + Send>,
}

async fn harness() -> Harness {
    let dsn = format!(
        "sqlite:file:rg-managed-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect admission database");
    let mut migrations = types_registry::infra::storage::Migrator::migrations();
    migrations.extend(
        toolkit_db::outbox::outbox_migrations_with_prefix(
            types_registry::infra::outbox::TABLE_PREFIX,
        )
        .expect("registry outbox prefix"),
    );
    run_migrations_for_testing(&db, migrations)
        .await
        .expect("registry and outbox migrations");
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        Arc::new(types_registry::infra::storage::Repos),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        Arc::new(types_registry::domain::ports::metrics::NoopMetrics),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Harness {
        client: LocalClient::new(service),
        _outbox: Box::new(outbox),
    }
}

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid identifier")
}

/// The base exactly as resource-group publishes it.
fn rg_base() -> Value {
    let (_, text) = resource_group_sdk::gts_declarations()
        .into_iter()
        .find(|(declared, _)| declared == BASE)
        .expect("the RG SDK declares the base");
    serde_json::from_str(&text).expect("declared JSON")
}

fn user_type() -> Value {
    json!({
        "$id": gts_uri!("cf.test.rg.user.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "id": { "type": "string" } }
    })
}

/// A tenant RG type: self-nesting, users as members, and metadata described
/// through a local `$defs` reference.
fn tenant_type() -> Value {
    json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits": {
            "can_be_root": true,
            "allowed_parent_types": [TENANT],
            "allowed_membership_types": [USER]
        },
        "$defs": {
            "tenant_meta": {
                "type": "object",
                "required": ["tier"],
                "properties": { "tier": { "type": "string" } }
            }
        },
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~") },
            { "properties": { "metadata": { "$ref": "#/$defs/tenant_meta" } } }
        ]
    })
}

/// Derived from the tenant type: inherits its metadata constraints and adds one.
fn dept_type() -> Value {
    json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-traits": {
            "can_be_root": false,
            "allowed_parent_types": [TENANT],
            "allowed_membership_types": []
        },
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~") },
            { "properties": { "metadata": {
                "type": "object",
                "properties": { "code": { "type": "string", "maxLength": 3 } }
            } } }
        ]
    })
}

async fn admit(h: &Harness, desired: &[(GtsId, Value)]) {
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    api.reconcile_entities_and_await(
        &PlatformSecurityContext::outbound_marker(),
        &PublisherContext {
            name: "rg-managed-types-test".to_owned(),
            version: "0.1.0".parse().expect("version"),
        },
        desired,
        &ReconcileOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("all RG declarations are admitted");
}

async fn read(h: &Harness, type_id: &str) -> TypeSchema {
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    api.get_type_schema(
        &PlatformSecurityContext::outbound_marker(),
        &GtsTypeId::new(type_id),
        ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
            EntityField::EffectiveTraits,
        ]))),
    )
    .await
    .expect("admitted type")
}

async fn admitted_rg_types() -> Harness {
    let h = harness().await;
    admit(
        &h,
        &[
            (id(BASE), rg_base()),
            (id(USER), user_type()),
            (id(TENANT), tenant_type()),
            (id(DEPT), dept_type()),
        ],
    )
    .await;
    h
}

fn traits(schema: &TypeSchema) -> ResourceGroupTypeTraitsV1 {
    serde_json::from_value(schema.effective_traits.clone().expect("selected traits"))
        .expect("effective traits match the RG traits schema")
}

#[tokio::test]
async fn the_base_materializes_the_default_placement_traits() {
    let h = admitted_rg_types().await;
    assert_eq!(
        traits(&read(&h, BASE).await),
        ResourceGroupTypeTraitsV1::default()
    );
}

#[tokio::test]
async fn a_self_nesting_tenant_type_is_admitted_with_its_traits() {
    let h = admitted_rg_types().await;
    let traits = traits(&read(&h, TENANT).await);
    assert!(traits.can_be_root);
    assert_eq!(traits.allowed_parent_types[0].0, TENANT);
    assert_eq!(traits.allowed_membership_types[0].0, USER);
}

#[tokio::test]
async fn a_parent_type_outside_the_rg_family_is_refused() {
    let h = harness().await;
    let mut wrong = tenant_type();
    wrong["x-gts-traits"]["allowed_parent_types"] = json!([USER]);
    let api: &dyn PlatformTypesRegistryApi = &h.client;
    let error = api
        .reconcile_entities_and_await(
            &PlatformSecurityContext::outbound_marker(),
            &PublisherContext {
                name: "rg-managed-types-test".to_owned(),
                version: "0.1.0".parse().expect("version"),
            },
            &[
                (id(BASE), rg_base()),
                (id(USER), user_type()),
                (id(TENANT), wrong),
            ],
            &ReconcileOptions::default(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect_err("allowed_parent_types names a non-RG type");
    let report = types_registry_sdk::ReconcileReport::from_canonical(&error).expect("report");
    let types_registry_sdk::ReconcileOutcome::Rejected(refusal) = &report.outcomes[&id(TENANT)]
    else {
        panic!("the tenant type is the refused candidate: {report:?}");
    };
    let failure = types_registry_sdk::AdmissionFailure::from_canonical(refusal)
        .expect("an admission refusal");
    assert_eq!(
        failure.reason,
        types_registry_sdk::AdmissionFailureReason::InvalidSchema,
        "{failure:?}"
    );
    assert!(
        failure.message.contains("/allowed_parent_types/0") && failure.message.contains(USER),
        "the RG type reference on the parent trait refuses the value: {failure:?}"
    );
}

#[tokio::test]
async fn the_sdk_validator_applies_inherited_metadata_through_a_nested_ref() {
    let h = admitted_rg_types().await;
    let validator = read(&h, DEPT)
        .await
        .compile_payload_validator()
        .expect("compiles from the managed materialization");

    validator
        .validate(&json!({ "metadata": { "tier": "gold", "code": "ab" } }))
        .expect("conforms to the tenant and dept constraints");
    validator
        .validate(&json!({ "metadata": { "code": "ab" } }))
        .expect_err("tier is required by the inherited, $defs-referenced tenant metadata");
    validator
        .validate(&json!({ "metadata": { "tier": 7 } }))
        .expect_err("tier must be a string");
    validator
        .validate(&json!({ "metadata": { "tier": "gold", "code": "abcd" } }))
        .expect_err("dept's own constraint applies");
    validator
        .validate(&json!({ "metadata": "not an object" }))
        .expect_err("the base requires an object");
}

/// The base as the previous RG SDK declared it in the global inventory (instance
/// properties, no traits), which a persisted registry may already hold. Verbatim
/// output of the old `#[gts_type_schema]` declaration (the em dash as a JSON
/// `\u` escape).
const PREVIOUS_RG_BASE: &str = r#"{"$id":"gts://gts.cf.core.rg.type.v1~","$schema":"http://json-schema.org/draft-07/schema#","additionalProperties":false,"description":"Resource Group base type \u2014 defines placement and tenant scope traits","properties":{"allowed_membership_types":{"description":"GTS type paths of allowed membership resource types.","items":{"type":"string"},"type":"array"},"allowed_parent_types":{"description":"GTS type paths of allowed parent types.","items":{"type":"string"},"type":"array"},"can_be_root":{"description":"Whether groups of this type can be root nodes (no parent). Default `false`.","type":"boolean"},"id":{"allOf":[{"description":"GTS instance identifier","title":"GTS Instance ID","type":"string","x-gts-instance-ref":"gts.*"}],"description":"GTS type path (schema identifier)."}},"required":["id","can_be_root","allowed_parent_types","allowed_membership_types"],"type":"object"}"#;

#[tokio::test]
async fn a_registry_seeded_with_the_previous_base_admits_the_new_one() {
    let h = harness().await;
    admit(
        &h,
        &[(
            id(BASE),
            serde_json::from_str(PREVIOUS_RG_BASE).expect("previous base JSON"),
        )],
    )
    .await;

    admit(
        &h,
        &[
            (id(BASE), rg_base()),
            (id(USER), user_type()),
            (id(TENANT), tenant_type()),
        ],
    )
    .await;

    let base = read(&h, BASE).await;
    assert_eq!(
        base.content,
        Some(rg_base()),
        "the new base replaced the old"
    );
    assert_eq!(traits(&base), ResourceGroupTypeTraitsV1::default());
    assert!(traits(&read(&h, TENANT).await).can_be_root);
}

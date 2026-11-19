#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Resource-group types as managed registry entities (T38b RG slice).
// @cpt-dod:cpt-cf-resource-group-dod-testing-managed-types:p1
//!
//! Admits group and membership schemas through the real Registry admission
//! path and exercises the RG writes that consume the admitted definitions.
//! The SDK's in-memory fake skips publisher guards and DB-backed admission,
//! so the registry is wired here from its implementation crate's internals;
//! a change to that wiring has to be mirrored in this test.

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
    CompilePayloadValidatorError, EntityField, FieldSelection, GtsCompilePayloadValidatorError,
    GtsId, GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection,
    PublisherContext, ReadOptions, ReconcileOptions, TypeSchema,
};

const BASE: &str = gts_id!("cf.core.rg.type.v1~");
const USER: &str = gts_id!("cf.test.rg.user.v1~");
const TENANT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~");
const DEPT: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~");

struct Harness {
    client: Arc<LocalClient>,
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
        client: Arc::new(LocalClient::new(service)),
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
        "$defs": {
            "tenant_meta": {
                "type": "object",
                "required": ["tier"],
                "properties": { "tier": { "type": "string" } }
            }
        },
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~") },
            { "properties": {
                "parent_type": {"type": ["string", "null"], "enum": [null, TENANT]},
                "membership_type": {"type": "string", "const": TEAM_MEMBER},
                "metadata": { "$ref": "#/$defs/tenant_meta" }
            } }
        ]
    })
}

/// Derived from the tenant type: inherits its metadata constraints and adds one.
fn dept_type() -> Value {
    json!({
        "$id": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~cf.test.rg.dept.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            { "$ref": gts_uri!("cf.core.rg.type.v1~cf.test.rg.tenant.v1~") },
            { "properties": { "metadata": {
                "type": "object",
                "properties": { "code": { "type": "string", "maxLength": 3 } }
            } } }
        ]
    })
}

async fn reconcile(
    h: &Harness,
    desired: &[(GtsId, Value)],
) -> Result<types_registry_sdk::Reconciliation, toolkit_canonical_errors::CanonicalError> {
    let api: &dyn PlatformTypesRegistryApi = h.client.as_ref();
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
}

async fn admit(h: &Harness, desired: &[(GtsId, Value)]) {
    reconcile(h, desired)
        .await
        .expect("all RG declarations are admitted");
}

async fn revision_refusal(
    h: &Harness,
    code: &str,
    document: Value,
) -> types_registry_sdk::AdmissionFailureReason {
    use types_registry_sdk::{AdmissionFailure, ReconcileOutcome, ReconcileReport};

    let error = reconcile(h, &[(id(code), document)])
        .await
        .expect_err("refused revision");
    let report = ReconcileReport::from_canonical(&error).expect("report");
    let ReconcileOutcome::Rejected(refusal) = &report.outcomes[&id(code)] else {
        panic!("expected a rejected candidate: {report:?}");
    };
    AdmissionFailure::from_canonical(refusal)
        .expect("admission failure")
        .reason
}

async fn read(h: &Harness, type_id: &str) -> TypeSchema {
    let api: &dyn PlatformTypesRegistryApi = h.client.as_ref();
    api.get_type_schema(
        &PlatformSecurityContext::outbound_marker(),
        &GtsTypeId::new(type_id),
        ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
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

#[tokio::test]
async fn the_abstract_group_base_constrains_parent_types_of_concrete_groups() {
    let h = harness().await;
    let concrete = json!({
        "$id": format!("gts://{TEAM_SCHEMA}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            {"$ref": format!("gts://{BASE}")},
            {"properties": {"membership_type": {"type": "string", "const": TEAM_MEMBER}}}
        ]
    });
    admit(&h, &[(id(BASE), rg_base()), (id(TEAM_SCHEMA), concrete)]).await;
    assert!(matches!(
        read(&h, BASE).await.compile_payload_validator(),
        Err(CompilePayloadValidatorError::Upstream(
            GtsCompilePayloadValidatorError::Abstract(_)
        ))
    ));

    let validator = read(&h, TEAM_SCHEMA)
        .await
        .compile_payload_validator()
        .expect("concrete group inherits the base parent type constraint");

    for parent_type in [Value::Null, json!(TENANT), json!(DEPT)] {
        validator
            .validate(&json!({"parent_type": parent_type, "membership_type": TEAM_MEMBER}))
            .expect("root or a type in the resource-group family");
    }
    validator
        .validate(&json!({"membership_type": TEAM_MEMBER}))
        .expect_err("parent_type is required");
    validator
        .validate(&json!({"parent_type": USER, "membership_type": TEAM_MEMBER}))
        .expect_err("a type outside the resource-group family cannot be a parent");
}

#[tokio::test]
async fn the_group_payload_requires_parent_type_and_membership_type() {
    let h = admitted_rg_types().await;
    let validator = read(&h, TENANT)
        .await
        .compile_payload_validator()
        .expect("tenant schema");
    let mut root = json!({
        "parent_type": null, "membership_type": TEAM_MEMBER, "metadata": {"tier": "gold"}
    });
    validator
        .validate(&root)
        .expect("null is the required root placement value");
    root.as_object_mut()
        .expect("group document")
        .remove("parent_type");
    validator
        .validate(&root)
        .expect_err("parent_type is required");
    root["parent_type"] = json!(TENANT);
    validator
        .validate(&root)
        .expect("nests under the same type");
    root.as_object_mut()
        .expect("group document")
        .remove("membership_type");
    validator
        .validate(&root)
        .expect_err("membership_type is required");
}

#[tokio::test]
async fn the_sdk_validator_applies_inherited_metadata_through_a_nested_ref() {
    let h = admitted_rg_types().await;
    let validator = read(&h, DEPT)
        .await
        .compile_payload_validator()
        .expect("compiles from the managed materialization");

    let payload = |metadata| {
        json!({
            "parent_type": null, "membership_type": TEAM_MEMBER, "metadata": metadata
        })
    };
    validator
        .validate(&payload(json!({ "tier": "gold", "code": "ab" })))
        .expect("conforms to the tenant and dept constraints");
    for invalid in [
        json!({"code": "ab"}),
        json!({"tier": 7}),
        json!({"tier": "gold", "code": "abcd"}),
        json!("not an object"),
    ] {
        validator
            .validate(&payload(invalid))
            .expect_err("metadata constraints apply");
    }
}

mod common;

const MEMBER_V1: &str = gts_id!("cf.core.rg.membership.v1~");
const DEPT_SCHEMA: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.dept.v1~");
const TEAM_SCHEMA: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.team.v1~");
const TEAM_MEMBER: &str = gts_id!("cf.core.rg.membership.v1~cf.test.rg.team_member.v1~");

fn group_schema(code: &str, extra_parent: bool) -> Value {
    let mut parents = vec![
        json!({"type": "null"}),
        json!({"type": "string", "x-gts-type-ref": code}),
    ];
    if extra_parent {
        parents.push(json!({"type": "string", "x-gts-type-ref": DEPT_SCHEMA}));
    }
    json!({
        "$id": format!("gts://{code}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            {"$ref": format!("gts://{BASE}")},
            {
                "required": ["parent_type", "membership_type"],
                "properties": {
                    "parent_type": {"anyOf": parents},
                    "membership_type": {"type": "string", "const": TEAM_MEMBER}
                }
            }
        ]
    })
}

#[tokio::test]
async fn group_and_membership_schemas_drive_writes_and_evolve_through_admission() {
    use resource_group::domain::error::DomainError;
    use resource_group::domain::repo::GroupRepositoryTrait;
    use resource_group::infra::storage::group_repo::GroupRepository;
    use resource_group::infra::types_registry::RegistryTypeCatalog;
    use resource_group_sdk::{CreateGroupRequest, UpdateGroupRequest};
    use types_registry_sdk::AdmissionFailureReason::{
        CompatibilityUndecidable, IncompatibleWithBaseline,
    };

    let h = harness().await;
    let bases = resource_group_sdk::gts_declarations();
    let declared = |code| {
        let (_, text) = bases
            .iter()
            .find(|(declared, _)| declared == code)
            .expect("the RG SDK declares the schema");
        (id(code), serde_json::from_str(text).expect("schema JSON"))
    };
    let mut membership = json!({
        "$id": format!("gts://{TEAM_MEMBER}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            {"$ref": format!("gts://{MEMBER_V1}")},
            {"properties": {"resource_type": {"type": "string", "const": USER}}}
        ]
    });
    admit(
        &h,
        &[
            declared(BASE),
            declared(MEMBER_V1),
            (id(USER), user_type()),
            (id(DEPT_SCHEMA), group_schema(DEPT_SCHEMA, false)),
            (id(TEAM_SCHEMA), group_schema(TEAM_SCHEMA, false)),
            (id(TEAM_MEMBER), membership.clone()),
        ],
    )
    .await;

    let db = common::test_db().await;
    let catalog = Arc::new(RegistryTypeCatalog::new(h.client.clone()));
    let groups = common::make_group_service_with(db.clone(), catalog.clone());
    let members = common::make_membership_service_with(db.clone(), catalog);
    let tenant = uuid::Uuid::new_v4();
    let ctx = common::make_ctx(tenant);
    let imported_id = uuid::Uuid::new_v4();
    {
        let conn = db.conn().expect("RG database");
        let type_key = GroupRepository
            .ensure_type_key(&conn, TEAM_SCHEMA)
            .await
            .expect("imported group type");
        GroupRepository
            .insert(
                &conn,
                imported_id,
                None,
                type_key,
                None,
                "Imported",
                None,
                tenant,
            )
            .await
            .expect("pre-existing group without a binding column value");
    }
    members
        .add_membership(&ctx, imported_id, USER, "imported-user")
        .await
        .expect("the existing row uses the published membership schema");
    assert!(matches!(
        members
            .add_membership(&ctx, imported_id, DEPT_SCHEMA, "not-a-user")
            .await,
        Err(DomainError::Validation { .. })
    ));
    let department = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(DEPT_SCHEMA, "Engineering"),
            tenant,
        )
        .await
        .expect("department root");
    let team = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(TEAM_SCHEMA, "Platform"),
            tenant,
        )
        .await
        .expect("team root");
    let move_team = || UpdateGroupRequest {
        name: team.name.clone(),
        parent_id: Some(department.id),
        metadata: None,
    };
    let binding = GroupRepository
        .find_model_by_id(&db.conn().expect("RG database"), team.id)
        .await
        .expect("read group")
        .expect("stored group")
        .membership_type;
    assert_eq!(binding.as_deref(), Some(TEAM_MEMBER));
    assert!(matches!(
        groups.update_group(&ctx, team.id, move_team()).await,
        Err(DomainError::Validation { .. })
    ));
    admit(&h, &[(id(TEAM_SCHEMA), group_schema(TEAM_SCHEMA, true))]).await;
    let moved = groups
        .update_group(&ctx, team.id, move_team())
        .await
        .expect("compatible widening");
    assert_eq!(moved.hierarchy.parent_id, Some(department.id));
    assert!(matches!(
        revision_refusal(&h, TEAM_SCHEMA, group_schema(TEAM_SCHEMA, false)).await,
        IncompatibleWithBaseline | CompatibilityUndecidable
    ));

    let mut changed_binding = group_schema(TEAM_SCHEMA, true);
    changed_binding["allOf"][1]["properties"]["membership_type"]["const"] = json!(gts_id!(
        "cf.core.rg.membership.v1~cf.test.rg.other_member.v1~"
    ));
    assert_eq!(
        revision_refusal(&h, TEAM_SCHEMA, changed_binding).await,
        IncompatibleWithBaseline
    );

    assert!(matches!(
        members
            .add_membership(&ctx, team.id, DEPT_SCHEMA, "department-42")
            .await,
        Err(DomainError::Validation { .. })
    ));
    assert!(matches!(
        members.add_membership(&ctx, team.id, USER, "").await,
        Err(DomainError::Validation { .. })
    ));
    members
        .add_membership(&ctx, team.id, USER, "user-42")
        .await
        .expect("membership schema accepts users");

    membership["allOf"][1]["properties"]["resource_type"] = json!({
        "anyOf": [{"type": "string", "x-gts-type-ref": USER}]
    });
    admit(&h, &[(id(TEAM_MEMBER), membership.clone())]).await;
    let mut widened = membership.clone();
    widened["allOf"][1]["properties"]["resource_type"]["anyOf"]
        .as_array_mut()
        .expect("membership type families")
        .push(json!({"type": "string", "x-gts-type-ref": DEPT_SCHEMA}));
    admit(&h, &[(id(TEAM_MEMBER), widened)]).await;
    members
        .add_membership(&ctx, team.id, DEPT_SCHEMA, "department-42")
        .await
        .expect("existing group uses the current membership schema");
    assert!(matches!(
        revision_refusal(&h, TEAM_MEMBER, membership).await,
        IncompatibleWithBaseline | CompatibilityUndecidable
    ));

    let mut without_binding = group_schema(TEAM_SCHEMA, true);
    without_binding["allOf"][1]["properties"]
        .as_object_mut()
        .expect("schema properties")
        .remove("membership_type");
    admit(&h, &[(id(TEAM_SCHEMA), without_binding)]).await;
    assert!(matches!(
        groups
            .create_group(
                &ctx,
                CreateGroupRequest::new(TEAM_SCHEMA, "Unbound"),
                tenant
            )
            .await,
        Err(DomainError::Validation { .. })
    ));
    members
        .add_membership(&ctx, team.id, USER, "user-43")
        .await
        .expect("the group keeps its persisted binding");
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use gts::GtsId;
use parking_lot::Mutex;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_id;
use types_registry_sdk::{
    CandidateError, CandidateStatus, EntitySnapshot, OperationStatus, RegisterEntities,
    RegistrationItemResult, RegistrationOperation, TypesRegistryEntities,
};

use super::*;

const ROOT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~");

fn config(idp_provisioning: bool) -> RootTypeConfig {
    RootTypeConfig {
        gts_id: gts::GtsTypeId::new(ROOT_TYPE),
        idp_provisioning,
    }
}

fn snapshot(cfg: &RootTypeConfig) -> EntitySnapshot {
    EntitySnapshot {
        gts_id: ROOT_TYPE.to_owned(),
        gts_uuid: GtsId::try_new(ROOT_TYPE).expect("valid id").to_uuid(),
        kind: EntityKind::TypeSchema,
        lifecycle_status: LifecycleStatus::Active,
        resource_version: 1,
        owning_gear: Some(AM_OWNING_GEAR.to_owned()),
        content: Some(desired_root_schema(cfg).expect("desired schema")),
        resolved_schema: None,
        effective_traits: Some(json!({
            "allowed_parent_types": [],
            "idp_provisioning": cfg.idp_provisioning,
        })),
        effective_traits_schema: None,
    }
}

#[derive(Clone, Copy)]
enum RegisterBehaviour {
    Succeed,
    LoseRace,
    LoseResponseAfterCommit,
}

struct BarrierRegistry {
    inner: Arc<dyn TypesRegistryEntities>,
    initial_root_read: Arc<tokio::sync::Barrier>,
    waiting: AtomicBool,
}

impl BarrierRegistry {
    fn new(
        inner: Arc<dyn TypesRegistryEntities>,
        initial_root_read: Arc<tokio::sync::Barrier>,
    ) -> Self {
        Self {
            inner,
            initial_root_read,
            waiting: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl TypesRegistryEntities for BarrierRegistry {
    async fn get_entity(&self, gts_id: &str) -> Result<Option<EntitySnapshot>, CanonicalError> {
        if gts_id == ROOT_TYPE && self.waiting.swap(false, Ordering::AcqRel) {
            self.initial_root_read.wait().await;
        }
        self.inner.get_entity(gts_id).await
    }

    async fn compare_and_swap_owning_gear(
        &self,
        gts_id: &str,
        expected_resource_version: i64,
        expected_owning_gear: Option<String>,
        owning_gear: String,
    ) -> Result<bool, CanonicalError> {
        self.inner
            .compare_and_swap_owning_gear(
                gts_id,
                expected_resource_version,
                expected_owning_gear,
                owning_gear,
            )
            .await
    }

    async fn register_and_await(
        &self,
        idempotency_key: String,
        request: RegisterEntities,
        deadline: Duration,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.inner
            .register_and_await(idempotency_key, request, deadline)
            .await
    }
}

struct MockPersistentRegistry {
    base_present: Mutex<bool>,
    root: Mutex<Option<EntitySnapshot>>,
    desired_after_register: EntitySnapshot,
    behaviour: RegisterBehaviour,
    submissions: Mutex<usize>,
    reads_unavailable: bool,
    replacement_on_cas: Mutex<Option<EntitySnapshot>>,
}

impl MockPersistentRegistry {
    fn new(
        initial: Option<EntitySnapshot>,
        desired_after_register: EntitySnapshot,
        behaviour: RegisterBehaviour,
    ) -> Self {
        Self {
            base_present: Mutex::new(true),
            root: Mutex::new(initial),
            desired_after_register,
            behaviour,
            submissions: Mutex::new(0),
            reads_unavailable: false,
            replacement_on_cas: Mutex::new(None),
        }
    }

    fn with_replacement_on_cas(self, replacement: EntitySnapshot) -> Self {
        *self.replacement_on_cas.lock() = Some(replacement);
        self
    }

    fn with_unavailable_reads(mut self) -> Self {
        self.reads_unavailable = true;
        self
    }

    fn with_missing_base(self) -> Self {
        *self.base_present.lock() = false;
        self
    }
}

#[async_trait]
impl TypesRegistryEntities for MockPersistentRegistry {
    async fn get_entity(&self, gts_id: &str) -> Result<Option<EntitySnapshot>, CanonicalError> {
        if self.reads_unavailable {
            return Err(CanonicalError::service_unavailable()
                .with_detail("simulated registry outage")
                .create());
        }
        if gts_id == TENANT_TYPE_BASE {
            return Ok(self.base_present.lock().then(|| EntitySnapshot {
                gts_id: TENANT_TYPE_BASE.to_owned(),
                gts_uuid: Uuid::nil(),
                kind: EntityKind::TypeSchema,
                lifecycle_status: LifecycleStatus::Active,
                resource_version: 1,
                owning_gear: Some(AM_OWNING_GEAR.to_owned()),
                content: Some(TenantTypeEnvelopeV1::<()>::gts_schema_with_refs()),
                resolved_schema: None,
                effective_traits: None,
                effective_traits_schema: None,
            }));
        }
        Ok(self.root.lock().clone())
    }

    async fn compare_and_swap_owning_gear(
        &self,
        gts_id: &str,
        expected_resource_version: i64,
        expected_owning_gear: Option<String>,
        owning_gear: String,
    ) -> Result<bool, CanonicalError> {
        if gts_id == TENANT_TYPE_BASE {
            return Ok(true);
        }
        let mut root = self.root.lock();
        if let Some(replacement) = self.replacement_on_cas.lock().take() {
            *root = Some(replacement);
            return Ok(false);
        }
        let Some(stored) = root.as_mut() else {
            return Ok(false);
        };
        if stored.resource_version != expected_resource_version
            || stored.owning_gear != expected_owning_gear
        {
            return Ok(false);
        }
        stored.resource_version += 1;
        stored.owning_gear = Some(owning_gear);
        Ok(true)
    }

    async fn register_and_await(
        &self,
        _idempotency_key: String,
        request: RegisterEntities,
        _deadline: Duration,
    ) -> Result<RegistrationOperation, CanonicalError> {
        *self.submissions.lock() += 1;
        assert_eq!(request.owning_gear, AM_OWNING_GEAR);
        if request.items.len() == 1 && request.items[0].gts_id == TENANT_TYPE_BASE {
            *self.base_present.lock() = true;
            return Ok(RegistrationOperation {
                operation_id: Uuid::from_u128(3),
                status: OperationStatus::Completed,
                items: vec![RegistrationItemResult {
                    gts_id: TENANT_TYPE_BASE.to_owned(),
                    status: CandidateStatus::Succeeded,
                    resource_version: Some(1),
                    error: None,
                }],
            });
        }
        assert!(request.items.iter().any(|item| item.gts_id == ROOT_TYPE));
        *self.root.lock() = Some(self.desired_after_register.clone());
        if matches!(self.behaviour, RegisterBehaviour::LoseResponseAfterCommit) {
            return Err(CanonicalError::service_unavailable()
                .with_detail("simulated lost operation receipt")
                .create());
        }
        let (status, error) = match self.behaviour {
            RegisterBehaviour::Succeed => (CandidateStatus::Succeeded, None),
            RegisterBehaviour::LoseRace => (
                CandidateStatus::Failed,
                Some(CandidateError {
                    reason: PRECONDITION_FAILED.to_owned(),
                    message: "another replica created it".to_owned(),
                }),
            ),
            RegisterBehaviour::LoseResponseAfterCommit => unreachable!("handled above"),
        };
        Ok(RegistrationOperation {
            operation_id: Uuid::from_u128(2),
            status: OperationStatus::Completed,
            items: vec![RegistrationItemResult {
                gts_id: ROOT_TYPE.to_owned(),
                status,
                resource_version: Some(1),
                error,
            }],
        })
    }
}

#[test]
fn configured_id_must_be_concrete_and_in_am_chain() {
    for invalid in [
        TENANT_TYPE_BASE,
        gts_id!("cf.other.am.tenant_type.v1~cf.core.am.platform.v1~"),
        gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1"),
    ] {
        let mut cfg = config(false);
        cfg.gts_id = gts::GtsTypeId::new(invalid);
        assert!(desired_root_schema(&cfg).is_err(), "must reject {invalid}");
    }
}

#[test]
fn root_operation_key_is_stable_and_content_sensitive() {
    let disabled = desired_root_schema(&config(false)).expect("disabled schema");
    let enabled = desired_root_schema(&config(true)).expect("enabled schema");
    assert_eq!(
        idempotency_key("root-type", &disabled).expect("key"),
        idempotency_key("root-type", &disabled).expect("same key")
    );
    assert_ne!(
        idempotency_key("root-type", &disabled).expect("disabled key"),
        idempotency_key("root-type", &enabled).expect("enabled key")
    );
}

#[test]
fn desired_schema_contains_only_the_configured_root_contract() {
    let desired = desired_root_schema(&config(true)).expect("desired root schema");
    assert_eq!(
        desired.get("$id").and_then(Value::as_str),
        Some("gts://gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~")
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/allowed_parent_types"),
        Some(&json!([]))
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/idp_provisioning"),
        Some(&json!(true))
    );
}

#[test]
fn descriptive_metadata_does_not_create_drift() {
    let cfg = config(false);
    let mut stored = snapshot(&cfg);
    let content = stored
        .content
        .as_mut()
        .and_then(Value::as_object_mut)
        .expect("object schema");
    content.insert("title".to_owned(), json!("Operator title"));
    content.insert("description".to_owned(), json!("Operator description"));
    validate_persisted_root(&stored, &cfg).expect("annotations are not semantic drift");
}

#[test]
fn persisted_uuid_drift_is_rejected() {
    let cfg = config(false);
    let mut stored = snapshot(&cfg);
    stored.gts_uuid = Uuid::nil();

    let error = validate_persisted_root(&stored, &cfg).expect_err("UUID drift must fail");
    assert!(error.to_string().contains("deterministic UUID"));
    assert!(error.to_string().contains("explicit registry repair"));
}

#[test]
fn incompatible_authored_schema_fields_are_rejected() {
    let cfg = config(false);
    let mut stored = snapshot(&cfg);
    stored
        .content
        .as_mut()
        .and_then(Value::as_object_mut)
        .expect("object schema")
        .insert("maxProperties".to_owned(), json!(1));

    let error = validate_persisted_root(&stored, &cfg).expect_err("schema keyword must drift");
    assert!(
        error
            .to_string()
            .contains("incompatible AM-owned schema fields")
    );
}

#[test]
fn idp_provisioning_drift_is_rejected() {
    let cfg = config(false);
    let mut stored = snapshot(&cfg);
    stored.effective_traits = Some(json!({
        "allowed_parent_types": [],
        "idp_provisioning": true,
    }));
    let error = validate_persisted_root(&stored, &cfg).expect_err("drift must fail");
    assert!(error.to_string().contains("idp_provisioning"));
}

#[test]
fn non_empty_effective_parents_are_rejected() {
    let cfg = config(false);
    let mut stored = snapshot(&cfg);
    stored.effective_traits = Some(json!({
        "allowed_parent_types": [ROOT_TYPE],
        "idp_provisioning": false,
    }));
    let error = validate_persisted_root(&stored, &cfg).expect_err("parents must fail");
    assert!(error.to_string().contains("allowed_parent_types"));
}

#[tokio::test]
async fn matching_existing_schema_is_a_read_only_success() {
    let cfg = config(false);
    let registry = Arc::new(MockPersistentRegistry::new(
        Some(snapshot(&cfg)),
        snapshot(&cfg),
        RegisterBehaviour::Succeed,
    ));
    let outcome = reconcile_root_type(registry.clone(), &cfg)
        .await
        .expect("matching schema");
    assert_eq!(outcome, RootTypeReconcileOutcome::Existing);
    assert_eq!(*registry.submissions.lock(), 0);
}

#[tokio::test]
async fn matching_legacy_or_unowned_schema_is_adopted_without_registration() {
    for previous_owner in [None, Some(LEGACY_OWNING_GEAR.to_owned())] {
        let cfg = config(false);
        let mut legacy = snapshot(&cfg);
        legacy.owning_gear = previous_owner;
        let registry = Arc::new(MockPersistentRegistry::new(
            Some(legacy),
            snapshot(&cfg),
            RegisterBehaviour::Succeed,
        ));

        let outcome = reconcile_root_type(registry.clone(), &cfg)
            .await
            .expect("matching schema is safe to adopt");
        assert_eq!(outcome, RootTypeReconcileOutcome::Adopted);
        assert_eq!(*registry.submissions.lock(), 0);
        let adopted = registry.root.lock().clone().expect("root");
        assert_eq!(adopted.owning_gear.as_deref(), Some(AM_OWNING_GEAR));
        assert_eq!(adopted.resource_version, 2);
    }
}

#[tokio::test]
async fn adoption_revalidates_content_after_losing_the_owner_cas() {
    let cfg = config(false);
    let mut legacy = snapshot(&cfg);
    legacy.owning_gear = Some(LEGACY_OWNING_GEAR.to_owned());
    let mut concurrent_drift = snapshot(&config(true));
    concurrent_drift.owning_gear = Some(LEGACY_OWNING_GEAR.to_owned());
    concurrent_drift.resource_version = 2;
    let registry = Arc::new(
        MockPersistentRegistry::new(Some(legacy), snapshot(&cfg), RegisterBehaviour::Succeed)
            .with_replacement_on_cas(concurrent_drift),
    );

    let error = reconcile_root_type(registry, &cfg)
        .await
        .expect_err("changed content must be validated before adoption retry");
    assert!(error.to_string().contains("idp_provisioning"));
}

#[tokio::test]
async fn matching_schema_owned_by_another_gear_is_not_taken_over() {
    let cfg = config(false);
    let mut foreign = snapshot(&cfg);
    foreign.owning_gear = Some("foreign-gear".to_owned());
    let registry = Arc::new(MockPersistentRegistry::new(
        Some(foreign),
        snapshot(&cfg),
        RegisterBehaviour::Succeed,
    ));

    let error = reconcile_root_type(registry.clone(), &cfg)
        .await
        .expect_err("foreign ownership is startup-fatal");
    assert!(
        error
            .to_string()
            .contains("automatic ownership takeover is forbidden")
    );
    assert_eq!(*registry.submissions.lock(), 0);
}

async fn sqlite_registry() -> Arc<dyn TypesRegistryEntities> {
    sqlite_registry_with_mode(types_registry::domain::registry_service::AdmissionMode::Inline).await
}

async fn sqlite_registry_with_mode(
    mode: types_registry::domain::registry_service::AdmissionMode,
) -> Arc<dyn TypesRegistryEntities> {
    use sea_orm_migration::MigratorTrait;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, DBProvider, connect_db};
    use types_registry::config::TypesRegistryConfig;
    use types_registry::domain::admission::NullDispatch;
    use types_registry::domain::entities_client::TypesRegistryEntitiesClient;
    use types_registry::domain::policy::RegistrationPolicy;
    use types_registry::domain::ports::metrics::NoopMetrics;
    use types_registry::domain::registry_service::RegistryService;
    use types_registry::infra::storage::{Migrator, Repos};

    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect SQLite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrate persistent registry");
    let provider = DBProvider::<toolkit_db::DbError>::new(db);
    let service = Arc::new(RegistryService::new(
        provider.db(),
        Arc::new(Repos),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::new(NullDispatch),
        mode,
        Arc::new(NoopMetrics),
    ));
    Arc::new(TypesRegistryEntitiesClient::new(service))
}

#[tokio::test]
async fn persistent_polling_obeys_one_global_deadline() {
    use types_registry::domain::registry_service::AdmissionMode;

    let registry = sqlite_registry_with_mode(AdmissionMode::Outbox).await;
    let error = registry
        .register_and_await(
            "deadline-test".to_owned(),
            RegisterEntities {
                items: vec![RegisterItem {
                    gts_id: TENANT_TYPE_BASE.to_owned(),
                    content: TenantTypeEnvelopeV1::<()>::gts_schema_with_refs(),
                    expected_resource_version: None,
                    force: false,
                }],
                dry_run: false,
                owning_gear: AM_OWNING_GEAR.to_owned(),
            },
            Duration::from_millis(20),
        )
        .await
        .expect_err("an undriven outbox operation must time out");
    assert!(error.to_string().contains("deadline"));
}

#[tokio::test]
async fn sqlite_authoritative_store_creates_and_replays_the_owned_contract() {
    let registry = sqlite_registry().await;
    let cfg = config(false);

    assert_eq!(
        reconcile_root_type(registry.clone(), &cfg)
            .await
            .expect("initial reconciliation"),
        RootTypeReconcileOutcome::Created
    );
    let stored = registry
        .get_entity(ROOT_TYPE)
        .await
        .expect("authoritative read")
        .expect("persisted root");
    assert_eq!(stored.owning_gear.as_deref(), Some(AM_OWNING_GEAR));
    validate_persisted_root(&stored, &cfg).expect("persisted contract");
    assert_eq!(
        reconcile_root_type(registry, &cfg)
            .await
            .expect("idempotent replay"),
        RootTypeReconcileOutcome::Existing
    );
}

#[tokio::test]
async fn two_sqlite_replicas_converge_on_one_authoritative_contract() {
    let registry = sqlite_registry().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first: Arc<dyn TypesRegistryEntities> = Arc::new(BarrierRegistry::new(
        Arc::clone(&registry),
        Arc::clone(&barrier),
    ));
    let second: Arc<dyn TypesRegistryEntities> =
        Arc::new(BarrierRegistry::new(Arc::clone(&registry), barrier));
    let cfg = config(false);

    let (first_outcome, second_outcome) = tokio::join!(
        reconcile_root_type(first, &cfg),
        reconcile_root_type(second, &cfg)
    );
    assert!(first_outcome.is_ok(), "first replica: {first_outcome:?}");
    assert!(second_outcome.is_ok(), "second replica: {second_outcome:?}");

    let stored = registry
        .get_entity(ROOT_TYPE)
        .await
        .expect("authoritative read")
        .expect("one persisted root");
    assert_eq!(stored.owning_gear.as_deref(), Some(AM_OWNING_GEAR));
    validate_persisted_root(&stored, &cfg).expect("converged contract");
}

#[tokio::test]
async fn two_sqlite_replicas_with_conflicting_contracts_do_not_both_start() {
    let registry = sqlite_registry().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first: Arc<dyn TypesRegistryEntities> = Arc::new(BarrierRegistry::new(
        Arc::clone(&registry),
        Arc::clone(&barrier),
    ));
    let second: Arc<dyn TypesRegistryEntities> =
        Arc::new(BarrierRegistry::new(Arc::clone(&registry), barrier));
    let disabled = config(false);
    let enabled = config(true);

    let (first_outcome, second_outcome) = tokio::join!(
        reconcile_root_type(first, &disabled),
        reconcile_root_type(second, &enabled)
    );
    assert_ne!(
        first_outcome.is_ok(),
        second_outcome.is_ok(),
        "exactly one conflicting replica must converge: first={first_outcome:?}, second={second_outcome:?}"
    );
    let failure = first_outcome
        .err()
        .or_else(|| second_outcome.err())
        .expect("one conflict");
    assert!(failure.to_string().contains("idp_provisioning"));
}

#[tokio::test]
async fn absent_schema_is_registered_and_read_back() {
    let cfg = config(false);
    let registry = Arc::new(MockPersistentRegistry::new(
        None,
        snapshot(&cfg),
        RegisterBehaviour::Succeed,
    ));
    let outcome = reconcile_root_type(registry.clone(), &cfg)
        .await
        .expect("created schema");
    assert_eq!(outcome, RootTypeReconcileOutcome::Created);
    assert_eq!(*registry.submissions.lock(), 1);
}

#[tokio::test]
async fn missing_am_base_is_admitted_before_the_concrete_root() {
    let cfg = config(false);
    let registry = Arc::new(
        MockPersistentRegistry::new(None, snapshot(&cfg), RegisterBehaviour::Succeed)
            .with_missing_base(),
    );
    let outcome = reconcile_root_type(registry.clone(), &cfg)
        .await
        .expect("base and root created");
    assert_eq!(outcome, RootTypeReconcileOutcome::Created);
    assert_eq!(*registry.submissions.lock(), 2);
}

#[tokio::test]
async fn concurrent_matching_create_converges() {
    let cfg = config(false);
    let registry = Arc::new(MockPersistentRegistry::new(
        None,
        snapshot(&cfg),
        RegisterBehaviour::LoseRace,
    ));
    let outcome = reconcile_root_type(registry, &cfg)
        .await
        .expect("race winner matches");
    assert_eq!(outcome, RootTypeReconcileOutcome::ConcurrentlyCreated);
}

#[tokio::test]
async fn lost_registration_receipt_succeeds_only_after_matching_fresh_read() {
    let cfg = config(false);
    let registry = Arc::new(MockPersistentRegistry::new(
        None,
        snapshot(&cfg),
        RegisterBehaviour::LoseResponseAfterCommit,
    ));
    let outcome = reconcile_root_type(registry, &cfg)
        .await
        .expect("fresh read proves the ambiguous commit");
    assert_eq!(outcome, RootTypeReconcileOutcome::ConcurrentlyCreated);
}

#[tokio::test]
async fn registry_unavailability_is_startup_fatal() {
    let cfg = config(false);
    let registry = Arc::new(
        MockPersistentRegistry::new(None, snapshot(&cfg), RegisterBehaviour::Succeed)
            .with_unavailable_reads(),
    );

    let error = reconcile_root_type(registry, &cfg)
        .await
        .expect_err("registry outage must not be suppressed");
    assert!(error.to_string().contains("simulated registry outage"));
}

#[tokio::test]
async fn concurrent_conflicting_create_fails_after_reread() {
    let cfg = config(false);
    let conflicting = snapshot(&config(true));
    let registry = Arc::new(MockPersistentRegistry::new(
        None,
        conflicting,
        RegisterBehaviour::LoseRace,
    ));
    let error = reconcile_root_type(registry, &cfg)
        .await
        .expect_err("race winner conflicts");
    assert!(error.to_string().contains("idp_provisioning"));
}

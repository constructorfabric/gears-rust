#![allow(clippy::missing_panics_doc)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quota_resolver_sdk::models::{
    LeaseOutcome, MetricItem, MetricObject, PlatformCommitLeaseRequest,
    PlatformReleaseLeaseRequest, PlatformReturnRequest, PluginAcquireLeaseRequest,
    PluginDebitRequest, PluginQuotaStateRequest, QuotaDecision, QuotaState, QuotaSubject,
};
use quota_resolver_sdk::{QuotaResolverPluginClient, QuotaResolverPluginSpecV1};
use serde_json::{Map, json};
use toolkit::Page;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use toolkit_security::PlatformIdentity;
use types_registry_sdk::testing_platform::{MockTypesRegistry, Seed};
use uuid::Uuid;

use super::ports::{Operation, Outcome, Plane, QuotaMetrics};
use super::service::Service;

pub const VENDOR: &str = "acme";
pub const PRINCIPAL_BASE: &str = "gts.cf.core.test.principal.v1~";
pub const USER_TYPE: &str = "gts.cf.core.test.principal.v1~cf.core.test.user.v1~";
pub const ADMIN_TYPE: &str =
    "gts.cf.core.test.principal.v1~cf.core.test.user.v1~cf.core.test.admin.v1~";
pub const USER_SUBJECT: &str = "gts.cf.core.quota.subj.v1~cf.core.test.user_subject.v1~";
pub const ADMIN_SUBJECT: &str = "gts.cf.core.quota.subj.v1~cf.core.test.admin_subject.v1~";
pub const METRIC_TYPE: &str = "gts.cf.core.quota.metric.v1~cf.core.test.tokens.v1~";

#[resource_error(gts_id!("cf.core.quota.request.v1~"))]
pub struct PluginError;

#[must_use]
pub fn aborted() -> CanonicalError {
    PluginError::aborted("lock timeout")
        .with_reason("LOCK_TIMEOUT")
        .create()
}

#[must_use]
pub fn invalid_argument() -> CanonicalError {
    PluginError::invalid_argument()
        .with_field_violation("subjects", "not admitted", "SUBJECT_TYPE_NOT_ADMITTED")
        .create()
}

#[must_use]
pub fn already_exists() -> CanonicalError {
    PluginError::already_exists("key in use")
        .with_resource("key")
        .create()
}

#[must_use]
pub fn tenant() -> Uuid {
    Uuid::from_u128(0x5eed_0000_0000_0000_0000_0000_0000_0001)
}

#[must_use]
pub fn principal_id() -> Uuid {
    Uuid::from_u128(0x5eed_0000_0000_0000_0000_0000_0000_0002)
}

#[must_use]
pub fn item() -> MetricItem {
    MetricItem {
        metric: MetricObject {
            metric_type: METRIC_TYPE.to_owned(),
            id: None,
            metadata: Map::new(),
            extra: Map::new(),
        },
        amount: 5,
    }
}

#[must_use]
pub fn quota_state() -> QuotaState {
    QuotaState {
        quota_id: "q1".to_owned(),
        subject: QuotaSubject {
            subject_type: USER_SUBJECT.to_owned(),
            id: principal_id().to_string(),
        },
        metric_type: METRIC_TYPE.to_owned(),
        cap: Some(10),
        consumed: 4,
        remaining: Some(6),
        period: None,
        metadata: None,
    }
}

#[domain_model]
#[derive(Default)]
pub struct MockPlugin {
    pub debits: Mutex<Vec<PluginDebitRequest>>,
    pub leases: Mutex<Vec<PluginAcquireLeaseRequest>>,
    pub states: Mutex<Vec<PluginQuotaStateRequest>>,
    pub settlements: Mutex<Vec<(Uuid, Option<PlatformIdentity>)>>,
    failure: Mutex<Option<CanonicalError>>,
}

impl MockPlugin {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn failing(err: CanonicalError) -> Arc<Self> {
        let plugin = Self::new();
        *plugin.failure.lock().unwrap() = Some(err);
        plugin
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.debits.lock().unwrap().len()
            + self.leases.lock().unwrap().len()
            + self.states.lock().unwrap().len()
            + self.settlements.lock().unwrap().len()
    }

    fn outcome<T>(&self, ok: T) -> Result<T, CanonicalError> {
        match self.failure.lock().unwrap().clone() {
            Some(err) => Err(err),
            None => Ok(ok),
        }
    }
}

#[async_trait]
impl QuotaResolverPluginClient for MockPlugin {
    async fn debit(&self, request: PluginDebitRequest) -> Result<QuotaDecision, CanonicalError> {
        self.debits.lock().unwrap().push(request);
        self.outcome(QuotaDecision::allowed())
    }

    async fn acquire_lease(
        &self,
        request: PluginAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError> {
        self.leases.lock().unwrap().push(request);
        self.outcome(LeaseOutcome::Acquired {
            token: "lease-1".to_owned(),
            expires_at: chrono::DateTime::UNIX_EPOCH,
        })
    }

    async fn commit_lease(
        &self,
        request: PlatformCommitLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        self.settlements
            .lock()
            .unwrap()
            .push((request.tenant_id, caller));
        self.outcome(())
    }

    async fn release_lease(
        &self,
        request: PlatformReleaseLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        self.settlements
            .lock()
            .unwrap()
            .push((request.tenant_id, caller));
        self.outcome(())
    }

    async fn return_amount(
        &self,
        request: PlatformReturnRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        self.settlements
            .lock()
            .unwrap()
            .push((request.tenant_id, caller));
        self.outcome(())
    }

    async fn quota_state(
        &self,
        request: PluginQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        self.states.lock().unwrap().push(request);
        self.outcome(page_of_one())
    }
}

#[must_use]
pub fn page_of_one() -> Page<QuotaState> {
    let mut page = Page::empty(7);
    page.items.push(quota_state());
    page.page_info.next_cursor = Some("next".to_owned());
    page
}

#[domain_model]
#[derive(Default)]
pub struct RecordingMetrics {
    pub operations: Mutex<Vec<(Operation, Plane, Outcome)>>,
    pub overheads: Mutex<usize>,
}

impl RecordingMetrics {
    #[must_use]
    pub fn last(&self) -> (Operation, Plane, Outcome) {
        *self
            .operations
            .lock()
            .unwrap()
            .last()
            .expect("an operation was recorded")
    }
}

impl QuotaMetrics for RecordingMetrics {
    fn record_operation(&self, operation: Operation, plane: Plane, outcome: Outcome) {
        self.operations
            .lock()
            .unwrap()
            .push((operation, plane, outcome));
    }

    fn record_overhead_ms(&self, _millis: f64) {
        *self.overheads.lock().unwrap() += 1;
    }
}

#[must_use]
pub fn spec_id() -> String {
    QuotaResolverPluginSpecV1::gts_type_id().as_ref().to_owned()
}

#[must_use]
pub fn instance_id(name: &str) -> String {
    format!("{}test.quota_resolver.{name}.instance.v1", spec_id())
}

pub fn seed_type(registry: &MockTypesRegistry, type_id: &str) {
    seed_type_with(registry, type_id, false);
}

pub fn seed_deleted_type(registry: &MockTypesRegistry, type_id: &str) {
    seed_type_with(registry, type_id, true);
}

fn seed_type_with(registry: &MockTypesRegistry, type_id: &str, deleted: bool) {
    let ends: Vec<usize> = type_id.match_indices('~').map(|(i, _)| i).collect();
    for (n, end) in ends.iter().enumerate() {
        let id = &type_id[..=*end];
        let mut schema = json!({
            "$id": format!("gts://{id}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        });
        if let Some(parent_end) = n.checked_sub(1).map(|p| ends[p]) {
            schema["allOf"] = json!([{ "$ref": format!("gts://{}", &type_id[..=parent_end]) }]);
        }
        let seed = Seed::new(id, schema);
        registry.seed_entity(if deleted && n + 1 == ends.len() {
            seed.deleted()
        } else {
            seed
        });
    }
}

pub fn seed_plugin(registry: &MockTypesRegistry, id: &str, vendor: &str, priority: i16) {
    registry.seed(
        id,
        json!({ "id": id, "vendor": vendor, "priority": priority, "properties": {} }),
    );
}

#[domain_model]
pub struct Env {
    pub hub: Arc<ClientHub>,
    pub registry: Arc<MockTypesRegistry>,
    pub plugin: Arc<MockPlugin>,
    pub metrics: Arc<RecordingMetrics>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    #[must_use]
    pub fn with_plugin(plugin: Arc<MockPlugin>) -> Self {
        let hub = Arc::new(ClientHub::default());
        let registry = Arc::new(MockTypesRegistry::new());
        registry.install(&hub);
        let id = instance_id("mock");
        seed_plugin(&registry, &id, VENDOR, 0);
        hub.register_scoped::<dyn QuotaResolverPluginClient>(
            ClientScope::gts_id(&id),
            plugin.clone() as Arc<dyn QuotaResolverPluginClient>,
        );
        Self {
            hub,
            registry,
            plugin,
            metrics: Arc::new(RecordingMetrics::default()),
        }
    }

    #[must_use]
    pub fn new() -> Self {
        Self::with_plugin(MockPlugin::new())
    }

    #[must_use]
    pub fn service(&self) -> Service {
        self.service_with(&[(USER_TYPE, USER_SUBJECT)])
    }

    #[must_use]
    pub fn service_with(&self, mapping: &[(&str, &str)]) -> Service {
        Service::new(
            self.hub.clone(),
            VENDOR.to_owned(),
            mapping
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            self.metrics.clone(),
        )
    }
}

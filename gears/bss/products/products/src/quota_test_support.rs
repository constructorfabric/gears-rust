//! The quota intake's test doubles and fixtures (P-D-268), written once for the record, snapshot,
//! staging and endpoint suites.
//!
//! Products registers no metric, unit or policy instance (P-D-267): every instance here is one a
//! test adapter registers in its own namespace, `cf.compute.quota_adapter`, as the package's
//! examples do. [`catalog_types`] and [`catalog_instances`] are the one list of what the registry
//! holds; [`seed_catalog`] loads it into the port fake and [`seed_mock`] into the platform's
//! in-memory registry, so both answer the same entities.
// Test-only: every helper panics on a broken fixture, which is a bug in the test.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::missing_panics_doc
)]
#![allow(unknown_lints, de0901_gts_string_pattern)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bss_products_sdk::ProductsQuotaDiscoveryPluginClientV1;
use bss_products_sdk::quota::digest::snapshot_digest;
use bss_products_sdk::quota::discovery::{QuotaDiscoveryPage, QuotaDiscoveryRequest};
use bss_products_sdk::quota::gts::{
    ACCOUNTING_POLICY_TYPE, CAPABILITY_TYPE, DISCOVERY_PLUGIN_TYPE, METRIC_TYPE, SCOPE_POLICY_TYPE,
    UNIT_TYPE,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use crate::domain::quota::ports::{
    QuotaDiscoveryResolver, QuotaRegistry, RegistryInstance, RegistryTypeSchema,
    RegistryUnavailable, RegistryVerdict, ResolvedDiscovery,
};
use crate::domain::quota::snapshot::QuotaSettings;

/// The test adapter's discovery plugin instance.
pub const TEST_PLUGIN: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.bss.products.quota_discovery.v1~\
                               cf.compute.quota_adapter.test.v1";
/// The toolkit's plugin base schema the discovery specification derives from.
pub const PLUGIN_BASE: &str = "gts.cf.toolkit.plugins.plugin.v1~";
/// The package's example metric: allocated vCPUs.
pub const VCPU_METRIC: &str =
    "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1";
/// The package's example unit: one vCPU, its own canonical unit.
pub const VCPU_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1";
/// The package's example accounting policy.
pub const ALLOCATED_VCPU: &str =
    "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated_vcpu.v1";
/// The package's example scope policy.
pub const REGIONAL_POOL: &str =
    "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1";
/// The RAM family's canonical unit.
pub const BYTES_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.bytes.v1";
/// 10^9 bytes.
pub const GB_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.gb.v1";
/// 2^30 bytes.
pub const GIB_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.gib.v1";
/// Allocated RAM, counted in bytes.
pub const RAM_METRIC: &str =
    "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.allocated_ram.v1";
/// The RAM family's accounting policy.
pub const ALLOCATED_RAM: &str =
    "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated_ram.v1";
/// The package's example instances: the vCPU unit and metric and the two policies.
pub const WELL_KNOWN: [&str; 4] = [VCPU_UNIT, VCPU_METRIC, ALLOCATED_VCPU, REGIONAL_POOL];
/// A capability schema the test adapter derives, with a string `properties.reviewed_by`.
pub const DERIVED_CAPABILITY: &str =
    "gts.cf.bss.products.quota_capability.v1~cf.compute.quota_adapter.allocation_capability.v1~";
/// The package's example subject type.
pub const TENANT_TYPE: &str = "gts.cf.core.compute.tenant.v1~";
/// The package's example resource type.
pub const VM_TYPE: &str = "gts.cf.core.compute.vm.v1~";

/// The snapshot id every [`snapshot_of`] names.
pub const SNAPSHOT_ID: Uuid = Uuid::from_u128(0x9144_d0b7_3497_4db9_89e7_69c2_c5d8_f035);

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

fn package_file(name: &str) -> Vec<Value> {
    let text = match name {
        "units" => include_str!("../../../docs/quotas/schemas/quota-metric-unit.instances.json"),
        "policies" => include_str!("../../../docs/quotas/schemas/quota-policies.instances.json"),
        _ => include_str!("../../../docs/quotas/schemas/quota-capability-discovery.example.json"),
    };
    let value: Value = serde_json::from_str(text).expect("a package example is JSON");
    match value {
        Value::Array(items) => items,
        Value::Object(mut snapshot) => match snapshot.remove("items") {
            Some(Value::Array(items)) => items,
            other => panic!("the example snapshot has no items: {other:?}"),
        },
        other => panic!("unexpected package example: {other}"),
    }
}

/// A Products schema as the toolkit inventory seeds it into types-registry.
fn inventory_schema(type_id: &str) -> Value {
    toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .find(|entry| entry.type_id == type_id)
        .map_or_else(
            || panic!("{type_id} is not in the toolkit inventory"),
            |entry| serde_json::from_str((entry.schema_fn)().as_str()).expect("schema JSON"),
        )
}

fn object_schema(type_id: &str) -> Value {
    json!({"$id": format!("gts://{type_id}"), "$schema": DRAFT_07, "type": "object"})
}

/// Every type schema the quota fixtures hold: Products' own, as the toolkit inventory seeds them,
/// and the test adapter's derived capability schema and example subject and resource types.
#[must_use]
pub fn catalog_types() -> Vec<(String, Value)> {
    let mut types: Vec<(String, Value)> = [
        UNIT_TYPE,
        METRIC_TYPE,
        ACCOUNTING_POLICY_TYPE,
        SCOPE_POLICY_TYPE,
        CAPABILITY_TYPE,
        PLUGIN_BASE,
        DISCOVERY_PLUGIN_TYPE,
    ]
    .into_iter()
    .map(|id| (id.to_owned(), inventory_schema(id)))
    .collect();
    types.push((
        DERIVED_CAPABILITY.to_owned(),
        json!({
            "$id": format!("gts://{DERIVED_CAPABILITY}"),
            "$schema": DRAFT_07,
            "allOf": [
                {"$ref": format!("gts://{CAPABILITY_TYPE}")},
                {
                    "type": "object",
                    "properties": {
                        "properties": {
                            "type": "object",
                            "properties": {"reviewed_by": {"type": "string"}}
                        }
                    }
                }
            ]
        }),
    ));
    types.push((TENANT_TYPE.to_owned(), object_schema(TENANT_TYPE)));
    types.push((VM_TYPE.to_owned(), object_schema(VM_TYPE)));
    types
}

fn unit(id: &str, symbol: &str, canonical: &str, numerator: &str, denominator: &str) -> Value {
    json!({
        "id": id,
        "symbol": symbol,
        "canonical_unit_ref": canonical,
        "numerator": numerator,
        "denominator": denominator
    })
}

/// Every instance the test adapter registers: the package's example instances, the RAM family
/// (bytes canonical, GB and GiB by their own factors, the metric and its accounting policy) and
/// the discovery plugin instance.
#[must_use]
pub fn catalog_instances() -> Vec<(String, Value)> {
    let mut instances: Vec<(String, Value)> = package_file("units")
        .into_iter()
        .chain(package_file("policies"))
        .map(|content| {
            let id = content["id"].as_str().expect("an example id").to_owned();
            (id, content)
        })
        .collect();
    instances.push((
        BYTES_UNIT.to_owned(),
        unit(BYTES_UNIT, "B", BYTES_UNIT, "1", "1"),
    ));
    instances.push((
        GB_UNIT.to_owned(),
        unit(GB_UNIT, "GB", BYTES_UNIT, "1000000000", "1"),
    ));
    instances.push((
        GIB_UNIT.to_owned(),
        unit(GIB_UNIT, "GiB", BYTES_UNIT, "1073741824", "1"),
    ));
    instances.push((
        RAM_METRIC.to_owned(),
        json!({
            "id": RAM_METRIC,
            "name": "Allocated RAM",
            "display_name": {"en": "Memory"},
            "description": {"en": "Total memory allocated to the tenant's virtual machines."},
            "quota_kind": "allocation",
            "canonical_unit_ref": BYTES_UNIT,
            "properties": {}
        }),
    ));
    instances.push((
        ALLOCATED_RAM.to_owned(),
        json!({
            "id": ALLOCATED_RAM,
            "quantity_basis": "allocated_capacity",
            "aggregation": "sum",
            "description": {"en": "Counts allocated memory, including stopped virtual machines."}
        }),
    ));
    instances.push((
        TEST_PLUGIN.to_owned(),
        json!({"id": TEST_PLUGIN, "vendor": "test", "priority": 0, "properties": {}}),
    ));
    instances
}

/// The type schema an instance conforms to: its id without the last segment.
#[must_use]
pub fn type_of(instance_id: &str) -> String {
    gts::GtsId::try_new(instance_id)
        .expect("a fixture id is a GTS id")
        .get_type_id()
        .expect("an instance id has a type")
}

/// The fake loaded with every catalog entity.
#[must_use]
pub fn seed_catalog(registry: FakeQuotaRegistry) -> FakeQuotaRegistry {
    let mut registry = registry;
    for (id, content) in catalog_types() {
        registry = registry.with_type(&id, content);
    }
    for (id, content) in catalog_instances() {
        registry = registry.with_instance(&id, &type_of(&id), content);
    }
    registry
}

/// The platform's in-memory registry loaded with every catalog entity.
pub fn seed_mock(mock: &MockTypesRegistry) {
    for (id, content) in catalog_types().into_iter().chain(catalog_instances()) {
        mock.seed(&id, content);
    }
}

/// The package's example capability record: tenant vCPUs, 0 to 1024 by 1.
#[must_use]
pub fn vcpu_record() -> Value {
    package_file("snapshot")
        .into_iter()
        .next()
        .expect("the example snapshot has a record")
}

/// The example record with its own `id` and `capability_key`, one per `i`.
#[must_use]
pub fn vcpu_record_numbered(i: u32) -> Value {
    let mut record = vcpu_record();
    record["id"] = json!(Uuid::from_u128(0x5eed_0000 + u128::from(i)).to_string());
    record["capability_key"] = json!(format!("tenant-vcpu-{i}"));
    record
}

/// A RAM record whose bounds are stated in `unit_ref`: 0 to 64 by 1.
#[must_use]
pub fn ram_record_in(unit_ref: &str) -> Value {
    let mut record = vcpu_record();
    record["id"] = json!(Uuid::from_u128(0x0a11_0ca7_e000).to_string());
    record["capability_key"] = json!("tenant-ram");
    record["metric_ref"] = json!(RAM_METRIC);
    record["unit_ref"] = json!(unit_ref);
    record["accounting_policy_ref"] = json!(ALLOCATED_RAM);
    record["mapping_key"] = json!("tenant-ram-mapping-1");
    record["limits"] = json!({"min": "0", "step": "1", "max": "64"});
    record
}

/// The instant every [`snapshot_of`] observes.
#[must_use]
pub fn observed_at() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_794_571_200).expect("a fixed instant")
}

/// One page holding the whole snapshot of `records` at `revision`, with the right count and
/// digest and no next cursor; [`FakeDiscovery::paged`] cuts it into pages.
#[must_use]
pub fn snapshot_of(records: &[Value], revision: &str) -> QuotaDiscoveryPage {
    let item_count = u64::try_from(records.len()).expect("a small snapshot");
    QuotaDiscoveryPage {
        snapshot_id: SNAPSHOT_ID,
        source_revision: revision.to_owned(),
        observed_at: observed_at(),
        item_count,
        content_digest: snapshot_digest(SNAPSHOT_ID, revision, observed_at(), item_count, records)
            .expect("fixture records have ids"),
        items: records.to_vec(),
        next_cursor: None,
    }
}

// The port fake.

struct StoredInstance {
    type_id: String,
    version: u64,
    content: Value,
}

struct StoredType {
    version: u64,
    content: Value,
}

/// Where the test that holds the fake is, as [`FakeQuotaRegistry::begin`] marks it.
#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Outside,
    InsideTransaction,
}

#[derive(Default)]
struct RegistryState {
    instances: BTreeMap<String, StoredInstance>,
    types: BTreeMap<String, StoredType>,
    invalid: HashMap<String, String>,
    uncompilable: HashMap<String, String>,
    down: bool,
    armed: bool,
    phase: Phase,
    calls: usize,
    delay: Option<std::time::Duration>,
}

/// An in-memory [`QuotaRegistry`]: instances and type schemas with versions, a schema verdict per
/// type (valid unless told otherwise), an outage switch and a call counter. Clones share one state,
/// so a test keeps a handle on the fake it gave away.
#[derive(Clone, Default)]
pub struct FakeQuotaRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl FakeQuotaRegistry {
    fn state(&self) -> MutexGuard<'_, RegistryState> {
        self.state.lock().expect("the fake's lock")
    }

    /// Holds the instance `id` of `type_id`, at version 1.
    #[must_use]
    pub fn with_instance(self, id: &str, type_id: &str, content: Value) -> Self {
        self.state().instances.insert(
            id.to_owned(),
            StoredInstance {
                type_id: type_id.to_owned(),
                version: 1,
                content,
            },
        );
        self
    }

    /// Holds the type schema `id`, at version 1.
    #[must_use]
    pub fn with_type(self, id: &str, content: Value) -> Self {
        self.state().types.insert(
            id.to_owned(),
            StoredType {
                version: 1,
                content,
            },
        );
        self
    }

    /// Marks the held type schema `id` abstract (`x-gts-abstract: true`).
    #[must_use]
    pub fn abstract_type(self, id: &str) -> Self {
        let mut state = self.state();
        let stored = state.types.get_mut(id).expect("an abstract type is held");
        stored.content["x-gts-abstract"] = json!(true);
        drop(state);
        self
    }

    /// Validation against `type_id` answers `Invalid(reason)`.
    #[must_use]
    pub fn invalid_for(self, type_id: &str, reason: &str) -> Self {
        self.state()
            .invalid
            .insert(type_id.to_owned(), reason.to_owned());
        self
    }

    /// Validation against `type_id` answers `Uncompilable(reason)`: its live schema does not
    /// compile.
    #[must_use]
    pub fn uncompilable(self, type_id: &str, reason: &str) -> Self {
        self.state()
            .uncompilable
            .insert(type_id.to_owned(), reason.to_owned());
        self
    }

    /// Every call answers only after `delay`.
    #[must_use]
    pub fn slow(self, delay: std::time::Duration) -> Self {
        self.state().delay = Some(delay);
        self
    }

    /// Waits the delay [`Self::slow`] set, if any.
    async fn wait(&self) {
        let delay = self.state().delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }

    /// Every call answers an outage.
    #[must_use]
    pub fn outage(self) -> Self {
        self.go_down();
        self
    }

    /// Replaces the content of the held instance or type `id` and moves its version by one.
    pub fn bump(&self, id: &str, content: Value) {
        let mut state = self.state();
        if let Some(stored) = state.instances.get_mut(id) {
            stored.version += 1;
            stored.content = content;
        } else if let Some(stored) = state.types.get_mut(id) {
            stored.version += 1;
            stored.content = content;
        } else {
            panic!("{id} is not held");
        }
    }

    /// From now on every call answers an outage.
    pub fn go_down(&self) {
        self.state().down = true;
    }

    /// Arms the fake: once [`Self::begin`] marks a transaction's start, any call panics.
    pub fn panic_after_begin(&self) {
        self.state().armed = true;
    }

    /// Marks the start of a transaction; an armed fake panics on any call from here on.
    pub fn begin(&self) {
        self.state().phase = Phase::InsideTransaction;
    }

    /// Marks the end of the transaction [`Self::begin`] marked.
    pub fn end(&self) {
        self.state().phase = Phase::Outside;
    }

    /// The calls answered so far.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.state().calls
    }

    fn enter(
        &self,
        what: &str,
        id: &str,
    ) -> Result<MutexGuard<'_, RegistryState>, RegistryUnavailable> {
        let mut state = self.state();
        assert!(
            !(state.armed && state.phase == Phase::InsideTransaction),
            "types-registry asked inside a transaction: {what} {id}"
        );
        state.calls += 1;
        if state.down {
            return Err(RegistryUnavailable("the fake registry is down".to_owned()));
        }
        Ok(state)
    }
}

fn uuid_of(id: &str) -> Uuid {
    gts::GtsId::try_new(id).map_or_else(|_| Uuid::nil(), |g| g.to_uuid())
}

#[async_trait]
impl QuotaRegistry for FakeQuotaRegistry {
    async fn instance(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryInstance>, RegistryUnavailable> {
        self.wait().await;
        let state = self.enter("instance", id)?;
        Ok(state.instances.get(id).map(|stored| RegistryInstance {
            gts_id: id.to_owned(),
            uuid: uuid_of(id),
            type_id: stored.type_id.clone(),
            resource_version: stored.version,
            content: stored.content.clone(),
        }))
    }

    async fn type_schema(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryTypeSchema>, RegistryUnavailable> {
        self.wait().await;
        let state = self.enter("type schema", id)?;
        Ok(state.types.get(id).map(|stored| RegistryTypeSchema {
            gts_id: id.to_owned(),
            uuid: uuid_of(id),
            resource_version: stored.version,
            content: stored.content.clone(),
            is_abstract: stored.content.get("x-gts-abstract") == Some(&json!(true)),
        }))
    }

    async fn validate(
        &self,
        _ctx: &SecurityContext,
        type_id: &str,
        _payload: &Value,
    ) -> Result<RegistryVerdict, RegistryUnavailable> {
        self.wait().await;
        let state = self.enter("validate", type_id)?;
        if !state.types.contains_key(type_id) {
            return Ok(RegistryVerdict::UnknownSchema);
        }
        if let Some(reason) = state.uncompilable.get(type_id) {
            return Ok(RegistryVerdict::Uncompilable(reason.clone()));
        }
        Ok(state
            .invalid
            .get(type_id)
            .map_or(RegistryVerdict::Valid, |reason| {
                RegistryVerdict::Invalid(reason.clone())
            }))
    }
}

// The discovery fake.

enum Script {
    Paged {
        snapshot: QuotaDiscoveryPage,
        page_size: usize,
    },
    Slow {
        snapshot: QuotaDiscoveryPage,
        page_size: usize,
        delay: std::time::Duration,
    },
    Pages(Vec<QuotaDiscoveryPage>),
    Failing(String),
    Stalled,
}

struct DiscoveryState {
    script: Script,
    requests: Vec<QuotaDiscoveryRequest>,
    served: usize,
}

/// A discovery plugin client that answers a scripted snapshot and records every request. Clones
/// share one state.
#[derive(Clone)]
pub struct FakeDiscovery {
    state: Arc<Mutex<DiscoveryState>>,
}

impl FakeDiscovery {
    fn new(script: Script) -> Self {
        Self {
            state: Arc::new(Mutex::new(DiscoveryState {
                script,
                requests: Vec::new(),
                served: 0,
            })),
        }
    }

    /// Serves `snapshot` in pages of `page_size` items, chained by cursors `p1`, `p2`, ….
    #[must_use]
    pub fn paged(snapshot: QuotaDiscoveryPage, page_size: usize) -> Self {
        Self::new(Script::Paged {
            snapshot,
            page_size,
        })
    }

    /// [`Self::paged`], each page answered after `delay`.
    #[must_use]
    pub fn slow(
        snapshot: QuotaDiscoveryPage,
        page_size: usize,
        delay: std::time::Duration,
    ) -> Self {
        Self::new(Script::Slow {
            snapshot,
            page_size,
            delay,
        })
    }

    /// Answers the `n`th call with `pages[n]`, whatever its cursor; a call past the last page
    /// fails.
    #[must_use]
    pub fn scripted(pages: Vec<QuotaDiscoveryPage>) -> Self {
        Self::new(Script::Pages(pages))
    }

    /// Fails every call with `text` as the adapter's own explanation.
    #[must_use]
    pub fn failing(text: &str) -> Self {
        Self::new(Script::Failing(text.to_owned()))
    }

    /// Never answers.
    #[must_use]
    pub fn stalled() -> Self {
        Self::new(Script::Stalled)
    }

    /// Every request received, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<QuotaDiscoveryRequest> {
        self.state.lock().expect("the fake's lock").requests.clone()
    }

    /// The delay before the next answer.
    fn delay(&self) -> Option<std::time::Duration> {
        match &self.state.lock().expect("the fake's lock").script {
            Script::Slow { delay, .. } => Some(*delay),
            _ => None,
        }
    }

    fn answer(&self, request: QuotaDiscoveryRequest) -> Option<Result<QuotaDiscoveryPage, String>> {
        let mut state = self.state.lock().expect("the fake's lock");
        let cursor = request.cursor.clone();
        state.requests.push(request);
        let served = state.served;
        state.served += 1;
        match &state.script {
            Script::Paged {
                snapshot,
                page_size,
            }
            | Script::Slow {
                snapshot,
                page_size,
                ..
            } => {
                let page = cursor.map_or(0, |c| {
                    c.trim_start_matches('p')
                        .parse::<usize>()
                        .expect("a fake cursor")
                });
                let chunks: Vec<&[Value]> = snapshot.items.chunks(*page_size).collect();
                let items = chunks.get(page).map_or_else(Vec::new, |c| c.to_vec());
                let next_cursor = (page + 1 < chunks.len()).then(|| format!("p{}", page + 1));
                Some(Ok(QuotaDiscoveryPage {
                    items,
                    next_cursor,
                    ..snapshot.clone()
                }))
            }
            Script::Pages(pages) => Some(
                pages
                    .get(served)
                    .cloned()
                    .ok_or_else(|| "the script has no more pages".to_owned()),
            ),
            Script::Failing(text) => Some(Err(text.clone())),
            Script::Stalled => None,
        }
    }
}

#[async_trait]
impl ProductsQuotaDiscoveryPluginClientV1 for FakeDiscovery {
    async fn list_capabilities(
        &self,
        _ctx: &SecurityContext,
        request: QuotaDiscoveryRequest,
    ) -> Result<QuotaDiscoveryPage, CanonicalError> {
        if let Some(delay) = self.delay() {
            tokio::time::sleep(delay).await;
        }
        match self.answer(request) {
            Some(Ok(page)) => Ok(page),
            Some(Err(text)) => Err(CanonicalError::service_unavailable()
                .with_detail(text)
                .create()),
            None => std::future::pending().await,
        }
    }
}

/// A [`QuotaDiscoveryResolver`] with one plugin client, or none.
pub struct FakeResolver {
    client: Option<(String, FakeDiscovery)>,
}

impl FakeResolver {
    /// `plugin_id` resolves to `discovery`; any other id is closed.
    #[must_use]
    pub fn client(plugin_id: &str, discovery: FakeDiscovery) -> Self {
        Self {
            client: Some((plugin_id.to_owned(), discovery)),
        }
    }

    /// Every plugin instance is closed.
    #[must_use]
    pub fn closed() -> Self {
        Self { client: None }
    }
}

impl QuotaDiscoveryResolver for FakeResolver {
    fn resolve(&self, plugin_instance_id: &str) -> ResolvedDiscovery {
        match &self.client {
            Some((id, discovery)) if id == plugin_instance_id => {
                ResolvedDiscovery::Client(Arc::new(discovery.clone()))
            }
            _ => ResolvedDiscovery::Closed,
        }
    }
}

// The endpoint fixtures.

/// What the quota endpoints' fixture runs against: a registry fake, a discovery resolver and the
/// intake's settings.
pub struct QuotaFakes {
    pub registry: FakeQuotaRegistry,
    pub discovery: FakeResolver,
    pub settings: QuotaSettings,
}

/// The configuration's defaults: pages of 50, 5 s a page, at most 10 000 records, and 25 s
/// for a whole discovery.
#[must_use]
pub fn default_settings() -> QuotaSettings {
    crate::config::ProductsConfig::default().quota_settings()
}

impl Default for QuotaFakes {
    /// The seeded registry, no adapter (every target's discovery is closed) and the default
    /// settings.
    fn default() -> Self {
        Self {
            registry: seed_catalog(FakeQuotaRegistry::default()),
            discovery: FakeResolver::closed(),
            settings: default_settings(),
        }
    }
}

impl QuotaFakes {
    /// The seeded registry, and the test plugin answering `records` at `revision` in pages of 50.
    #[must_use]
    pub fn discovering(records: &[Value], revision: &str) -> Self {
        Self::answering(FakeDiscovery::paged(snapshot_of(records, revision), 50))
    }

    /// The seeded registry, and the test plugin answering through `discovery`.
    #[must_use]
    pub fn answering(discovery: FakeDiscovery) -> Self {
        Self {
            discovery: FakeResolver::client(TEST_PLUGIN, discovery),
            ..Self::default()
        }
    }
}

/// What the PDP was asked: the resource type, the action and the tenant mode.
pub type PdpQuestion = (
    String,
    String,
    Option<authz_resolver_sdk::models::TenantMode>,
);

/// A PDP that allows the listed tenants, each under `In([its own tenant])`, refuses every other
/// subject, and records what it is asked: the resource type, the action and the tenant mode.
/// Narrowed, it also grants only the named rows: `In([the rows])` over the resource id.
pub struct QuotaPdp {
    allowed: Vec<Uuid>,
    asked: Mutex<Vec<PdpQuestion>>,
    rows: Mutex<Option<Vec<Uuid>>>,
}

impl QuotaPdp {
    /// Every request asked so far, in order.
    #[must_use]
    pub fn asked(&self) -> Vec<PdpQuestion> {
        self.asked.lock().expect("the fake's lock").clone()
    }

    /// From now on every grant is `owner_tenant_id = T AND id IN (rows)`.
    pub fn narrow_to(&self, rows: &[Uuid]) {
        *self.rows.lock().expect("the fake's lock") = Some(rows.to_vec());
    }
}

/// A PDP that fails to evaluate: an unreachable PDP.
pub struct FailingResolver;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for FailingResolver {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        _req: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::models::EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("pdp unreachable".to_owned()).create())
    }
}

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for QuotaPdp {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::models::EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
        use authz_resolver_sdk::models::{EvaluationResponse, EvaluationResponseContext};
        self.asked.lock().expect("the fake's lock").push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
            request
                .context
                .tenant_context
                .as_ref()
                .map(|t| t.mode.clone()),
        ));
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(Value::as_str)
            .and_then(|text| Uuid::parse_str(text).ok())
            .filter(|tenant| self.allowed.contains(tenant));
        let Some(tenant) = tenant else {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            });
        };
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![tenant],
        ))];
        if let Some(rows) = self.rows.lock().expect("the fake's lock").clone() {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                rows,
            )));
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}

/// The quota endpoints mounted over one test database, with the fakes they were built with.
pub struct QuotaApp {
    pub app: axum::Router,
    pub dsn: crate::test_support::TestDsn,
    pub registry: FakeQuotaRegistry,
    pub state: Arc<crate::api::rest::quota::QuotaState>,
    pub pdp: Arc<QuotaPdp>,
    /// Keeps the state's outbox running for the app's life.
    _outbox: axum::Router,
}

/// The quota endpoints for one tenant: [`quota_app_for`] with that tenant allowed.
pub async fn quota_app(tenant: Uuid, fakes: QuotaFakes) -> QuotaApp {
    quota_app_for(&[tenant], fakes).await
}

/// The quota endpoints over one database whose PDP allows each of `tenants` in its own tenant and
/// refuses every other subject (none listed: it refuses everyone), with the fakes' settings.
pub async fn quota_app_for(tenants: &[Uuid], fakes: QuotaFakes) -> QuotaApp {
    let (db, _, _, dsn) = crate::test_support::test_db().await;
    quota_app_on(tenants, fakes, db, dsn).await
}

/// [`quota_app`] over a database whose statements the recorder records, cleared after the
/// fixture's own writes.
pub async fn quota_app_recorded(
    tenant: Uuid,
    fakes: QuotaFakes,
) -> (QuotaApp, toolkit_db::test_support::QueryRecorder) {
    let (db, _, _, dsn, recorder) = crate::test_support::recorded_test_db().await;
    let q = quota_app_on(&[tenant], fakes, db, dsn).await;
    recorder.clear();
    (q, recorder)
}

async fn quota_app_on(
    tenants: &[Uuid],
    fakes: QuotaFakes,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    dsn: crate::test_support::TestDsn,
) -> QuotaApp {
    let pdp = Arc::new(QuotaPdp {
        allowed: tenants.to_vec(),
        asked: Mutex::new(Vec::new()),
        rows: Mutex::new(None),
    });
    let tenant = Uuid::new_v4();
    let (outbox, api) = crate::test_support::rest_app_on_db(
        tenant,
        |_, _| axum::Router::new(),
        crate::test_support::resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let state = Arc::new(crate::api::rest::quota::QuotaState {
        api,
        registry: Arc::new(fakes.registry.clone()),
        discovery: Arc::new(fakes.discovery),
        settings: fakes.settings,
        approve_hook: None,
    });
    let app =
        crate::api::rest::quota::router(state.clone(), &toolkit::api::OpenApiRegistryImpl::new())
            .layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
                pdp.clone(),
            )));
    QuotaApp {
        app,
        dsn,
        registry: fakes.registry,
        state,
        pdp,
        _outbox: outbox,
    }
}

/// One request to `app` as `ctx`, with an optional `Idempotency-Key`, an optional `If-Match` and
/// a raw body (JSON or not).
pub async fn send(
    app: &axum::Router,
    ctx: &SecurityContext,
    method: axum::http::Method,
    uri: &str,
    headers: (Option<&str>, Option<&str>),
    body: Option<String>,
) -> axum::response::Response {
    use tower::ServiceExt;
    let (key, if_match) = headers;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .extension(ctx.clone())
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    if let Some(tag) = if_match {
        request = request.header("If-Match", tag);
    }
    let body = body.map_or_else(axum::body::Body::empty, axum::body::Body::from);
    app.clone()
        .oneshot(request.body(body).expect("a request"))
        .await
        .expect("an answer")
}

/// The status, the error code and the resource type a refusal answers.
pub async fn refusal(response: axum::response::Response) -> (u16, String, Value) {
    let status = response.status().as_u16();
    let body = crate::test_support::body_json(response).await;
    let code = crate::test_support::problem_code(&body);
    (status, code, body["context"]["resource_type"].clone())
}

impl QuotaApp {
    /// A target of `tenant` binding the test plugin under the key `region-1`.
    pub async fn create_target(&self, tenant: Uuid) -> Uuid {
        let response = crate::test_support::post(
            &self.app,
            tenant,
            crate::api::rest::quota::TARGETS,
            json!({
                "plugin_instance_id": TEST_PLUGIN,
                "target_key": "region-1",
                "name": "Region 1"
            }),
        )
        .await;
        assert_eq!(response.status(), 201, "a target is created");
        let body = crate::test_support::body_json(response).await;
        body["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .expect("the target answers its id")
    }

    /// The rows `tail` (`<table> [WHERE …]`) counts.
    pub async fn count(&self, tail: &str) -> i64 {
        crate::test_support::raw_i64(&self.dsn, &format!("SELECT COUNT(*) AS v FROM {tail}")).await
    }

    /// A target of `tenant` binding the test plugin under a fresh key.
    pub async fn fresh_target(&self, tenant: Uuid) -> Uuid {
        let key = format!("region-{}", Uuid::new_v4().simple());
        let response = crate::test_support::post(
            &self.app,
            tenant,
            crate::api::rest::quota::TARGETS,
            json!({"plugin_instance_id": TEST_PLUGIN, "target_key": key, "name": "Region"}),
        )
        .await;
        assert_eq!(response.status(), 201, "a target is created");
        id_of(&crate::test_support::body_json(response).await)
    }

    /// A manual candidate of `record` on `target`, authored by `author`: its id and its `ETag`.
    pub async fn manual_candidate_on(
        &self,
        author: &SecurityContext,
        target: Uuid,
        record: Value,
    ) -> (Uuid, String) {
        let response = crate::test_support::request_as(
            &self.app,
            author,
            axum::http::Method::POST,
            crate::api::rest::quota::CANDIDATES,
            Some(json!({"target_id": target, "record": record})),
            None,
        )
        .await;
        assert_eq!(response.status(), 201, "a manual candidate is created");
        let etag = etag_of(&response);
        (id_of(&crate::test_support::body_json(response).await), etag)
    }

    /// A target and a manual candidate of `record`, both by the tenant's user: the candidate's id
    /// and its `ETag`.
    pub async fn manual_candidate(&self, tenant: Uuid, record: Value) -> (Uuid, String) {
        let target = self.fresh_target(tenant).await;
        self.manual_candidate_on(&crate::test_support::tenant_user(tenant), target, record)
            .await
    }

    /// The tenant's user submits `candidate` for `sku` at `sku_version`, versioning `definition`
    /// when named: the record's own unit, both bases and conditions in English and Russian. The
    /// candidate's id and its new `ETag`.
    pub async fn submit(
        &self,
        tenant: Uuid,
        candidate: &(Uuid, String),
        record: &Value,
        sku: Uuid,
        sku_version: i64,
        definition: Option<Uuid>,
    ) -> (Uuid, String) {
        let response = crate::test_support::request(
            &self.app,
            tenant,
            axum::http::Method::POST,
            &format!(
                "{}/{}/submit",
                crate::api::rest::quota::CANDIDATES,
                candidate.0
            ),
            Some(submit_body(record, sku, sku_version, definition)),
            Some(&candidate.1),
        )
        .await;
        let status = response.status();
        let etag = etag_of(&response);
        let body = crate::test_support::body_json(response).await;
        assert_eq!(status, 200, "the candidate is submitted: {body}");
        (candidate.0, etag)
    }

    /// A SKU of `tenant` with versions 1 … `sku_version`, and a manual candidate of `record`
    /// authored and submitted against it by the tenant's user: the id and the `ETag`.
    pub async fn submitted_candidate(
        &self,
        tenant: Uuid,
        record: Value,
        sku_version: i64,
    ) -> (Uuid, String) {
        self.submitted_candidate_by(
            tenant,
            &crate::test_support::tenant_user(tenant),
            record,
            sku_version,
        )
        .await
    }

    /// [`Self::submitted_candidate`], the record authored by `author` and submitted by the
    /// tenant's user.
    pub async fn submitted_candidate_by(
        &self,
        tenant: Uuid,
        author: &SecurityContext,
        record: Value,
        sku_version: i64,
    ) -> (Uuid, String) {
        let sku = seed_sku_version(&self.dsn, tenant, &fresh_code(), sku_version).await;
        let target = self.fresh_target(tenant).await;
        let candidate = self
            .manual_candidate_on(author, target, record.clone())
            .await;
        self.submit(tenant, &candidate, &record, sku, sku_version, None)
            .await
    }
}

/// The content the test adapter registers under `id`, an instance or a type.
#[must_use]
pub fn registered(id: &str) -> Value {
    catalog_instances()
        .into_iter()
        .chain(catalog_types())
        .find(|(held, _)| held == id)
        .map_or_else(
            || panic!("the test adapter registers no {id}"),
            |(_, content)| content,
        )
}

fn evidence_of(id: &str, content: &Value) -> crate::domain::quota::evidence::Evidence {
    crate::domain::quota::evidence::Evidence {
        gts_id: id.to_owned(),
        uuid: uuid_of(id),
        resource_version: 1,
        digest: bss_products_sdk::quota::digest::content_digest(content),
    }
}

/// What intake keeps of `record` and what a submit pins: the evidence of every entity it names
/// (its instances, its subject and resource types), and the instances with their content.
#[must_use]
pub fn judged_entities(
    record: &Value,
) -> (
    crate::domain::quota::evidence::EvidenceSet,
    crate::domain::quota::review::Pinned,
) {
    use crate::domain::quota::review::{Pinned, PinnedEntity};
    let text = |name: &str| record[name].as_str().expect("a reference").to_owned();
    let metric = text("metric_ref");
    let canonical = registered(&metric)["canonical_unit_ref"]
        .as_str()
        .expect("a canonical unit")
        .to_owned();
    let mut evidence = crate::domain::quota::evidence::EvidenceSet::default();
    let mut pinned = Pinned::default();
    for id in [
        metric,
        text("unit_ref"),
        canonical,
        text("accounting_policy_ref"),
        text("scope_policy_ref"),
    ] {
        let content = registered(&id);
        let entity = evidence_of(&id, &content);
        evidence.insert(entity.clone());
        pinned.insert(PinnedEntity {
            evidence: entity,
            content,
        });
    }
    let types = std::iter::once(text("subject_type_ref")).chain(
        record["resource_type_refs"]
            .as_array()
            .expect("resource types")
            .iter()
            .map(|id| id.as_str().expect("a type").to_owned()),
    );
    for id in types {
        evidence.insert(evidence_of(&id, &registered(&id)));
    }
    (evidence, pinned)
}

/// A proposal of `record` for `sku` at `sku_version`, versioning `definition` when named: the
/// record's own unit with its bounds, both bases, and conditions in English and Russian.
#[must_use]
pub fn proposal_of(
    record: &Value,
    sku: Uuid,
    sku_version: u64,
    definition: Option<Uuid>,
) -> crate::domain::quota::review::Proposal {
    use crate::domain::quota::review::{AllowedUnit, Proposal};
    let unit_ref = record["unit_ref"].as_str().expect("a unit");
    let content = registered(unit_ref);
    let limits = &record["limits"];
    let bound = |name: &str| limits[name].as_str().map(str::to_owned);
    Proposal {
        sku_id: sku,
        sku_version: bss_products_sdk::quota::catalog::QuotaVersion::new(sku_version)
            .expect("a version"),
        definition_id: definition,
        allowed_units: vec![AllowedUnit {
            unit: serde_json::from_value(content.clone()).expect("a unit"),
            evidence: evidence_of(unit_ref, &content),
            min: bound("min").expect("a min"),
            step: bound("step").expect("a step"),
            max: bound("max"),
        }],
        supported_bases: vec![
            bss_products_sdk::quota::catalog::QuotaBasis::Fixed,
            bss_products_sdk::quota::catalog::QuotaBasis::PerPlanUnit,
        ],
        profile_conditions: BTreeMap::from([
            (
                "en".to_owned(),
                "Allocated vCPUs count while a VM is stopped.".to_owned(),
            ),
            (
                "ru".to_owned(),
                "Vydelennye vCPU uchityvayutsya, poka VM ostanovlena.".to_owned(),
            ),
        ]),
    }
}

/// A submitted manual candidate row of `record` with `proposal`, as the store reads it back,
/// staged on `target` by `author` and submitted by `submitter`.
#[must_use]
pub fn submitted_row(
    record: Value,
    proposal: &crate::domain::quota::review::Proposal,
    target: Uuid,
    (author, submitter): (Uuid, Uuid),
) -> crate::domain::quota::QuotaCandidate {
    let (evidence, pinned) = judged_entities(&record);
    let record_id = Uuid::parse_str(record["id"].as_str().expect("an id")).expect("a UUID");
    let at = crate::infra::storage::stored_now();
    let record_digest = bss_products_sdk::quota::digest::record_digest(&record);
    crate::domain::quota::QuotaCandidate {
        id: Uuid::new_v4(),
        target_id: target,
        origin: crate::domain::quota::CandidateOrigin::Manual,
        snapshot_row_id: None,
        record_id,
        capability_key: record["capability_key"].as_str().expect("a key").to_owned(),
        record_json: record,
        record_digest,
        evidence_json: evidence.to_json(),
        state: crate::domain::quota::CandidateState::Submitted,
        version: 2,
        proposal_json: Some(crate::domain::quota::review::proposal_json(proposal)),
        proposal_evidence_json: Some(pinned.to_json()),
        created_by: author,
        created_at: at,
        submitted_by: Some(submitter),
        submitted_at: Some(at),
        submit_note: None,
        decided_by: None,
        decided_at: None,
        decided_note: None,
        definition_id: None,
        definition_version: None,
    }
}

impl QuotaApp {
    /// `record` authored and submitted against `sku` at `sku_version` by the tenant's user, and
    /// approved by another subject of the tenant: the published definition id and version.
    pub async fn approved(
        &self,
        tenant: Uuid,
        record: Value,
        sku: Uuid,
        sku_version: i64,
    ) -> (Uuid, bss_products_sdk::quota::catalog::QuotaVersion) {
        let target = self.fresh_target(tenant).await;
        self.approved_on(tenant, target, record, sku, sku_version)
            .await
    }

    /// [`QuotaApp::approved`] on a given adapter target (`manual_candidate_on`), so that several
    /// definitions share one target, as the rules of one logical binding must (P-D-276).
    pub async fn approved_on(
        &self,
        tenant: Uuid,
        target: Uuid,
        record: Value,
        sku: Uuid,
        sku_version: i64,
    ) -> (Uuid, bss_products_sdk::quota::catalog::QuotaVersion) {
        let candidate = self
            .manual_candidate_on(
                &crate::test_support::tenant_user(tenant),
                target,
                record.clone(),
            )
            .await;
        let candidate = self
            .submit(tenant, &candidate, &record, sku, sku_version, None)
            .await;
        let response = crate::test_support::request_as(
            &self.app,
            &crate::test_support::authed_ctx(tenant),
            axum::http::Method::POST,
            &format!(
                "{}/{}/approve",
                crate::api::rest::quota::CANDIDATES,
                candidate.0
            ),
            Some(json!({})),
            Some(&candidate.1),
        )
        .await;
        let status = response.status();
        let body = crate::test_support::body_json(response).await;
        assert_eq!(status, 200, "the candidate is approved: {body}");
        let definition = &body["definition"];
        (
            Uuid::parse_str(definition["definition_id"].as_str().expect("an id")).expect("a UUID"),
            definition["version"]
                .as_str()
                .and_then(|v| v.parse().ok())
                .expect("a version"),
        )
    }

    /// Write availability revision 2 `enabled` for one version, as the qualification of the quotas
    /// package's feature 5 will: through `quota_catalog_repo::insert_availability`, copying the
    /// revision-1 write of the approval (`inst-quota-availability` in `api/rest/quota/review.rs`)
    /// with `revision: 2`, the enabled state and no reason. Test only: no endpoint enables a
    /// version.
    pub async fn enable(
        &self,
        tenant: Uuid,
        definition_id: Uuid,
        version: bss_products_sdk::quota::catalog::QuotaVersion,
    ) {
        let (db, scope) = crate::test_support::repo_connection(&self.dsn, tenant).await;
        crate::infra::storage::repo::quota_catalog_repo::insert_availability(
            &db.conn().expect("a connection"),
            &scope.tenant_only(),
            tenant,
            &crate::domain::quota::Availability {
                definition_id,
                version,
                revision: bss_products_sdk::quota::catalog::QuotaVersion::new(2)
                    .expect("a version"),
                state: bss_products_sdk::quota::catalog::QuotaAvailabilityState::Enabled,
                reason_code: None,
                qualification: None,
                observed_at: crate::infra::storage::stored_now(),
                decided_by: Uuid::new_v4(),
            },
        )
        .await
        .expect("availability revision 2");
    }
}

/// A SKU code no other fixture SKU of the tenant holds.
#[must_use]
pub fn fresh_code() -> String {
    format!("SKU-{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// The submit body of `record`: its own unit, both bases and conditions in English and Russian.
#[must_use]
pub fn submit_body(record: &Value, sku: Uuid, sku_version: i64, definition: Option<Uuid>) -> Value {
    json!({
        "sku_id": sku,
        "sku_version": sku_version.to_string(),
        "definition_id": definition,
        "allowed_unit_refs": [record["unit_ref"]],
        "supported_bases": ["fixed", "per_plan_unit"],
        "profile_conditions": {
            "en": "Allocated vCPUs count while a VM is stopped.",
            "ru": "Vydelennye vCPU uchityvayutsya, poka VM ostanovlena."
        }
    })
}

/// The `id` an answer names.
#[must_use]
pub fn id_of(body: &Value) -> Uuid {
    body["id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("the answer names its id")
}

/// The `ETag` an answer carries.
#[must_use]
pub fn etag_of(response: &axum::response::Response) -> String {
    response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .expect("the answer carries an ETag")
        .to_owned()
}

/// A SKU of `tenant` coded `code`, with versions 1 … `versions`, written through the
/// repositories.
pub async fn seed_sku_version(dsn: &str, tenant: Uuid, code: &str, versions: i64) -> Uuid {
    use crate::infra::storage::repo::{append_version, insert_sku};
    let (db, scope) = crate::test_support::repo_connection(dsn, tenant).await;
    let conn = db.conn().expect("a connection");
    let sku = insert_sku(
        &conn,
        &scope,
        tenant,
        crate::domain::sku::NewSku {
            code: code.to_owned(),
            name: code.to_owned(),
            r#type: bss_products_sdk::models::SkuType::Recurring,
            category_id: None,
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        crate::test_support::tenant_user(tenant).subject_id(),
        crate::infra::storage::stored_now(),
    )
    .await
    .expect("a SKU");
    let content = bss_products_sdk::models::SkuContent::from(&sku);
    let day = crate::test_support::utc(2026, 11, 1, 0, 0, 0).date();
    for version in 1..=versions {
        append_version(
            &conn,
            &scope,
            tenant,
            sku.id,
            version,
            day,
            &content,
            crate::infra::storage::stored_now(),
        )
        .await
        .expect("a SKU version");
    }
    sku.id
}

/// The log lines of the current thread, captured while the guard [`LogCapture::install`] answers
/// lives: each event's fields as `name=value`, its message first and its target last
/// (`target=…`). A `#[tokio::test]` runs on one thread, so it sees every line its request writes.
#[derive(Clone, Default)]
pub struct LogCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl LogCapture {
    /// Capture this thread's log lines until the guard drops.
    #[must_use]
    pub fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let capture = Self::default();
        let guard = tracing::subscriber::set_default(Capturing(capture.clone()));
        (capture, guard)
    }

    /// Every line captured so far.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("the capture's lock").clone()
    }
}

struct Capturing(LogCapture);

/// An event's message, then its other fields as `name=value`.
#[derive(Default)]
struct Fields(Vec<String>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert(0, format!("{value:?}"));
        } else {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }
}

impl tracing::Subscriber for Capturing {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        fields
            .0
            .push(format!("target={}", event.metadata().target()));
        self.0
            .lines
            .lock()
            .expect("the capture's lock")
            .push(fields.0.join(" "));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

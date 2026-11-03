//! In-memory test client for both registry contracts (`test-util`).
//!
//! Seed entities with [`MockTypesRegistry::seed`], [`Seed`] or
//! [`MockTypesRegistry::seed_inventory`]. Mutations advance versions, dry runs discard
//! changes, and idempotency keys replay operations. Type Schema materializations are
//! resolved by `gts-rust`; tombstones retain them, and unresolvable document reads fail.
//!
//! Use [`Fault`] for delays and errors, [`MockTypesRegistry::reject`] and
//! [`MockTypesRegistry::depends_on`] for admission rules, and [`ProtocolFault`] for
//! malformed responses. [`MockTypesRegistry::strict`] panics on unexpected calls.
//! Inspect calls and writes through the client's recording methods.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::contract::TypesRegistryApi;
use crate::field;
use crate::gts::{OperationResource, TypeResource};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason as Reason, context};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionItemResult,
    DeletionOperation, DeletionOutcome, Entity, EntityField, EntityKey, EntityKind, EntityLookup,
    FieldSelection, IdempotencyKey, LifecycleFilter, LifecycleStatus, ListEntitiesRequest,
    ListEntitiesResponse, Operation, OperationStatus, Origin, Provenance, RegisterEntitiesRequest,
    RegisterItem, RegistrationItemResult, RegistrationOperation, RegistrationOutcome, Validator,
};
use crate::reason::aborted;

pub mod conformance;

// configuration.

/// A contract method, for [`Fault`] selectors, [`MockTypesRegistry::calls`] and strict mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Call {
    BatchGet,
    List,
    Register,
    Delete,
    GetOperation,
}

#[derive(Debug, Clone, Copy)]
enum When {
    Always,
    Nth(u32),
    First(u32),
    From(u32),
}

impl When {
    fn admits(self, seen: u32) -> bool {
        match self {
            Self::Always => true,
            Self::Nth(n) => seen == n,
            Self::First(n) => seen <= n,
            Self::From(n) => seen >= n,
        }
    }
}

/// A rule that delays and/or fails matching calls; install it with
/// [`MockTypesRegistry::inject`]. Counts are 1-based over the calls the rule matches since it
/// was installed. Every matching rule counts a call; the first installed one that admits it
/// applies.
#[derive(Debug, Clone)]
pub struct Fault {
    call: Option<Call>,
    key: Option<EntityKey>,
    when: When,
    delay: Duration,
    failure: Option<CanonicalError>,
}

impl Fault {
    /// Calls of one method.
    #[must_use]
    pub fn on(call: Call) -> Self {
        Self {
            call: Some(call),
            ..Self::any()
        }
    }

    /// Calls of every method.
    #[must_use]
    pub fn any() -> Self {
        Self {
            call: None,
            key: None,
            when: When::Always,
            delay: Duration::ZERO,
            failure: None,
        }
    }

    /// Only calls naming `key`: a batch read asking for it, a registration or deletion carrying
    /// it. An identifier and its Registry Reference name the same entity.
    #[must_use]
    pub fn key(mut self, key: impl Into<EntityKey>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Only the `n`th matching call.
    #[must_use]
    pub fn nth(mut self, n: u32) -> Self {
        self.when = When::Nth(n);
        self
    }

    /// The first `n` matching calls.
    #[must_use]
    pub fn first(mut self, n: u32) -> Self {
        self.when = When::First(n);
        self
    }

    /// The `n`th matching call and every later one.
    #[must_use]
    pub fn from(mut self, n: u32) -> Self {
        self.when = When::From(n);
        self
    }

    /// The call answers after `delay` (on the tokio clock, so a paused clock skips it).
    #[must_use]
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// The call fails with `error`, after the delay if any.
    #[must_use]
    pub fn fail(mut self, error: CanonicalError) -> Self {
        self.failure = Some(error);
        self
    }

    fn matches(&self, call: Call, keys: &[EntityKey]) -> bool {
        self.call.is_none_or(|c| c == call)
            && self
                .key
                .as_ref()
                .is_none_or(|key| keys.iter().any(|k| same_entity(k, key)))
    }
}

/// Defects in otherwise successful responses, for the SDK's protocol tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolFault {
    /// Batch reads answer no key at all.
    DropAnswers,
    /// Batch reads answer `Found` without the selected `origin`.
    StripOrigin,
    /// Batch reads answer `Found` with the opposite `kind` from what the identifier names.
    MislabelKind,
    /// A list page continued from a cursor answers that same cursor again.
    RepeatListCursor,
    /// Completed operations report no items.
    DropOperationItems,
    /// Committing registrations report their successes as dry-run previews.
    PreviewSuccesses,
    /// The next `n` accepted submissions fail their read back.
    LoseReadBacks(u32),
}

#[derive(Debug, Default)]
struct Protocol {
    /// Every installed fault but `LoseReadBacks`, which counts down in `lose_read_backs`.
    defects: HashSet<ProtocolFault>,
    lose_read_backs: u32,
}

impl Protocol {
    fn has(&self, fault: ProtocolFault) -> bool {
        self.defects.contains(&fault)
    }
}

/// An entity stored directly, bypassing admission. Active at version 1 unless told otherwise.
#[derive(Debug, Clone)]
pub struct Seed {
    gts_id: gts::GtsId,
    content: serde_json::Value,
    version: u64,
    lifecycle: LifecycleStatus,
}

impl Seed {
    /// # Panics
    /// If `gts_id` is not a valid GTS identifier.
    #[must_use]
    pub fn new(gts_id: &str, content: serde_json::Value) -> Self {
        Self {
            gts_id: gts::GtsId::try_new(gts_id).expect("a seed needs a valid GTS identifier"),
            content,
            version: 1,
            lifecycle: LifecycleStatus::Active,
        }
    }

    /// # Panics
    /// If `version` is zero: stored versions start at 1.
    #[must_use]
    pub fn version(mut self, version: u64) -> Self {
        assert!(version > 0, "stored versions start at 1");
        self.version = version;
        self
    }

    /// A tombstone. A Type Schema's documents are those it resolves to on its first read.
    #[must_use]
    pub fn deleted(mut self) -> Self {
        self.lifecycle = LifecycleStatus::Deleted;
        self
    }
}

// state.

/// A Type Schema's resolved documents, or why it does not resolve.
type Resolution = Result<Documents, String>;

#[derive(Debug, Clone, PartialEq)]
struct Documents {
    resolved_schema: serde_json::Value,
    effective_traits: serde_json::Value,
    effective_traits_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
struct Stored {
    content: serde_json::Value,
    resource_version: u64,
    lifecycle: LifecycleStatus,
    /// A tombstone's documents, fixed by its first successful resolution (on delete or on its
    /// first read): refresh skips deleted entities.
    frozen: Option<Documents>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    Registration,
    Deletion,
}

#[derive(Debug, Clone)]
enum Candidate {
    Register {
        gts_id: gts::GtsId,
        content: serde_json::Value,
        expected: Option<u64>,
    },
    Delete {
        key: EntityKey,
        expected: u64,
    },
}

/// A decided candidate, as if committed; a dry run renders `Succeeded` without its version.
#[derive(Debug, Clone)]
enum Decided {
    Succeeded(u64),
    Unchanged(u64),
    Failed(AdmissionFailure),
}

#[derive(Debug)]
struct FakeOperation {
    kind: OperationKind,
    candidates: Vec<Candidate>,
    /// Decided against a copy of the state, which is then discarded.
    dry_run: bool,
    /// Polls left before the operation completes.
    polls_left: u32,
    /// Decided outcomes, once completed.
    outcomes: Option<Vec<Decided>>,
}

/// How admission decides, besides versions: explicit refusals and dependencies.
#[derive(Default)]
struct Admission {
    rejections: HashMap<String, Reason>,
    dependencies: Vec<(String, String)>,
}

struct Strict {
    allowed: HashSet<Call>,
    absent: Vec<EntityKey>,
}

#[derive(Default)]
struct State {
    entities: BTreeMap<String, Stored>,
    operations: HashMap<Uuid, FakeOperation>,
    keys: HashMap<String, (String, Uuid)>,
    submissions: Vec<(IdempotencyKey, RegisterEntitiesRequest)>,
    /// Writes another publisher makes just before the next registration.
    races: Vec<(String, serde_json::Value)>,
    admission: Admission,
    /// Installed rules with the number of calls each has matched.
    rules: Vec<(Fault, u32)>,
    protocol: Protocol,
    strict: Option<Strict>,
    calls: HashMap<Call, u32>,
    reads: Vec<EntityKey>,
    resolver: Resolver,
}

/// In-memory [`PlatformTypesRegistryApi`] and [`TypesRegistryApi`]; see the module docs.
pub struct MockTypesRegistry {
    state: Mutex<State>,
    polls_to_complete: u32,
    max_batch: usize,
}

impl Default for MockTypesRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTypesRegistry {
    /// Lenient: every call is answered, an unknown key is `NotFound`. Operations complete on
    /// their first poll; batches up to 100 candidates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            polls_to_complete: 1,
            max_batch: 100,
        }
    }

    /// Strict: a call of a method not [allowed](Self::allow), or a batch read of a key that is
    /// neither stored nor [expected absent](Self::expect_absent), panics, so a caller that
    /// swallows errors cannot hide it.
    #[must_use]
    pub fn strict() -> Self {
        let fake = Self::new();
        fake.state.lock().strict = Some(Strict {
            allowed: HashSet::new(),
            absent: Vec::new(),
        });
        fake
    }

    /// Allows `call` in strict mode; no effect on a lenient fake.
    pub fn allow(&self, call: Call) {
        if let Some(strict) = &mut self.state.lock().strict {
            strict.allowed.insert(call);
        }
    }

    /// Lets a strict batch read ask for `key` while it is not stored; no effect on a lenient fake.
    pub fn expect_absent(&self, key: impl Into<EntityKey>) {
        if let Some(strict) = &mut self.state.lock().strict {
            strict.absent.push(key.into());
        }
    }

    /// Operations stay pending for `polls` polls.
    #[must_use]
    pub fn completing_after(mut self, polls: u32) -> Self {
        self.polls_to_complete = polls;
        self
    }

    /// Submissions of more than `max` candidates are refused synchronously.
    #[must_use]
    pub fn with_max_batch(mut self, max: usize) -> Self {
        self.max_batch = max;
        self
    }

    /// Registers this fake in `hub` under both contracts.
    pub fn install(self: &Arc<Self>, hub: &toolkit::ClientHub) {
        hub.register::<dyn PlatformTypesRegistryApi>(
            Arc::clone(self) as Arc<dyn PlatformTypesRegistryApi>
        );
        hub.register::<dyn TypesRegistryApi>(Arc::clone(self) as Arc<dyn TypesRegistryApi>);
    }

    /// Stores an active entity directly, at version 1.
    ///
    /// # Panics
    /// If `gts_id` is not a valid GTS identifier.
    pub fn seed(&self, gts_id: &str, content: serde_json::Value) {
        self.seed_entity(Seed::new(gts_id, content));
    }

    /// Stores an entity directly, replacing any stored one.
    pub fn seed_entity(&self, seed: Seed) {
        let mut state = self.state.lock();
        state.resolver.invalidate();
        state.entities.insert(
            seed.gts_id.id().to_owned(),
            Stored {
                content: seed.content,
                resource_version: seed.version,
                lifecycle: seed.lifecycle,
                frozen: None,
            },
        );
    }

    /// Seed all Type Schemas and Instances in this binary's `toolkit-gts` inventory.
    ///
    /// # Panics
    /// If a declared Type Schema is not valid JSON.
    pub fn seed_inventory(&self) {
        for entry in toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema> {
            let schema = serde_json::from_str((entry.schema_fn)().as_str())
                .expect("an inventory Type Schema is valid JSON");
            self.seed(entry.type_id, schema);
        }
        for entry in toolkit_gts::inventory::iter::<toolkit_gts::InventoryInstance> {
            self.seed(entry.instance_id, (entry.payload_fn)());
        }
    }

    /// Admission refuses every registration of `gts_id` with `reason`.
    pub fn reject(&self, gts_id: &str, reason: Reason) {
        self.state
            .lock()
            .admission
            .rejections
            .insert(gts_id.to_owned(), reason);
    }

    /// Admission refuses `gts_id` as `dependency_not_found` while `dependency` is not active.
    pub fn depends_on(&self, gts_id: &str, dependency: &str) {
        self.state
            .lock()
            .admission
            .dependencies
            .push((gts_id.to_owned(), dependency.to_owned()));
    }

    /// Installs a rule; see [`Fault`].
    ///
    /// # Panics
    /// If the rule selects a key on a method that carries none (`List`, `GetOperation`).
    pub fn inject(&self, fault: Fault) {
        assert!(
            fault.key.is_none() || !matches!(fault.call, Some(Call::List | Call::GetOperation)),
            "a key selector never matches {:?}",
            fault.call
        );
        self.state.lock().rules.push((fault, 0));
    }

    /// Breaks later responses; see [`ProtocolFault`].
    pub fn protocol_fault(&self, fault: ProtocolFault) {
        let protocol = &mut self.state.lock().protocol;
        if let ProtocolFault::LoseReadBacks(n) = fault {
            protocol.lose_read_backs = n;
        } else {
            protocol.defects.insert(fault);
        }
    }

    /// Race the next registration with another publisher's create/update.
    pub fn race_next_submit(&self, gts_id: &str, content: serde_json::Value) {
        self.state.lock().races.push((gts_id.to_owned(), content));
    }

    /// How many calls of `call` were made, counted on entry: failed and cancelled ones included.
    #[must_use]
    pub fn calls(&self, call: Call) -> u32 {
        self.state.lock().calls.get(&call).copied().unwrap_or(0)
    }

    /// Every key batch reads asked for, in order.
    #[must_use]
    pub fn reads(&self) -> Vec<EntityKey> {
        self.state.lock().reads.clone()
    }

    /// Every item of every registration submitted, in order: [`Self::submissions`] flattened.
    #[must_use]
    pub fn registered(&self) -> Vec<RegisterItem> {
        self.state
            .lock()
            .submissions
            .iter()
            .flat_map(|(_, request)| request.items.iter().cloned())
            .collect()
    }

    /// Every registration that got past its delay, accepted, refused or replayed, in order.
    #[must_use]
    pub fn submissions(&self) -> Vec<(IdempotencyKey, RegisterEntitiesRequest)> {
        self.state.lock().submissions.clone()
    }

    /// The stored content of `gts_id`, if active.
    #[must_use]
    pub fn content(&self, gts_id: &str) -> Option<serde_json::Value> {
        self.state
            .lock()
            .entities
            .get(gts_id)
            .filter(|s| s.lifecycle == LifecycleStatus::Active)
            .map(|s| s.content.clone())
    }
}

// calls.

impl MockTypesRegistry {
    /// Records the call, enforces strict mode, and applies the first admitting rule: its delay,
    /// then its failure. The rule is chosen atomically, before any await.
    async fn enter(&self, call: Call, keys: &[EntityKey]) -> Option<CanonicalError> {
        let (delay, failure) = {
            let mut guard = self.state.lock();
            let state = &mut *guard;
            *state.calls.entry(call).or_default() += 1;
            if call == Call::BatchGet {
                state.reads.extend(keys.iter().cloned());
            }
            if let Some(strict) = &state.strict {
                assert!(
                    strict.allowed.contains(&call),
                    "MockTypesRegistry (strict): unexpected {call:?} call"
                );
                if call == Call::BatchGet {
                    for key in keys {
                        assert!(
                            find(&state.entities, key).is_some()
                                || strict.absent.iter().any(|a| same_entity(a, key)),
                            "MockTypesRegistry (strict): unexpected read of {key}"
                        );
                    }
                }
            }
            let mut chosen = None;
            for (fault, seen) in &mut state.rules {
                if fault.matches(call, keys) {
                    *seen += 1;
                    if chosen.is_none() && fault.when.admits(*seen) {
                        chosen = Some((fault.delay, fault.failure.clone()));
                    }
                }
            }
            chosen.unwrap_or_default()
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        failure
    }

    fn accept(
        &self,
        key: &IdempotencyKey,
        fingerprint: String,
        kind: OperationKind,
        candidates: Vec<Candidate>,
        dry_run: bool,
    ) -> Result<Uuid, CanonicalError> {
        let mut state = self.state.lock();
        if let Some((known, operation_id)) = state.keys.get(key.as_str()) {
            return if *known == fingerprint {
                Ok(*operation_id)
            } else {
                Err(OperationResource::already_exists(
                    "Idempotency-Key is already bound to another request",
                )
                .with_resource(operation_id.to_string())
                .create())
            };
        }
        let operation_id = Uuid::new_v4();
        state
            .keys
            .insert(key.as_str().to_owned(), (fingerprint, operation_id));
        state.operations.insert(
            operation_id,
            FakeOperation {
                kind,
                candidates,
                dry_run,
                polls_left: self.polls_to_complete,
                outcomes: None,
            },
        );
        Ok(operation_id)
    }

    fn read_back(&self, operation_id: Uuid) -> Result<Operation, CanonicalError> {
        {
            let protocol = &mut self.state.lock().protocol;
            if protocol.lose_read_backs > 0 {
                protocol.lose_read_backs -= 1;
                return Err(OperationResource::aborted("lost read back")
                    .with_resource(operation_id.to_string())
                    .with_reason(aborted::OPERATION_READ_FAILED)
                    .create());
            }
        }
        self.operation(operation_id, false)
    }

    /// The operation as a read sees it; a poll counts toward completion.
    fn operation(&self, operation_id: Uuid, poll: bool) -> Result<Operation, CanonicalError> {
        let mut guard = self.state.lock();
        let State {
            entities,
            operations,
            admission,
            protocol,
            resolver,
            ..
        } = &mut *guard;
        let Some(op) = operations.get_mut(&operation_id) else {
            return Err(OperationResource::not_found("no such operation")
                .with_resource(operation_id.to_string())
                .create());
        };
        if poll && op.outcomes.is_none() {
            op.polls_left = op.polls_left.saturating_sub(1);
        }
        if op.outcomes.is_none() && op.polls_left == 0 {
            let mut scratch;
            let entities = if op.dry_run {
                scratch = entities.clone();
                &mut scratch
            } else {
                resolver.invalidate();
                entities
            };
            let outcomes = op
                .candidates
                .iter()
                .map(|c| decide(entities, admission, c))
                .collect();
            op.outcomes = Some(outcomes);
        }
        let mut operation = render(
            operation_id,
            op,
            protocol.has(ProtocolFault::PreviewSuccesses),
        );
        if op.outcomes.is_some() && protocol.has(ProtocolFault::DropOperationItems) {
            match &mut operation {
                Operation::Registration(op) => op.items.clear(),
                Operation::Deletion(op) => op.items.clear(),
            }
        }
        Ok(operation)
    }
}

fn uuid_of(key: &EntityKey) -> Uuid {
    match key {
        EntityKey::GtsId(id) => id.to_uuid(),
        EntityKey::GtsUuid(uuid) => *uuid,
    }
}

fn same_entity(a: &EntityKey, b: &EntityKey) -> bool {
    uuid_of(a) == uuid_of(b)
}

fn find<'a>(
    entities: &'a BTreeMap<String, Stored>,
    key: &EntityKey,
) -> Option<(&'a String, &'a Stored)> {
    match key {
        EntityKey::GtsId(id) => entities.get_key_value(id.id()),
        EntityKey::GtsUuid(uuid) => entities
            .iter()
            .find(|(id, _)| gts::GtsId::try_new(id).is_ok_and(|g| g.to_uuid() == *uuid)),
    }
}

fn is_type(gts_id: &str) -> bool {
    gts_id.ends_with('~')
}

fn decide(
    entities: &mut BTreeMap<String, Stored>,
    admission: &Admission,
    candidate: &Candidate,
) -> Decided {
    let failed =
        |reason: Reason, message: &str| Decided::Failed(AdmissionFailure::new(reason, message));
    match candidate {
        Candidate::Register {
            gts_id,
            content,
            expected,
        } => {
            if let Some(reason) = admission.rejections.get(gts_id.id()) {
                return failed(reason.clone(), "the fake refuses this entity");
            }
            let missing = admission.dependencies.iter().find(|(id, dependency)| {
                id == gts_id.id()
                    && !entities
                        .get(dependency)
                        .is_some_and(|s| s.lifecycle == LifecycleStatus::Active)
            });
            if let Some((_, dependency)) = missing {
                return Decided::Failed(
                    AdmissionFailure::new(
                        Reason::DependencyNotFound,
                        "a dependency is not registered",
                    )
                    .with_context(context::DEPENDENCY_ID, dependency),
                );
            }
            // A tombstone stays present: it is never recreated, and a revision of it is refused.
            let current = entities.get(gts_id.id());
            match (current, expected) {
                (Some(_), None) => failed(Reason::AlreadyExists, "already exists"),
                (Some(s), Some(_)) if s.lifecycle == LifecycleStatus::Deleted => {
                    failed(Reason::EntityDeleted, "the entity is deleted")
                }
                (Some(s), Some(v)) if s.resource_version != *v => {
                    failed(Reason::PreconditionFailed, "stale precondition")
                }
                (None, Some(_)) => failed(Reason::PreconditionFailed, "nothing to update"),
                (Some(s), Some(_)) if s.content == *content => {
                    Decided::Unchanged(s.resource_version)
                }
                (current, _) => {
                    let version = current.map_or(1, |s| s.resource_version + 1);
                    entities.insert(
                        gts_id.id().to_owned(),
                        Stored {
                            content: content.clone(),
                            resource_version: version,
                            lifecycle: LifecycleStatus::Active,
                            frozen: None,
                        },
                    );
                    Decided::Succeeded(version)
                }
            }
        }
        Candidate::Delete { key, expected } => {
            // Check lifecycle before version: tombstones cannot be retried as live entities.
            let Some((gts_id, stored)) = find(entities, key) else {
                return failed(Reason::PreconditionFailed, "names no entity");
            };
            if stored.lifecycle != LifecycleStatus::Active {
                return failed(Reason::NotActive, "already deleted");
            }
            if stored.resource_version != *expected {
                return failed(Reason::PreconditionFailed, "stale precondition");
            }
            let gts_id = gts_id.clone();
            let frozen = is_type(&gts_id)
                .then(|| Resolver::default().resolve(entities, &gts_id).ok())
                .flatten();
            let Some(stored) = entities.get_mut(&gts_id) else {
                return failed(Reason::PreconditionFailed, "names no entity");
            };
            stored.lifecycle = LifecycleStatus::Deleted;
            stored.resource_version += 1;
            stored.frozen = frozen;
            Decided::Succeeded(stored.resource_version)
        }
    }
}

fn render(operation_id: Uuid, op: &FakeOperation, preview: bool) -> Operation {
    let status = if op.outcomes.is_some() {
        OperationStatus::Completed
    } else {
        OperationStatus::Pending
    };
    let decided = |i: usize| op.outcomes.as_ref().map(|o| &o[i]);
    let registration = |i: usize, key: &str| match decided(i) {
        None => RegistrationOutcome::Pending,
        Some(Decided::Succeeded(_)) if op.dry_run || preview => RegistrationOutcome::WouldSucceed,
        Some(Decided::Succeeded(resource_version)) => RegistrationOutcome::Succeeded {
            resource_version: *resource_version,
        },
        Some(Decided::Unchanged(resource_version)) => RegistrationOutcome::Unchanged {
            resource_version: *resource_version,
        },
        Some(Decided::Failed(failure)) => RegistrationOutcome::Failed {
            error: failure.clone().into_canonical(key),
        },
    };
    let deletion = |i: usize, key: &str| match decided(i) {
        None => DeletionOutcome::Pending,
        Some(Decided::Succeeded(_)) if op.dry_run => DeletionOutcome::WouldSucceed,
        Some(Decided::Succeeded(resource_version)) => DeletionOutcome::Succeeded {
            resource_version: *resource_version,
        },
        // `decide` never answers a deletion unchanged.
        Some(Decided::Unchanged(_)) => unreachable!("a deletion is never unchanged"),
        Some(Decided::Failed(failure)) => DeletionOutcome::Failed {
            error: failure.clone().into_canonical(key),
        },
    };
    match op.kind {
        OperationKind::Deletion => Operation::Deletion(DeletionOperation {
            operation_id,
            status,
            items: op
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    Candidate::Delete { key, .. } => Some(DeletionItemResult {
                        entity_key: key.clone(),
                        outcome: deletion(i, &key.to_string()),
                    }),
                    Candidate::Register { .. } => None,
                })
                .collect(),
        }),
        OperationKind::Registration => Operation::Registration(RegistrationOperation {
            operation_id,
            status,
            items: op
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    Candidate::Register { gts_id, .. } => Some(RegistrationItemResult {
                        gts_id: gts_id.clone(),
                        outcome: registration(i, gts_id.id()),
                    }),
                    Candidate::Delete { .. } => None,
                })
                .collect(),
        }),
    }
}

// documents and reads.

/// Resolves Type Schemas with a `gts-rust` store over every stored Type Schema. The store and the
/// resolutions are kept until a Type Schema's content changes ([`Self::invalidate`]).
#[derive(Default)]
struct Resolver {
    store: Option<gts::GtsStore>,
    resolved: HashMap<String, Resolution>,
}

impl Resolver {
    fn invalidate(&mut self) {
        self.store = None;
        self.resolved.clear();
    }

    fn resolve(&mut self, entities: &BTreeMap<String, Stored>, gts_id: &str) -> Resolution {
        if let Some(frozen) = entities.get(gts_id).and_then(|s| s.frozen.clone()) {
            return Ok(frozen);
        }
        if let Some(resolution) = self.resolved.get(gts_id) {
            return resolution.clone();
        }
        let store = self.store.get_or_insert_with(|| {
            let mut store = gts::GtsStore::new();
            for (id, stored) in entities.iter().filter(|(id, _)| is_type(id)) {
                // An identifier that is not a type id fails only its own resolution.
                store.register_schema(id, &stored.content).ok();
            }
            store
        });
        let resolution = store
            .validate_schema_with(gts_id, gts::GtsRefValidation::None)
            .map(|resolved| Documents {
                resolved_schema: resolved.schema,
                effective_traits: resolved.effective_traits,
                effective_traits_schema: resolved.effective_traits_schema,
            })
            .map_err(|error| error.to_string());
        self.resolved.insert(gts_id.to_owned(), resolution.clone());
        resolution
    }
}

/// Resolves `gts_id`; the first successful resolution of a tombstone fixes its documents.
fn resolve_and_freeze(
    resolver: &mut Resolver,
    entities: &mut BTreeMap<String, Stored>,
    gts_id: &str,
) -> Resolution {
    let resolution = resolver.resolve(entities, gts_id);
    if let (Ok(documents), Some(stored)) = (&resolution, entities.get_mut(gts_id))
        && stored.lifecycle == LifecycleStatus::Deleted
        && stored.frozen.is_none()
    {
        stored.frozen = Some(documents.clone());
    }
    resolution
}

const DOCUMENTS: [EntityField; 3] = [
    EntityField::ResolvedSchema,
    EntityField::EffectiveTraits,
    EntityField::EffectiveTraitsSchema,
];

/// `resolution` is the entity's own for a Type Schema, `None` for an Instance.
fn snapshot(
    gts_id: &str,
    stored: &Stored,
    fields: &FieldSelection,
    resolution: Option<&Resolution>,
) -> Result<Entity, CanonicalError> {
    let id = gts::GtsId::try_new(gts_id).expect("the fake stores valid identifiers");
    let (gts_uuid, kind) = (id.to_uuid(), EntityKind::of(&id));
    let has = |f| fields.contains(f);
    let documents = match resolution {
        Some(Err(error)) if DOCUMENTS.into_iter().any(has) => {
            return Err(CanonicalError::internal(format!(
                "MockTypesRegistry: {gts_id} does not resolve: {error}"
            ))
            .create());
        }
        Some(Ok(documents)) => Some(documents),
        _ => None,
    };
    let document = |field, pick: fn(&Documents) -> &serde_json::Value| {
        documents.filter(|_| has(field)).map(|d| pick(d).clone())
    };
    Ok(Entity {
        gts_id: id,
        gts_uuid,
        kind,
        lifecycle_status: stored.lifecycle,
        origin: has(EntityField::Origin).then_some(Origin::Managed {
            resource_version: stored.resource_version,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }),
        content: has(EntityField::Content).then(|| stored.content.clone()),
        resolved_schema: document(EntityField::ResolvedSchema, |d| &d.resolved_schema),
        effective_traits: document(EntityField::EffectiveTraits, |d| &d.effective_traits),
        effective_traits_schema: document(EntityField::EffectiveTraitsSchema, |d| {
            &d.effective_traits_schema
        }),
        // The mock does not model compatibility waivers, so it reports `false`.
        provenance: (kind == EntityKind::TypeSchema && has(EntityField::Provenance)).then_some(
            Provenance {
                compatibility_check_skipped: false,
            },
        ),
    })
}

/// Bind the token to the projection and resolved documents, including parent changes.
fn validator(
    stored: &Stored,
    fields: &FieldSelection,
    resolution: Option<&Resolution>,
) -> Validator {
    let resolution = resolution.map(|r| {
        let mut hasher = DefaultHasher::new();
        hasher.write(format!("{r:?}").as_bytes());
        hasher.finish()
    });
    Validator::from_bytes(
        format!("v{}:{fields:?}:{resolution:?}", stored.resource_version).into_bytes(),
    )
}

/// Bind cursors to filters and projection, allowing page-size changes.
fn binding(query: &ListEntitiesRequest) -> String {
    let filter = &query.filter;
    format!(
        "{:?}",
        (
            &filter.pattern,
            filter.max_chain_depth,
            filter.kind,
            filter.lifecycle,
            query.projection.normalized(),
        )
    )
}

/// Refuse a cursor used with a different query.
fn cursor_not_usable() -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(
            field::CURSOR_FIELD,
            "the cursor cannot be used for this request: it was issued for another query",
            field::VALIDATION_FAILED,
        )
        .create()
}

fn items_violation(message: impl Into<String>) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(field::ITEMS_FIELD, message, field::VALIDATION_FAILED)
        .create()
}

impl MockTypesRegistry {
    /// The batch read both contracts serve.
    async fn read_batch(
        &self,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let keys: Vec<EntityKey> = request.items.iter().map(|i| i.key.clone()).collect();
        if let Some(error) = self.enter(Call::BatchGet, &keys).await {
            return Err(error);
        }
        if request.items.is_empty() || request.items.len() > crate::models::MAX_BATCH_GET_KEYS {
            return Err(items_violation("batch read out of range"));
        }
        let fields = request.projection.normalized();
        let mut guard = self.state.lock();
        let state = &mut *guard;
        if state.protocol.has(ProtocolFault::DropAnswers) {
            return Ok(BatchGetEntitiesResponse(HashMap::new()));
        }
        let mut out = HashMap::new();
        for item in request.items {
            // A repeated key is answered once, under its first mention's condition.
            if out.contains_key(&item.key) {
                continue;
            }
            let Some(id) = find(&state.entities, &item.key).map(|(id, _)| id.clone()) else {
                out.insert(item.key, EntityLookup::NotFound);
                continue;
            };
            let resolution = is_type(&id)
                .then(|| resolve_and_freeze(&mut state.resolver, &mut state.entities, &id));
            let stored = state.entities.get(&id).expect("found above");
            let etag = validator(stored, &fields, resolution.as_ref());
            let lookup = if item.if_none_match.as_ref() == Some(&etag) {
                EntityLookup::Unchanged { etag }
            } else {
                let mut snapshot = snapshot(&id, stored, &fields, resolution.as_ref())?;
                if state.protocol.has(ProtocolFault::MislabelKind) {
                    snapshot.kind = match snapshot.kind {
                        EntityKind::TypeSchema => EntityKind::Instance,
                        EntityKind::Instance => EntityKind::TypeSchema,
                    };
                }
                if state.protocol.has(ProtocolFault::StripOrigin) {
                    snapshot.origin = None;
                }
                EntityLookup::Found {
                    entity: Box::new(snapshot),
                    etag,
                }
            };
            out.insert(item.key, lookup);
        }
        Ok(BatchGetEntitiesResponse(out))
    }

    /// The discovery page both contracts serve.
    async fn read_page(
        &self,
        query: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        if let Some(error) = self.enter(Call::List, &[]).await {
            return Err(error);
        }
        let fields = query.projection.normalized();
        // Default page size 50, maximum 100.
        let limit = match query.page.limit {
            None => 50,
            Some(asked @ 1..=100) => asked as usize,
            Some(asked) => {
                return Err(TypeResource::invalid_argument()
                    .with_field_violation(
                        field::LIMIT_FIELD,
                        format!(
                            "a page size must be between 1 and 100; this request asked for {asked}"
                        ),
                        field::VALIDATION_FAILED,
                    )
                    .create());
            }
        };
        let binding = binding(&query);
        let after = match &query.page.cursor {
            None => None,
            Some(cursor) => match cursor.as_str().rsplit_once('\n') {
                Some((bound, after)) if bound == binding => Some(after.to_owned()),
                _ => return Err(cursor_not_usable()),
            },
        };
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let matching: Vec<String> = state
            .entities
            .iter()
            .filter(|(id, stored)| {
                let gid = gts::GtsId::try_new(id).expect("valid");
                let lifecycle = match query.filter.lifecycle {
                    LifecycleFilter::Active => stored.lifecycle == LifecycleStatus::Active,
                    LifecycleFilter::Deleted => stored.lifecycle == LifecycleStatus::Deleted,
                    LifecycleFilter::All => true,
                };
                lifecycle
                    && query
                        .filter
                        .max_chain_depth
                        .is_none_or(|d| gid.segments().len() <= usize::from(d.get()))
                    && query.filter.kind.is_none_or(|k| EntityKind::of(&gid) == k)
                    && query
                        .filter
                        .pattern
                        .as_ref()
                        .is_none_or(|p| gid.matches_pattern(p))
                    && after.as_ref().is_none_or(|a| id.as_str() > a.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();
        let next = match &query.page.cursor {
            Some(cursor) if state.protocol.has(ProtocolFault::RepeatListCursor) => {
                Some(cursor.clone())
            }
            _ => (matching.len() > limit)
                .then(|| crate::Cursor::from_token(format!("{binding}\n{}", matching[limit - 1]))),
        };
        let items = matching
            .into_iter()
            .take(limit)
            .map(|id| {
                let resolution = is_type(&id)
                    .then(|| resolve_and_freeze(&mut state.resolver, &mut state.entities, &id));
                let stored = state.entities.get(&id).expect("listed above");
                snapshot(&id, stored, &fields, resolution.as_ref())
            })
            .collect::<Result<_, _>>()?;
        Ok(ListEntitiesResponse { items, next })
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for MockTypesRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.read_batch(request).await
    }

    async fn list_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.read_page(request).await
    }

    async fn register_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        let keys: Vec<EntityKey> = request
            .items
            .iter()
            .map(|i| EntityKey::GtsId(i.gts_id.clone()))
            .collect();
        let failure = self.enter(Call::Register, &keys).await;
        {
            let mut state = self.state.lock();
            state.submissions.push((key.clone(), request.clone()));
            if let Some(error) = failure {
                return Err(error);
            }
            if !state.races.is_empty() {
                state.resolver.invalidate();
            }
            for (gts_id, content) in std::mem::take(&mut state.races) {
                let version = state
                    .entities
                    .get(&gts_id)
                    .map_or(1, |s| s.resource_version + 1);
                state.entities.insert(
                    gts_id,
                    Stored {
                        content,
                        resource_version: version,
                        lifecycle: LifecycleStatus::Active,
                        frozen: None,
                    },
                );
            }
        }
        if request.items.is_empty() {
            return Err(items_violation("a request must carry at least one entity"));
        }
        if request.items.len() > self.max_batch {
            return Err(items_violation(format!(
                "{} entities exceeds the limit of {} per request",
                request.items.len(),
                self.max_batch
            )));
        }
        let fingerprint = format!(
            "{:?}",
            (OperationKind::Registration, &request.items, request.dry_run,)
        );
        let candidates = request
            .items
            .into_iter()
            .map(|item| Candidate::Register {
                gts_id: item.gts_id,
                content: item.content,
                expected: item.expected_resource_version,
            })
            .collect();
        let operation_id = self.accept(
            &key,
            fingerprint,
            OperationKind::Registration,
            candidates,
            request.dry_run,
        )?;
        match self.read_back(operation_id)? {
            Operation::Registration(op) => Ok(op),
            Operation::Deletion(_) => Err(OperationResource::already_exists(
                "Idempotency-Key is already bound to another request",
            )
            .with_resource(operation_id.to_string())
            .create()),
        }
    }

    async fn delete_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        let keys: Vec<EntityKey> = request.items.iter().map(|i| i.key.clone()).collect();
        if let Some(error) = self.enter(Call::Delete, &keys).await {
            return Err(error);
        }
        if request.items.is_empty() {
            return Err(items_violation("a request must carry at least one entity"));
        }
        // As the real deletion digest: both key spellings of one entity bind the same request.
        let targets: Vec<(Uuid, u64)> = request
            .items
            .iter()
            .map(|i| (uuid_of(&i.key), i.expected_resource_version))
            .collect();
        let fingerprint = format!("{:?}", (OperationKind::Deletion, targets, request.dry_run));
        let candidates = request
            .items
            .into_iter()
            .map(|item| Candidate::Delete {
                key: item.key,
                expected: item.expected_resource_version,
            })
            .collect();
        let operation_id = self.accept(
            &key,
            fingerprint,
            OperationKind::Deletion,
            candidates,
            request.dry_run,
        )?;
        match self.read_back(operation_id)? {
            Operation::Deletion(op) => Ok(op),
            Operation::Registration(_) => Err(OperationResource::already_exists(
                "Idempotency-Key is already bound to another request",
            )
            .with_resource(operation_id.to_string())
            .create()),
        }
    }

    async fn get_operation(
        &self,
        _ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        if let Some(error) = self.enter(Call::GetOperation, &[]).await {
            return Err(error);
        }
        self.operation(operation_id, true)
    }
}

/// The tenant reads are the platform's: one store, one set of faults and counters.
#[async_trait]
impl TypesRegistryApi for MockTypesRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.read_batch(request).await
    }

    async fn list_entities(
        &self,
        _ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.read_page(request).await
    }
}

#[cfg(test)]
#[path = "testing_platform_tests.rs"]
mod testing_platform_tests;

//! In-memory platform fixture with delayed completion and idempotency replay (`test-util`).
//! Document hooks simulate invalid schemas, dependencies, supersession and publisher mismatch;
//! use `x-fake-invalid`, `x-fake-depends-on`, `x-fake-superseded`, `x-fake-publisher-mismatch`.
//! request hooks simulate failures, delays, panics, missing results and concurrent writes.
//! Equal content is unchanged; writes advance versions; dry runs use a discarded copy.
//! Type Schema materializations are synthetic; Instances have none.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, CandidateStatus, DeleteEntitiesRequest,
    DeletionItemResult, DeletionOperation, EntityField, EntityKey, EntityKind, EntityLookup,
    EntitySnapshot, IdempotencyKey, LifecycleStatus, ListEntitiesRequest, ListEntitiesResponse,
    Operation, OperationStatus, Origin, RegisterEntitiesRequest, RegistrationItemResult,
    RegistrationOperation, Validator,
};
use crate::field;
use crate::gts::{OperationResource, TypeResource};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason as Reason, context};
use crate::reason::aborted;

#[derive(Debug, Clone)]
struct Stored {
    content: serde_json::Value,
    resource_version: u64,
    lifecycle: LifecycleStatus,
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

/// A decided candidate: its status, resulting version and failure.
type Outcome = (CandidateStatus, Option<u64>, Option<AdmissionFailure>);

#[derive(Debug)]
struct FakeOperation {
    candidates: Vec<Candidate>,
    /// Decided against a copy of the state, which is then discarded.
    dry_run: bool,
    /// Polls left before the operation completes.
    polls_left: u32,
    /// Decided outcomes, once completed: status, version, failure.
    outcomes: Option<Vec<Outcome>>,
}

#[derive(Default)]
struct State {
    entities: BTreeMap<String, Stored>,
    operations: HashMap<Uuid, FakeOperation>,
    keys: HashMap<String, (String, Uuid)>,
    submissions: Vec<(IdempotencyKey, RegisterEntitiesRequest)>,
    deletions: Vec<(IdempotencyKey, DeleteEntitiesRequest)>,
    /// Errors the next submissions fail with, in order, before being accepted.
    submit_failures: Vec<CanonicalError>,
    /// Writes another publisher makes just before the next registration.
    races: Vec<(String, serde_json::Value)>,
}

pub struct FakePlatformRegistry {
    state: Mutex<State>,
    polls_to_complete: u32,
    max_batch: usize,
    lose_read_backs: AtomicU32,
    panic_on_submit: AtomicBool,
    batch_reads: AtomicU32,
    submit_delay: Mutex<std::time::Duration>,
    poll_delay: Mutex<std::time::Duration>,
    read_fault: Mutex<ReadFault>,
    drop_operation_items: AtomicBool,
    hang_read: AtomicU32,
    polls: AtomicU32,
    repeat_list_cursor: AtomicBool,
}

/// A broken `batch_get_entities` response, for protocol-fault tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFault {
    /// Answers normally.
    None,
    /// Answers no key at all.
    DropAnswers,
    /// Answers `Found` without the selected `origin`.
    StripOrigin,
    /// Fails as unavailable from the `n`th read on (1-based).
    FailFrom(u32),
    /// Fails as unavailable on the `n`th read only (1-based).
    FailOnly(u32),
}

impl Default for FakePlatformRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FakePlatformRegistry {
    /// Operations complete on their first poll; batches up to 100 candidates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            polls_to_complete: 1,
            max_batch: 100,
            lose_read_backs: AtomicU32::new(0),
            panic_on_submit: AtomicBool::new(false),
            batch_reads: AtomicU32::new(0),
            submit_delay: Mutex::new(std::time::Duration::ZERO),
            poll_delay: Mutex::new(std::time::Duration::ZERO),
            read_fault: Mutex::new(ReadFault::None),
            drop_operation_items: AtomicBool::new(false),
            hang_read: AtomicU32::new(0),
            polls: AtomicU32::new(0),
            repeat_list_cursor: AtomicBool::new(false),
        }
    }

    /// A list page continued from a cursor answers that same cursor again.
    pub fn repeat_list_cursor(&self) {
        self.repeat_list_cursor.store(true, Ordering::SeqCst);
    }

    /// Fail and record the next count submissions before acceptance ([`Self::submissions`]).
    pub fn fail_submits(&self, count: usize, error: &CanonicalError) {
        let mut state = self.state.lock();
        state
            .submit_failures
            .extend(std::iter::repeat_n(error.clone(), count));
    }

    /// Every deletion submitted, accepted or replayed, in order.
    #[must_use]
    pub fn deletions(&self) -> Vec<(IdempotencyKey, DeleteEntitiesRequest)> {
        self.state.lock().deletions.clone()
    }

    /// How many `get_operation` calls were made.
    #[must_use]
    pub fn polls(&self) -> u32 {
        self.polls.load(Ordering::SeqCst)
    }

    /// The `n`th `batch_get_entities` call (1-based) never answers.
    pub fn hang_read(&self, n: u32) {
        self.hang_read.store(n, Ordering::SeqCst);
    }

    /// Completed operations report no items.
    pub fn drop_operation_items(&self) {
        self.drop_operation_items.store(true, Ordering::SeqCst);
    }

    /// Breaks later `batch_get_entities` responses.
    pub fn fault_reads(&self, fault: ReadFault) {
        *self.read_fault.lock() = fault;
    }

    /// Every submission takes `delay` before it answers.
    pub fn delay_submits(&self, delay: std::time::Duration) {
        *self.submit_delay.lock() = delay;
    }

    /// Every `get_operation` takes `delay` before it answers.
    pub fn delay_polls(&self, delay: std::time::Duration) {
        *self.poll_delay.lock() = delay;
    }

    /// Race the next registration with another publisher’s create/update.
    pub fn race_next_submit(&self, gts_id: &str, content: serde_json::Value) {
        self.state.lock().races.push((gts_id.to_owned(), content));
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

    /// The next `count` accepted submissions fail their read back.
    pub fn lose_read_backs(&self, count: u32) {
        self.lose_read_backs.store(count, Ordering::SeqCst);
    }

    /// Every later submission panics.
    pub fn panic_on_submit(&self) {
        self.panic_on_submit.store(true, Ordering::SeqCst);
    }

    /// Stores an active entity directly, at version 1.
    pub fn seed(&self, gts_id: &str, content: serde_json::Value) {
        self.state.lock().entities.insert(
            gts_id.to_owned(),
            Stored {
                content,
                resource_version: 1,
                lifecycle: LifecycleStatus::Active,
            },
        );
    }

    /// Every registration submitted, accepted or replayed, in order.
    #[must_use]
    pub fn submissions(&self) -> Vec<(IdempotencyKey, RegisterEntitiesRequest)> {
        self.state.lock().submissions.clone()
    }

    /// How many `batch_get_entities` calls were made.
    #[must_use]
    pub fn batch_reads(&self) -> u32 {
        self.batch_reads.load(Ordering::SeqCst)
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

    fn accept(
        &self,
        key: &IdempotencyKey,
        fingerprint: String,
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
                candidates,
                dry_run,
                polls_left: self.polls_to_complete,
                outcomes: None,
            },
        );
        Ok(operation_id)
    }

    fn read_back(&self, operation_id: Uuid) -> Result<Operation, CanonicalError> {
        if self
            .lose_read_backs
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(OperationResource::aborted("lost read back")
                .with_resource(operation_id.to_string())
                .with_reason(aborted::OPERATION_READ_FAILED)
                .create());
        }
        self.operation(operation_id, false)
    }

    /// The operation as a read sees it; a poll counts toward completion.
    fn operation(&self, operation_id: Uuid, poll: bool) -> Result<Operation, CanonicalError> {
        let mut guard = self.state.lock();
        let state = &mut *guard;
        let Some(op) = state.operations.get_mut(&operation_id) else {
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
                scratch = state.entities.clone();
                &mut scratch
            } else {
                &mut state.entities
            };
            let outcomes = op.candidates.iter().map(|c| decide(entities, c)).collect();
            op.outcomes = Some(outcomes);
        }
        let mut operation = render(operation_id, op);
        if op.outcomes.is_some() && self.drop_operation_items.load(Ordering::SeqCst) {
            match &mut operation {
                Operation::Registration(op) => op.items.clear(),
                Operation::Deletion(op) => op.items.clear(),
            }
        }
        Ok(operation)
    }
}

fn decide(entities: &mut BTreeMap<String, Stored>, candidate: &Candidate) -> Outcome {
    let failed = |reason: Reason, message: &str| {
        (
            CandidateStatus::Failed,
            None,
            Some(AdmissionFailure::new(reason, message)),
        )
    };
    match candidate {
        Candidate::Register {
            gts_id,
            content,
            expected,
        } => {
            if content.get("x-fake-superseded") == Some(&serde_json::Value::Bool(true)) {
                if content.get("x-fake-superseded-deletes") == Some(&serde_json::Value::Bool(true))
                    && let Some(stored) = entities.get_mut(gts_id.id())
                {
                    stored.lifecycle = LifecycleStatus::Deleted;
                    stored.resource_version += 1;
                }
                let (status, version, failure) =
                    failed(Reason::Superseded, "a newer release owns this entity");
                let versions = content
                    .get("x-fake-superseded-versions")
                    .and_then(serde_json::Value::as_str);
                return (
                    status,
                    version,
                    failure.map(|f| match versions {
                        Some("missing") => f,
                        Some("invalid") => f
                            .with_context(context::STORED_VERSION, "nine")
                            .with_context(context::OFFERED_VERSION, "1.0.0"),
                        _ => f
                            .with_context(context::STORED_VERSION, "9.0.0")
                            .with_context(context::OFFERED_VERSION, "1.0.0"),
                    }),
                );
            }
            if content.get("x-fake-publisher-mismatch") == Some(&serde_json::Value::Bool(true)) {
                return failed(
                    Reason::PublisherMismatch,
                    "another publisher owns this entity",
                );
            }
            if content.get("x-fake-invalid") == Some(&serde_json::Value::Bool(true)) {
                return failed(Reason::InvalidSchema, "the fake refuses this document");
            }
            if let Some(dep) = content.get("x-fake-depends-on").and_then(|v| v.as_str())
                && !entities
                    .get(dep)
                    .is_some_and(|s| s.lifecycle == LifecycleStatus::Active)
            {
                let (status, version, failure) =
                    failed(Reason::DependencyNotFound, "a dependency is not registered");
                return (
                    status,
                    version,
                    failure.map(|f| f.with_context(context::DEPENDENCY_ID, dep)),
                );
            }
            let current = entities
                .get(gts_id.id())
                .filter(|s| s.lifecycle == LifecycleStatus::Active);
            match (current, expected) {
                (Some(_), None) => failed(Reason::AlreadyExists, "already exists"),
                (Some(s), Some(v)) if s.resource_version != *v => {
                    failed(Reason::PreconditionFailed, "stale precondition")
                }
                (None, Some(_)) => failed(Reason::PreconditionFailed, "nothing to update"),
                (Some(s), Some(_)) if s.content == *content => {
                    (CandidateStatus::Unchanged, Some(s.resource_version), None)
                }
                (current, _) => {
                    let version = current.map_or(1, |s| s.resource_version + 1);
                    entities.insert(
                        gts_id.id().to_owned(),
                        Stored {
                            content: content.clone(),
                            resource_version: version,
                            lifecycle: LifecycleStatus::Active,
                        },
                    );
                    (CandidateStatus::Succeeded, Some(version), None)
                }
            }
        }
        Candidate::Delete { key, expected } => {
            let EntityKey::GtsId(id) = key else {
                return failed(
                    Reason::from_wire("not_found"),
                    "the fake deletes by identifier only",
                );
            };
            match entities.get_mut(id.id()) {
                Some(s)
                    if s.lifecycle == LifecycleStatus::Active
                        && s.resource_version == *expected =>
                {
                    s.lifecycle = LifecycleStatus::Deleted;
                    s.resource_version += 1;
                    (CandidateStatus::Succeeded, Some(s.resource_version), None)
                }
                Some(_) => failed(Reason::PreconditionFailed, "stale precondition"),
                None => failed(Reason::from_wire("not_found"), "absent"),
            }
        }
    }
}

fn render(operation_id: Uuid, op: &FakeOperation) -> Operation {
    let status = if op.outcomes.is_some() {
        OperationStatus::Completed
    } else {
        OperationStatus::Pending
    };
    let outcome = |i: usize| {
        op.outcomes
            .as_ref()
            .map_or((CandidateStatus::Pending, None, None), |o| o[i].clone())
    };
    match op.candidates.first() {
        Some(Candidate::Delete { .. }) => Operation::Deletion(DeletionOperation {
            operation_id,
            status,
            items: op
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    Candidate::Delete { key, .. } => {
                        let (status, resource_version, failure) = outcome(i);
                        Some(DeletionItemResult {
                            entity_key: key.clone(),
                            status,
                            resource_version,
                            error: failure.map(|f| f.into_canonical(&key.to_string())),
                        })
                    }
                    Candidate::Register { .. } => None,
                })
                .collect(),
        }),
        _ => Operation::Registration(RegistrationOperation {
            operation_id,
            status,
            items: op
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    Candidate::Register { gts_id, .. } => {
                        let (status, resource_version, failure) = outcome(i);
                        Some(RegistrationItemResult {
                            gts_id: gts_id.clone(),
                            status,
                            resource_version,
                            error: failure.map(|f| f.into_canonical(gts_id.id())),
                        })
                    }
                    Candidate::Delete { .. } => None,
                })
                .collect(),
        }),
    }
}

fn snapshot(gts_id: &str, stored: &Stored, fields: &crate::FieldSelection) -> EntitySnapshot {
    let id = gts::GtsId::try_new(gts_id).expect("the fake stores valid identifiers");
    let (gts_uuid, kind) = (id.to_uuid(), EntityKind::of(&id));
    let is_type = kind == EntityKind::TypeSchema;
    let has = |f| fields.contains(f);
    EntitySnapshot {
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
        resolved_schema: (is_type && has(EntityField::ResolvedSchema))
            .then(|| serde_json::json!({ "x-fake-resolved": stored.content })),
        effective_traits: (is_type && has(EntityField::EffectiveTraits))
            .then(|| serde_json::json!({ "x-fake-traits-of": gts_id })),
        effective_traits_schema: (is_type && has(EntityField::EffectiveTraitsSchema))
            .then(|| serde_json::json!({ "x-fake-traits-schema-of": gts_id })),
        provenance: None,
    }
}

fn validator(stored: &Stored) -> Validator {
    Validator::from_bytes(format!("\"v{}\"", stored.resource_version).into_bytes())
}

#[async_trait]
impl PlatformTypesRegistryApi for FakePlatformRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let call = self.batch_reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.hang_read.load(Ordering::SeqCst) == call {
            std::future::pending::<()>().await;
        }
        let fault = *self.read_fault.lock();
        if matches!(fault, ReadFault::FailFrom(n) if call >= n)
            || matches!(fault, ReadFault::FailOnly(n) if call == n)
        {
            return Err(CanonicalError::service_unavailable().create());
        }
        if fault == ReadFault::DropAnswers {
            return Ok(BatchGetEntitiesResponse(HashMap::new()));
        }
        if request.items.is_empty() || request.items.len() > crate::ext::MAX_BATCH_GET_KEYS {
            return Err(TypeResource::invalid_argument()
                .with_field_violation(
                    field::ITEMS_FIELD,
                    "batch read out of range",
                    field::VALIDATION_FAILED,
                )
                .create());
        }
        let fields = request.projection.normalized();
        let state = self.state.lock();
        let mut out = HashMap::new();
        for item in request.items {
            let found = state.entities.iter().find(|(id, _)| match &item.key {
                EntityKey::GtsId(g) => g.id() == id.as_str(),
                EntityKey::GtsUuid(u) => gts::GtsId::try_new(id).is_ok_and(|g| g.to_uuid() == *u),
            });
            let lookup = match found {
                None => EntityLookup::NotFound,
                Some((id, stored)) => {
                    let etag = validator(stored);
                    if item.if_none_match.as_ref() == Some(&etag) {
                        EntityLookup::Unchanged { etag }
                    } else {
                        let mut snapshot = snapshot(id, stored, &fields);
                        if fault == ReadFault::StripOrigin {
                            snapshot.origin = None;
                        }
                        EntityLookup::Found {
                            snapshot: Box::new(snapshot),
                            etag,
                        }
                    }
                }
            };
            out.insert(item.key, lookup);
        }
        Ok(BatchGetEntitiesResponse(out))
    }

    async fn list_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let fields = query.projection.normalized();
        let limit = query.page.limit.unwrap_or(50) as usize;
        let after = query.page.cursor.as_ref().map(|c| c.as_str().to_owned());
        let state = self.state.lock();
        let matching: Vec<(&String, &Stored)> = state
            .entities
            .iter()
            .filter(|(id, stored)| {
                let gid = gts::GtsId::try_new(id).expect("valid");
                stored.lifecycle == LifecycleStatus::Active
                    && query.filter.kind.is_none_or(|k| EntityKind::of(&gid) == k)
                    && query
                        .filter
                        .pattern
                        .as_ref()
                        .is_none_or(|p| gid.matches_pattern(p))
                    && after.as_ref().is_none_or(|a| id.as_str() > a.as_str())
            })
            .collect();
        let next = match &query.page.cursor {
            Some(cursor) if self.repeat_list_cursor.load(Ordering::SeqCst) => Some(cursor.clone()),
            _ => (matching.len() > limit)
                .then(|| crate::Cursor::from_token(matching[limit - 1].0.clone())),
        };
        Ok(ListEntitiesResponse {
            items: matching
                .into_iter()
                .take(limit)
                .map(|(id, stored)| snapshot(id, stored, &fields))
                .collect(),
            next,
        })
    }

    async fn register_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        assert!(
            !self.panic_on_submit.load(Ordering::SeqCst),
            "the fake registry was told to panic"
        );
        let delay = *self.submit_delay.lock();
        if !delay.is_zero() {
            toolkit::tokio::time::sleep(delay).await;
        }
        {
            let mut state = self.state.lock();
            state.submissions.push((key.clone(), request.clone()));
            if !state.submit_failures.is_empty() {
                return Err(state.submit_failures.remove(0));
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
                    },
                );
            }
        }
        if request.items.len() > self.max_batch {
            return Err(TypeResource::invalid_argument()
                .with_field_violation(
                    field::ITEMS_FIELD,
                    format!(
                        "{} entities exceeds the limit of {} per request",
                        request.items.len(),
                        self.max_batch
                    ),
                    field::VALIDATION_FAILED,
                )
                .create());
        }
        let fingerprint = format!("{:?}", (&request.items, request.dry_run));
        let dry_run = request.dry_run;
        let candidates = request
            .items
            .into_iter()
            .map(|item| Candidate::Register {
                gts_id: item.gts_id,
                content: item.content,
                expected: item.expected_resource_version,
            })
            .collect();
        let operation_id = self.accept(&key, fingerprint, candidates, dry_run)?;
        match self.read_back(operation_id)? {
            Operation::Registration(op) => Ok(op),
            Operation::Deletion(_) => unreachable!("registration keys map to registrations"),
        }
    }

    async fn delete_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.state
            .lock()
            .deletions
            .push((key.clone(), request.clone()));
        let fingerprint = format!("{:?}", (&request.items, request.dry_run));
        let dry_run = request.dry_run;
        let candidates = request
            .items
            .into_iter()
            .map(|item| Candidate::Delete {
                key: item.key,
                expected: item.expected_resource_version,
            })
            .collect();
        let operation_id = self.accept(&key, fingerprint, candidates, dry_run)?;
        match self.read_back(operation_id)? {
            Operation::Deletion(op) => Ok(op),
            Operation::Registration(_) => unreachable!("deletion keys map to deletions"),
        }
    }

    async fn get_operation(
        &self,
        _ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        let delay = *self.poll_delay.lock();
        if !delay.is_zero() {
            toolkit::tokio::time::sleep(delay).await;
        }
        self.operation(operation_id, true)
    }
}

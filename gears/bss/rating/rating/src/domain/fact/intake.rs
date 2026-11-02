//! `FactIntake` (DESIGN 14 §3.2): accepts a fact version through the inbox and applies it, and
//! binds a pending usage fact again from `rating.fact_bind` (feature 06 §2.1, [14 §3.6]).
//!
//! The one remote read of intake — Products' derived declaration of a usage fact's meter — runs
//! before the inbox transaction, and its outcome is handed to the apply step
//! ([`Inbox::accept_one_with`]); the step itself reads and writes only Rating's rows. A fact
//! released from quarantine applies through the registered applier, which has read nothing: a
//! usage fact then waits `pending(derived_declaration_unavailable)` and binds on
//! `rating.fact_bind`.
//!
//! [14 §3.6]: ../../../../docs/features/06-fact-scheduling.md#contract-14-3-6-f06

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bss_rating_sdk::errors::reason::INVALID_REQUEST;
use serde_json::Value;
use time::{Duration, OffsetDateTime};
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::child::{ChildRepo, ChildStatus, ChildWork};
use crate::domain::declaration::{DeclarationOutcome, DerivedDeclarationStore, StoredDeclaration};
use crate::domain::error::DomainError;
use crate::domain::exception::{
    ExceptionKey, ExceptionReason, ExceptionRegister, SubjectKind, SubjectRef,
};
use crate::domain::fact::expected::{
    Expected, UsagePlan, UsageShape, expected_children, usage_shape,
};
use crate::domain::fact::model::{FactKind, FactVersion, Obligation, Timing};
use crate::domain::fact::profile::{ReadFact, read_fact};
use crate::domain::fact::{
    BindingState, FactHead, FactRepo, FailedBinding, NewSchedule, PendingBinding, ProjectionRepo,
    ProjectionRow, ScheduleRepo, StoredVersion, shard_of,
};
use crate::domain::inbox::{
    AcceptOutcome, InboundFact, Inbox, InboxApplier, InboxEntry, InboxKey, InboxSource,
    ProducerOrigin, SchemaSupport,
};
use crate::domain::ports::{Clock, TenantGrant};
use crate::domain::pricing::PricingBindingStore;
use crate::domain::work::{FactBindWork, Wakeup, WorkQueue};

/// The first wait of a pending binding before `rating.fact_bind` tries again (DESIGN 14 §3.6).
pub const FIRST_BIND_RETRY: Duration = Duration::seconds(10);

/// What fact intake is built from.
#[domain_model]
pub struct FactIntakeParts {
    /// The gear's database; intake opens its own read and re-bind transactions.
    pub db: Db,
    /// `bss_rating__fact` and `bss_rating__fact_head`.
    pub facts: Arc<dyn FactRepo>,
    /// `bss_rating__child_window`.
    pub children: Arc<dyn ChildRepo>,
    /// `bss_rating__window_schedule`.
    pub schedules: Arc<dyn ScheduleRepo>,
    /// `bss_rating__usage_policy_projection`.
    pub projections: Arc<dyn ProjectionRepo>,
    /// The binding store: accepted bindings become bindings of record.
    pub pricing: PricingBindingStore,
    /// Products' derived declarations.
    pub declarations: DerivedDeclarationStore,
    /// Exceptions of failed children and bindings.
    pub exceptions: ExceptionRegister,
    /// `rating.child_work` and `rating.fact_bind`.
    pub queue: Arc<dyn WorkQueue>,
    /// Rating's service grant per tenant.
    pub grant: Arc<dyn TenantGrant>,
    /// The time source.
    pub clock: Arc<dyn Clock>,
    /// The shard count `S` (DESIGN 14 §3.8).
    pub shards: u16,
}

/// Fact intake.
#[domain_model]
#[derive(Clone)]
pub struct FactIntake {
    parts: Arc<FactIntakeParts>,
}

/// What the apply step was handed: the declaration read before the transaction, if any.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// Nothing was read: a release, or a fact whose version is below the head.
    NotRead,
    /// The fact needs no declaration.
    NotNeeded,
    /// What reading the declaration found.
    Declaration(DeclarationOutcome),
}

/// What binding a pending usage fact again did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebindOutcome {
    /// The fact is bound now.
    Bound,
    /// Its declaration is still unavailable: it waits until `binding_next_check_at`.
    StillPending,
    /// Its declaration conflicts: an exception is open.
    Failed,
    /// Nothing to do: no such fact, not pending, or a newer version arrived meanwhile.
    NotPending,
}

/// The apply step of inbox source `fact`. It keeps the wakeup of what it enqueued, which the
/// caller fires once the inbox transaction has committed; the registered applier's is dropped,
/// and the outbox's periodic sweep finds its items.
struct FactApplier {
    intake: FactIntake,
    prepared: Prepared,
    wakeup: Mutex<Wakeup>,
}

impl FactApplier {
    fn new(intake: FactIntake, prepared: Prepared) -> Self {
        Self {
            intake,
            prepared,
            wakeup: Mutex::new(Wakeup::default()),
        }
    }

    fn take_wakeup(&self) -> Wakeup {
        std::mem::take(&mut *self.wakeup.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

#[async_trait]
impl InboxApplier for FactApplier {
    fn source(&self) -> InboxSource {
        InboxSource::Fact
    }

    async fn apply(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &InboxEntry,
    ) -> Result<(), DomainError> {
        let ReadFact::Accepted(fact) = read_fact(entry.payload()).map_err(|e| invalid(&e))? else {
            return Err(DomainError::InvalidArgument {
                field: "fact",
                detail: "the payload does not read as an applicable fact".to_owned(),
                reason: INVALID_REQUEST,
            });
        };
        let wakeup = self
            .intake
            .apply(tx, scope, entry, &fact, &self.prepared)
            .await?;
        self.wakeup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(wakeup);
        Ok(())
    }
}

fn invalid(error: &impl std::fmt::Display) -> DomainError {
    DomainError::InvalidArgument {
        field: "fact",
        detail: error.to_string(),
        reason: INVALID_REQUEST,
    }
}

impl FactIntake {
    /// Fact intake over `parts`.
    #[must_use]
    pub fn new(parts: FactIntakeParts) -> Self {
        Self {
            parts: Arc::new(parts),
        }
    }

    /// The apply step registered for inbox source `fact`: what a release applies through. It
    /// has read nothing before the transaction.
    #[must_use]
    pub fn applier(&self) -> Arc<dyn InboxApplier> {
        Arc::new(FactApplier::new(self.clone(), Prepared::NotRead))
    }

    /// Receives a fact payload from its source adapter (an event consumer or a recovery read):
    /// reads it against the profile, reads the derived declaration a usage fact needs, and
    /// accepts it through `inbox` in one transaction (feature 06 §2.1).
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] when the payload's key cannot be read (nothing is
    /// stored); the inbox's errors ([`Inbox::accept_one`]).
    pub async fn receive(
        &self,
        inbox: &Inbox,
        payload: Value,
        origin: ProducerOrigin,
    ) -> Result<AcceptOutcome, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-inbox
        let read = read_fact(&payload).map_err(|e| invalid(&e))?;
        let (fact, schema) = match &read {
            ReadFact::Accepted(version) => (version.fact, SchemaSupport::Supported),
            ReadFact::Unsupported { fact } => (*fact, SchemaSupport::Unsupported),
            ReadFact::Refused { fact, .. } => (*fact, SchemaSupport::Supported),
        };
        let key = InboxKey::new(
            fact.tenant_id,
            InboxSource::Fact,
            fact.fact_id.to_string(),
            fact.fact_version,
        )?;
        let mut inbound = InboundFact::new(key, payload, origin, schema);
        if let ReadFact::Refused {
            refusal, detail, ..
        } = &read
        {
            inbound = inbound.refused(*refusal, detail.clone());
        }
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-inbox
        let prepared = match &read {
            ReadFact::Accepted(version) => self.prepare(version).await?,
            ReadFact::Unsupported { .. } | ReadFact::Refused { .. } => Prepared::NotRead,
        };
        let applier = Arc::new(FactApplier::new(self.clone(), prepared));
        let outcome = inbox
            .accept_one_with(inbound, Arc::clone(&applier) as Arc<dyn InboxApplier>)
            .await?;
        applier.take_wakeup().fire();
        Ok(outcome)
    }

    /// The remote reads of a fact version, before its transaction: the derived declaration of a
    /// usage fact's meter, unless the version is below the head (V81: it binds nothing).
    async fn prepare(&self, fact: &FactVersion) -> Result<Prepared, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-declarations
        if fact.kind != FactKind::Usage || fact.obligation == Obligation::Zeroed {
            return Ok(Prepared::NotNeeded);
        }
        let UsageShape::Plan(plan) = usage_shape(fact) else {
            return Ok(Prepared::NotNeeded);
        };
        let current = self
            .current_version(fact.fact.tenant_id, fact.fact.fact_id)
            .await?;
        if current.is_some_and(|current| fact.fact.fact_version < current) {
            return Ok(Prepared::NotRead);
        }
        let outcome = self
            .parts
            .declarations
            .ensure(fact.fact.tenant_id, &plan.meter)
            .await?;
        Ok(Prepared::Declaration(outcome))
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-declarations
    }

    async fn current_version(
        &self,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<i64>, DomainError> {
        let scope = self.parts.grant.scope_for(tenant_id).await?;
        let facts = Arc::clone(&self.parts.facts);
        // A plain read: the apply step re-reads the head under its lock.
        self.parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    Ok(facts
                        .find_head(tx, &scope, tenant_id, fact_id)
                        .await?
                        .map(|head| head.current_version))
                })
            })
            .await
    }

    /// The apply step: steps 3–10 of feature 06 §2.1, inside the inbox transaction.
    async fn apply(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entry: &InboxEntry,
        fact: &FactVersion,
        prepared: &Prepared,
    ) -> Result<Wakeup, DomainError> {
        let now = self.parts.clock.now();
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-insert
        let stored = StoredVersion {
            fact,
            entry_id: entry.entry_id(),
            payload_digest: entry.payload_digest(),
            payload: entry.payload(),
            received_at: entry.received_at(),
        };
        if !self.parts.facts.insert_version(tx, scope, &stored).await? {
            // The inbox applies a new key only, so the version can be stored already only when an
            // operator releases a digest conflict to `apply`: a version is immutable (#5516), so
            // the conflict can only be discarded.
            return Err(DomainError::InvalidArgument {
                field: "fact",
                detail: format!(
                    "fact {} v{} is stored with another digest; a fact version is immutable",
                    fact.fact.fact_id, fact.fact.fact_version
                ),
                reason: INVALID_REQUEST,
            });
        }
        let (tenant_id, fact_id) = (fact.fact.tenant_id, fact.fact.fact_id);
        let previous = self
            .parts
            .facts
            .lock_head(tx, scope, tenant_id, fact_id)
            .await?;
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-insert
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-current-only
        if previous
            .as_ref()
            .is_some_and(|head| fact.fact.fact_version < head.current_version)
        {
            return Ok(Wakeup::default());
        }
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-current-only
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-bindings
        for term in &fact.terms {
            self.parts
                .pricing
                .store_accepted(tx, scope, tenant_id, term)
                .await?;
        }
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-bindings
        let mut work = Vec::new();
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-new-version
        if previous.is_some() {
            // @cpt-begin:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-bump
            work.extend(
                self.parts
                    .children
                    .bump_fact(tx, scope, tenant_id, fact_id, now)
                    .await?,
            );
            // @cpt-end:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-bump
        }
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-new-version
        let shard = previous
            .as_ref()
            .map_or_else(|| shard_of(fact_id, self.parts.shards), |head| head.shard);
        let mut head = FactHead {
            tenant_id,
            fact_id,
            shard,
            kind: fact.kind,
            current_version: fact.fact.fact_version,
            binding: BindingState::Bound,
            bound_version: None,
            binding_next_check_at: None,
            expected_manifest_digest: None,
            rollup_due_at: None,
            currency: fact.currency.clone(),
            received_at: entry.received_at(),
            updated_at: now,
        };
        let expected = expected_children(fact, now)?;
        let declaration = match prepared {
            Prepared::Declaration(outcome) => Some(outcome),
            Prepared::NotRead | Prepared::NotNeeded => None,
        };
        self.bind(
            tx,
            scope,
            fact,
            &expected,
            declaration,
            &mut head,
            &mut work,
        )
        .await?;
        let mut wakeup = Wakeup::default();
        if let BindingState::Pending(_) = head.binding {
            let item = FactBindWork { tenant_id, fact_id };
            wakeup.merge(self.parts.queue.enqueue_fact_bind(tx, item).await?);
        }
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-commit
        if previous.is_some() {
            self.parts.facts.update_head(tx, scope, &head).await?;
        } else {
            self.parts.facts.insert_head(tx, scope, &head).await?;
        }
        work.sort_by_key(|item| item.child_id);
        work.dedup();
        // @cpt-begin:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-enqueue
        wakeup.merge(self.parts.queue.enqueue_children(tx, &work).await?);
        // @cpt-end:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-enqueue
        Ok(wakeup)
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-commit
    }

    /// Steps 5–8: the children, schedule and binding state of the current version.
    #[expect(
        clippy::too_many_arguments,
        reason = "the transaction, its scope, the version, its expected set, the declaration read \
                  and the two outputs are each distinct"
    )]
    async fn bind(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        fact: &FactVersion,
        expected: &Expected,
        declaration: Option<&DeclarationOutcome>,
        head: &mut FactHead,
        work: &mut Vec<ChildWork>,
    ) -> Result<(), DomainError> {
        let now = head.updated_at;
        let Some(plan) = &expected.schedule else {
            // A recurring, one-time or zeroed fact, or a usage fact intake fails closed or that
            // names no term: its children are created now and it is bound.
            // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-recurring
            self.create_intake_children(tx, scope, fact, expected, head.shard, work)
                .await?;
            // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-recurring
            mark_bound(fact, expected, head, now);
            return Ok(());
        };
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-bind-pending
        let stored = match declaration {
            Some(DeclarationOutcome::Stored(stored)) => stored,
            Some(DeclarationOutcome::Conflict { detail }) => {
                head.binding = BindingState::Failed(FailedBinding::DerivedDeclarationConflict);
                head.binding_next_check_at = None;
                let reason = ExceptionReason::from_code("derived_declaration_conflict");
                let key = ExceptionKey::new(
                    fact.fact.tenant_id,
                    SubjectKind::Fact,
                    SubjectRef::from(fact.fact.fact_id),
                    reason,
                );
                self.parts
                    .exceptions
                    .record(tx, scope, &key, detail)
                    .await?;
                return Ok(());
            }
            Some(DeclarationOutcome::Unavailable { .. }) | None => {
                head.binding = BindingState::Pending(PendingBinding::DerivedDeclarationUnavailable);
                head.binding_next_check_at = Some(backoff(now, head.received_at));
                return Ok(());
            }
        };
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-bind-pending
        self.bind_usage(tx, scope, fact, expected, plan, stored, head)
            .await?;
        mark_bound(fact, expected, head, now);
        Ok(())
    }

    /// The projection and the schedule of a usage fact whose declaration is stored.
    #[expect(
        clippy::too_many_arguments,
        reason = "the transaction, its scope, the version, its expected set, plan and declaration, \
                  and the head are each distinct"
    )]
    async fn bind_usage(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        fact: &FactVersion,
        expected: &Expected,
        plan: &UsagePlan,
        declaration: &StoredDeclaration,
        head: &FactHead,
    ) -> Result<(), DomainError> {
        let row = ProjectionRow {
            tenant_id: fact.fact.tenant_id,
            subscription_id: fact.subscription_id,
            item_id: fact.item_id,
            valid_from: fact.served.start,
            valid_to: fact.served.end,
            fact_version: fact.fact.fact_version,
            fact_id: fact.fact.fact_id,
            sub_line_key: fact.sub_line_key.clone(),
            policy_id: plan.policy.policy_id,
            policy_version: plan.policy.version,
            policy_digest: plan.policy.digest.clone(),
            rating_window: plan.policy.rating_window,
            meter: plan.meter.clone(),
            inputs: declaration.declaration.inputs.clone(),
            binding_digest: plan.binding_digest.clone(),
        };
        self.parts
            .projections
            .insert(tx, scope, &row, head.updated_at)
            .await?;
        // @cpt-begin:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-manifest
        let schedule = NewSchedule {
            tenant_id: fact.fact.tenant_id,
            fact_id: fact.fact.fact_id,
            item_id: fact.item_id,
            shard: head.shard,
            fact_version: fact.fact.fact_version,
            window_policy: plan.policy.rating_window,
            usage_policy_id: plan.policy.policy_id,
            usage_policy_version: plan.policy.version,
            usage_policy_digest: plan.policy.digest.clone(),
            next_due_at: head.updated_at,
            expected_count: expected.children.len(),
        };
        self.parts
            .schedules
            .upsert(tx, scope, &schedule, head.updated_at)
            .await
        // @cpt-end:cpt-cf-bss-rating-algo-schedule-new-version:p1:inst-nv-manifest
    }

    /// The children intake creates itself: a `period_line`, a usage fact's billing-period child,
    /// or a child failed closed with its exception.
    async fn create_intake_children(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        fact: &FactVersion,
        expected: &Expected,
        shard: i16,
        work: &mut Vec<ChildWork>,
    ) -> Result<(), DomainError> {
        let now = self.parts.clock.now();
        for child in expected.children.iter().filter(|child| child.at_intake) {
            let row = child.new_child(fact, shard);
            // @cpt-begin:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-pending
            let created = self
                .parts
                .children
                .insert_if_absent(tx, scope, &row, now)
                .await?;
            // @cpt-end:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-pending
            if let Some(failure) = child.failure {
                let key = ExceptionKey::new(
                    fact.fact.tenant_id,
                    SubjectKind::Child,
                    SubjectRef::from(row.child_id),
                    ExceptionReason::from_code(failure.as_str()),
                );
                let detail = format!(
                    "fact {} v{}: {}",
                    fact.fact.fact_id,
                    fact.fact.fact_version,
                    failure.as_str()
                );
                self.parts
                    .exceptions
                    .record(tx, scope, &key, &detail)
                    .await?;
            } else if created && row.status == ChildStatus::Pending {
                work.push(ChildWork {
                    tenant_id: row.tenant_id,
                    child_id: row.child_id,
                    subscription_id: row.subscription_id,
                });
            }
        }
        Ok(())
    }

    /// Binds a pending usage fact again (from `rating.fact_bind`): reads the declaration outside
    /// any transaction, then, under the head's lock, binds the head's current version unless a
    /// newer version or another binding state arrived meanwhile ([14 §3.6] re-bind).
    ///
    /// [14 §3.6]: ../../../../docs/features/06-fact-scheduling.md#contract-14-3-6-f06
    ///
    /// # Errors
    ///
    /// The service grant's errors; a storage failure; a stored payload that no longer reads.
    pub async fn rebind(
        &self,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<RebindOutcome, DomainError> {
        let scope = self.parts.grant.scope_for(tenant_id).await?;
        let Some((version, fact)) = self.pending_version(&scope, tenant_id, fact_id).await? else {
            return Ok(RebindOutcome::NotPending);
        };
        let declaration = match usage_shape(&fact) {
            UsageShape::Plan(plan) => {
                self.parts
                    .declarations
                    .ensure(tenant_id, &plan.meter)
                    .await?
            }
            _ => return Ok(RebindOutcome::NotPending),
        };
        let (intake, fact) = (self.clone(), Arc::new(fact));
        let (outcome, wakeup) = self
            .parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    intake
                        .rebind_locked(tx, &scope, &fact, version, &declaration)
                        .await
                })
            })
            .await?;
        wakeup.fire();
        Ok(outcome)
    }

    async fn pending_version(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        fact_id: Uuid,
    ) -> Result<Option<(i64, FactVersion)>, DomainError> {
        let (facts, scope) = (Arc::clone(&self.parts.facts), scope.clone());
        let found = self
            .parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let Some(head) = facts.lock_head(tx, &scope, tenant_id, fact_id).await? else {
                        return Ok::<_, DomainError>(None);
                    };
                    if !matches!(head.binding, BindingState::Pending(_)) {
                        return Ok(None);
                    }
                    let payload = facts
                        .version_payload(tx, &scope, tenant_id, fact_id, head.current_version)
                        .await?;
                    Ok(payload.map(|payload| (head.current_version, payload)))
                })
            })
            .await?;
        let Some((version, payload)) = found else {
            return Ok(None);
        };
        match read_fact(&payload).map_err(|e| invalid(&e))? {
            ReadFact::Accepted(fact) => Ok(Some((version, *fact))),
            ReadFact::Unsupported { .. } | ReadFact::Refused { .. } => Err(
                DomainError::CorruptRow("bss_rating__fact.payload".to_owned()),
            ),
        }
    }

    async fn rebind_locked(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        fact: &FactVersion,
        version: i64,
        declaration: &DeclarationOutcome,
    ) -> Result<(RebindOutcome, Wakeup), DomainError> {
        let (tenant_id, fact_id) = (fact.fact.tenant_id, fact.fact.fact_id);
        let Some(mut head) = self
            .parts
            .facts
            .lock_head(tx, scope, tenant_id, fact_id)
            .await?
        else {
            return Ok((RebindOutcome::NotPending, Wakeup::default()));
        };
        if head.current_version != version || !matches!(head.binding, BindingState::Pending(_)) {
            return Ok((RebindOutcome::NotPending, Wakeup::default()));
        }
        let now = self.parts.clock.now();
        head.updated_at = now;
        let expected = expected_children(fact, now)?;
        let mut work = Vec::new();
        self.bind(
            tx,
            scope,
            fact,
            &expected,
            Some(declaration),
            &mut head,
            &mut work,
        )
        .await?;
        // A head still pending is retried by the wake scanner of feature 06b at
        // `binding_next_check_at`, never by re-enqueueing itself, so the queue does not spin.
        self.parts.facts.update_head(tx, scope, &head).await?;
        let wakeup = self.parts.queue.enqueue_children(tx, &work).await?;
        let outcome = match head.binding {
            BindingState::Bound => RebindOutcome::Bound,
            BindingState::Pending(_) => RebindOutcome::StillPending,
            BindingState::Failed(_) => RebindOutcome::Failed,
        };
        Ok((outcome, wakeup))
    }
}

/// Marks the head bound with its manifest and roll-up time (steps 7–8).
fn mark_bound(fact: &FactVersion, expected: &Expected, head: &mut FactHead, now: OffsetDateTime) {
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-manifest
    head.binding = BindingState::Bound;
    head.bound_version = Some(fact.fact.fact_version);
    head.binding_next_check_at = None;
    head.expected_manifest_digest = Some(expected.manifest_digest());
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-manifest
    // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-rollup-due
    head.rollup_due_at = match (fact.obligation, fact.kind, fact.timing) {
        // A zeroed version rolls up at once: its result is a complete empty replacement.
        (Obligation::Zeroed, _, _) => Some(now),
        (Obligation::Active, FactKind::Usage, _)
        | (Obligation::Active, FactKind::Recurring, Timing::Arrears) => Some(fact.served.end),
        (Obligation::Active, FactKind::Recurring, Timing::Advance)
        | (Obligation::Active, FactKind::OneTime, _) => None,
    };
    // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-rollup-due
}

/// The next retry of a pending binding: `now + min(5 min, max(10 s, now − received_at))`, so the
/// wait doubles without a counter (feature 06 §2.3 `inst-wk-bind`).
#[must_use]
pub fn backoff(now: OffsetDateTime, received_at: OffsetDateTime) -> OffsetDateTime {
    let wait = (now - received_at).clamp(FIRST_BIND_RETRY, Duration::minutes(5));
    now.checked_add(wait).unwrap_or(now)
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;

//! Submission, publish changes, the approval queue, generation-bound votes and the policy.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-publish-changes-selection:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-stale-refresh-generation:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-generation-and-duplicate-vote:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-unit-contended:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-terminal-audit-event:p1
use super::{
    dto::{
        PriceBookDto, PricingApprovalPolicyDto, PricingApprovalPolicyPut,
        PricingApprovalUnitCounts, PricingApprovalUnitDto, PricingApprovalUnitKindCounts,
        PricingApprovalUnitList, PricingApprovalUnitStateCounts, PricingEffectivePolicyDto,
        PricingPlanRevisionDto, PricingPlanRevisionSubmitReceipt, PricingPriceBookEntryDto,
        PricingPriceDto, PricingProposedPrice, PricingPublishChanges, PricingPublishChangesRequest,
        PricingSubmitReceipt, PricingVoteReceipt, PricingVoteRequest,
    },
    plans,
    support::{self, DoorError, approval_failure},
};
use crate::api::rest::closed_sets::PricingVoteOutcome;
use crate::{
    domain::price::{self, PriceState},
    infra::{
        approval_kinds::{Kind, Subject},
        events::{
            self, ApprovalUnitDecided, PlanRevisionPublished, PricesPublished, PublishedPrice,
            TxOutbox,
        },
        plan_revisions::PlanRevisionSubject,
        prices::{PricesSubject, Release},
        storage::{
            RepoError, entity,
            repo::{
                approval_repo::{self, PricingApprovalStore},
                book_repo, plan_item_repo, plan_revision_repo, price_book_entry_repo, price_repo,
            },
        },
    },
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bss_approval::{ApproveOutcome, Engine, RejectOutcome, Store, SubmitRequest, Unit, UnitState};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{
    Db, DbTx,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Everything one keyed approval command carries into its transaction.
#[derive(Clone)]
pub struct Command {
    pub scope: AccessScope,
    pub ctx: SecurityContext,
    pub hub: Arc<toolkit::ClientHub>,
    /// The sink the decision's transaction enqueues its events through; they wake the outbox's
    /// sequencer once it commits (D-455).
    pub outbox: crate::infra::events::EventSink,
    /// The clock the command reads its instant from ([`Command::now`]).
    pub clock: Arc<dyn crate::infra::reference_work::Clock>,
    pub correlation: Uuid,
    pub key: String,
    pub digest: Vec<u8>,
    /// Compiled once per request for the unit flags (D-497).
    pub approve_scope: AccessScope,
    pub submit_scope: AccessScope,
}
impl Command {
    fn tenant(&self) -> Uuid {
        self.ctx.subject_tenant_id()
    }
    /// The instant the command writes and answers, as storage keeps it (D-453).
    fn now(&self) -> OffsetDateTime {
        crate::infra::storage::stored_instant(self.clock.now())
    }
    fn store(&self) -> PricingApprovalStore {
        PricingApprovalStore {
            scope: AccessScope::for_tenant(self.tenant()),
            tenant_id: self.tenant(),
        }
    }
}
/// The three decisions a unit can take from a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vote {
    Approve,
    Reject,
    Withdraw,
}
impl Vote {
    const fn path(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Withdraw => "withdraw",
        }
    }
}

/// The unit as `reader` reads it: its decisions, and whether `reader` may approve it, judged over
/// its stored items' authors and its decisions (D-471), two statements; the items' content is not
/// read. Every answer that carries a unit builds it here or through
/// [`PricingApprovalUnitDto::of`] over rows already read.
async fn unit_dto(
    tx: &DbTx<'_>,
    store: &PricingApprovalStore,
    unit: Unit,
    reader: Uuid,
    approve_scope: &AccessScope,
    submit_scope: &AccessScope,
) -> Result<PricingApprovalUnitDto, DoorError> {
    let (authors, decisions) = rows_of(tx, store, unit.id).await?;
    let mut dto = PricingApprovalUnitDto::of(
        unit,
        &authors,
        decisions,
        reader,
        approve_scope,
        submit_scope,
    )?;
    name_books(tx, store.tenant_id, std::slice::from_mut(&mut dto)).await?;
    Ok(dto)
}
/// Book ids a snapshot names: every `book_id` whose value is an id, at any depth.
fn collect_book_ids(value: &serde_json::Value, out: &mut BTreeSet<Uuid>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(raw)) = map.get("book_id")
                && let Ok(id) = Uuid::parse_str(raw)
            {
                out.insert(id);
            }
            for child in map.values() {
                collect_book_ids(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_book_ids(item, out);
            }
        }
        _ => {}
    }
}
fn attach_book_identity(value: &mut serde_json::Value, books: &BTreeMap<Uuid, serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            let named = map
                .get("book_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| Uuid::parse_str(raw).ok());
            // A `book` already beside the id is the snapshot's own and is kept.
            if let Some(id) = named
                && let Some(book) = books.get(&id)
            {
                map.entry("book").or_insert_with(|| book.clone());
            }
            for child in map.values_mut() {
                attach_book_identity(child, books);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                attach_book_identity(item, books);
            }
        }
        _ => {}
    }
}
/// Put `book { id, code, name, currency }` beside every book id a served snapshot names (D-516).
/// One grouped read for the page, and none when no snapshot names a book. A book the tenant no
/// longer holds leaves `book` absent, so a decided unit stays readable after its book is deleted.
/// The stored snapshot and its fingerprint are unchanged: this is what the review reads.
/// # Errors
/// Storage failures.
async fn name_books(
    tx: &impl DBRunner,
    tenant: Uuid,
    units: &mut [PricingApprovalUnitDto],
) -> Result<(), DoorError> {
    let mut ids = BTreeSet::new();
    for unit in units.iter() {
        collect_book_ids(&unit.snapshot, &mut ids);
    }
    if ids.is_empty() {
        return Ok(());
    }
    let id_list: Vec<Uuid> = ids.iter().copied().collect();
    let found =
        book_repo::find_many(tx, &AccessScope::for_tenant(tenant), tenant, &id_list).await?;
    let books: BTreeMap<Uuid, serde_json::Value> = found
        .into_iter()
        .map(|book| {
            (
                book.id,
                serde_json::json!({
                    "id": book.id,
                    "code": book.code,
                    "name": book.name,
                    "currency": book.currency,
                }),
            )
        })
        .collect();
    for unit in units {
        attach_book_identity(&mut unit.snapshot, &books);
    }
    Ok(())
}
/// One unit's item authors and decisions, one statement each: what its receipt's flag and, for a
/// plan revision, its progress are built from.
async fn rows_of(
    tx: &DbTx<'_>,
    store: &PricingApprovalStore,
    unit: Uuid,
) -> Result<(Vec<Uuid>, Vec<bss_approval::Decision>), DoorError> {
    let authors = approval_repo::item_authors_of_units(tx, &store.scope, store.tenant_id, &[unit])
        .await?
        .remove(&unit)
        .unwrap_or_default();
    let decisions = store.decisions(tx, unit).await.map_err(approval_failure)?;
    Ok((authors, decisions))
}
/// The authors of `items`, in their order.
fn authors_of(items: &[bss_approval::ItemRef]) -> Vec<Uuid> {
    items.iter().map(|i| i.created_by).collect()
}
async fn load_unit(
    tx: &DbTx<'_>,
    store: &PricingApprovalStore,
    id: Uuid,
) -> Result<Unit, DoorError> {
    store
        .unit(tx, id)
        .await
        .map_err(approval_failure)?
        .ok_or_else(|| support::missing_what("approval_unit").into())
}
/// The prices of a unit's `items`, in their order, each with its entry's model (D-427): the prices
/// and their entries in two statements whatever the number of items (PS-39).
async fn prices_of(
    tx: &DbTx<'_>,
    store: &PricingApprovalStore,
    items: &[bss_approval::ItemRef],
) -> Result<Vec<PricingPriceDto>, DoorError> {
    let scope = AccessScope::for_tenant(store.tenant_id);
    let ids: Vec<Uuid> = items.iter().map(|i| i.item_id).collect();
    let mut found: BTreeMap<Uuid, entity::price::Model> =
        price_repo::find_many(tx, &scope, store.tenant_id, &ids)
            .await?
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
    let entry_ids: Vec<Uuid> = found
        .values()
        .map(|m| m.price_book_entry_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let models: BTreeMap<Uuid, String> =
        price_book_entry_repo::find_many(tx, &scope, store.tenant_id, &entry_ids)
            .await?
            .into_iter()
            .map(|e| (e.id, e.model))
            .collect();
    let mut prices = Vec::new();
    for item in items {
        if let Some(m) = found.remove(&item.item_id) {
            let model = models
                .get(&m.price_book_entry_id)
                .ok_or_else(|| RepoError::CorruptRow(format!("price {} has no entry", m.id)))?;
            prices.push(PricingPriceDto::of(m, model)?);
        }
    }
    Ok(prices)
}

/// `ApprovalUnitDecided` for a unit that just reached its terminal state, in the deciding
/// transaction: the current-generation voters and the acting principal.
async fn decided(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    store: &PricingApprovalStore,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let unit = load_unit(tx, store, id).await?;
    let mut actors: Vec<Uuid> = store
        .decisions(tx, unit.id)
        .await
        .map_err(approval_failure)?
        .into_iter()
        .filter(|d| !d.stale)
        .map(|d| d.actor)
        .collect();
    actors.push(cmd.ctx.subject_id());
    actors.sort_unstable();
    actors.dedup();
    let event = ApprovalUnitDecided {
        tenant_id: unit.tenant_id,
        unit_id: unit.id,
        kind: unit.kind.clone(),
        state: unit.state.as_str().into(),
        generation: unit.generation,
        actors,
    };
    events::enqueue(outbox, tx, &event, now).await?;
    Ok(())
}
/// The domain event of an applied unit, by its kind, in the apply transaction. A plan revision
/// approved before its sale date was scheduled, not published: its `PlanRevisionPublished` is
/// its switch's (D-449, D-450).
async fn published(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    store: &PricingApprovalStore,
    subject: &Subject,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    match subject {
        Subject::Prices(s) => prices_published(tx, outbox, cmd, store, s, id, now).await,
        Subject::PlanRevision(s) if s.published_now() => {
            plan_revision_published(tx, outbox, cmd, store, s, id, now).await
        }
        Subject::PlanRevision(_) => Ok(()),
    }
}
/// `PlanRevisionPublished` for an applied `plan_revision` unit: the revision now published, the
/// one its apply superseded, and the book it reads.
async fn plan_revision_published(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    store: &PricingApprovalStore,
    subject: &PlanRevisionSubject,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let unit = load_unit(tx, store, id).await?;
    let scope = AccessScope::for_tenant(store.tenant_id);
    let r = plan_revision_repo::find(tx, &scope, store.tenant_id, unit.ref_id)
        .await?
        .ok_or_else(|| {
            RepoError::CorruptRow(format!("unit {} lost revision {}", unit.id, unit.ref_id))
        })?;
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-3
    let event = PlanRevisionPublished {
        tenant_id: unit.tenant_id,
        plan_id: r.plan_id,
        revision_id: r.id,
        rev_no: r.rev_no,
        book_id: r.book_id,
        superseded_revision_id: subject.superseded(),
        unit_id: unit.id,
        actor_ref: cmd.ctx.subject_id(),
    };
    events::enqueue(outbox, tx, &event, now).await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-3
    Ok(())
}
/// `PricesPublished` for an applied `prices` unit: every price whose window or state the apply
/// changed, as the apply left it (D-520, D-521). These are the unit's prices with the window their
/// chain was approved with, each price before them whose end the chain re-closed or re-opened,
/// and each price the unit cancelled (`cancelled`) or ended (its new end). A `cancel` or `end` row
/// is a record of the change, not a price, so the event never lists it.
async fn prices_published(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    store: &PricingApprovalStore,
    subject: &PricesSubject,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let unit = load_unit(tx, store, id).await?;
    let scope = AccessScope::for_tenant(store.tenant_id);
    let mut ids: BTreeSet<Uuid> = store
        .items(tx, unit.id)
        .await
        .map_err(approval_failure)?
        .iter()
        .map(|i| i.item_id)
        .collect();
    ids.extend(subject.moved());
    let ids: Vec<Uuid> = ids.into_iter().collect();
    // The unit's prices and the prices it moved in ONE statement (PS-39).
    let mut found: BTreeMap<Uuid, entity::price::Model> =
        price_repo::find_many(tx, &scope, store.tenant_id, &ids)
            .await?
            .into_iter()
            .map(|m| (m.id, m))
            .collect();
    let mut prices = Vec::new();
    for price_id in ids {
        let m = found.remove(&price_id).ok_or_else(|| {
            RepoError::CorruptRow(format!("unit {} lost price {price_id}", unit.id))
        })?;
        if !price_repo::is_price(&m) {
            continue;
        }
        prices.push(PublishedPrice {
            price_id: m.id,
            price_book_entry_id: m.price_book_entry_id,
            dim_value: m.dim_value,
            effective_from: m.effective_from.to_string(),
            effective_to: m.effective_to.map(|d| d.to_string()),
            eligibility: m.eligibility,
            state: Some(m.state),
        });
    }
    let event = PricesPublished {
        tenant_id: unit.tenant_id,
        book_id: unit.ref_id,
        unit_id: unit.id,
        prices,
        actor_ref: cmd.ctx.subject_id(),
    };
    events::enqueue(outbox, tx, &event, now).await?;
    Ok(())
}

/// An engine refusal as the door answers it: a Products refusal the subject met while judging
/// keeps its own status and code; everything else maps through [`approval_failure`].
fn refusal(subject: &Subject) -> impl Fn(bss_approval::ApprovalError) -> DoorError + '_ {
    move |error| {
        subject
            .take_refusal()
            .map_or_else(|| approval_failure(error), DoorError::Api)
    }
}
/// What one submission names besides its items: the aggregate the unit references, the shared
/// start, the submitter's note (D-464; a single price's submit sends none) and the submission time.
struct Submission {
    ref_id: Uuid,
    common_effective_date: Option<time::Date>,
    note: Option<String>,
    now: OffsetDateTime,
}
/// Record one unit over the items through the kind's subject, applying it at once under quorum
/// zero with its domain event and `ApprovalUnitDecided`. The caller answers the key.
async fn record(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    subject: &Subject,
    submission: Submission,
    ids: &[Uuid],
) -> Result<bss_approval::Submitted, DoorError> {
    let store = cmd.store();
    let policy = approval_repo::read_policy(tx, &store.scope, cmd.tenant()).await?;
    let submitted = Engine::submit(
        &store,
        subject,
        tx,
        SubmitRequest {
            tenant_id: cmd.tenant(),
            ref_id: submission.ref_id,
            item_ids: ids,
            actor: cmd.ctx.subject_id(),
            policy: &policy,
            common_effective_date: submission.common_effective_date,
            note: submission.note.as_deref(),
            now: submission.now,
        },
    )
    .await
    .map_err(refusal(subject))?;
    let unit = &submitted.unit;
    support::audit(
        tx,
        &cmd.ctx,
        cmd.correlation,
        "approval.submitted",
        unit.id,
        unit.version,
    )
    .await?;
    if submitted.applied {
        support::audit(
            tx,
            &cmd.ctx,
            cmd.correlation,
            "approval.approved",
            unit.id,
            unit.version,
        )
        .await?;
        published(tx, outbox, cmd, &store, subject, unit.id, submission.now).await?;
        decided(tx, outbox, cmd, &store, unit.id, submission.now).await?;
    }
    Ok(submitted)
}
/// Record a `prices` unit with the submitter's `note` and answer the key with the unit and its
/// prices.
async fn record_prices(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    endpoint: &str,
    (subject, note): (PricesSubject, Option<String>),
    ids: &[Uuid],
) -> Result<Response, DoorError> {
    let submission = Submission {
        ref_id: subject.book_id,
        common_effective_date: subject.common_effective_date,
        note,
        now: subject.now,
    };
    let submitted = record(tx, outbox, cmd, &Subject::Prices(subject), submission, ids).await?;
    let store = cmd.store();
    // The unit's items and decisions, read once each: its prices and its unit are built from
    // them (the phase 9 review's R44).
    let items = store
        .items(tx, submitted.unit.id)
        .await
        .map_err(approval_failure)?;
    let decisions = store
        .decisions(tx, submitted.unit.id)
        .await
        .map_err(approval_failure)?;
    let prices = prices_of(tx, &store, &items).await?;
    let mut unit = PricingApprovalUnitDto::of(
        submitted.unit,
        &authors_of(&items),
        decisions,
        cmd.ctx.subject_id(),
        &cmd.approve_scope,
        &cmd.submit_scope,
    )?;
    name_books(tx, cmd.tenant(), std::slice::from_mut(&mut unit)).await?;
    let receipt = PricingSubmitReceipt {
        applied: submitted.applied,
        unit,
        prices,
    };
    support::answer(
        tx,
        cmd.tenant(),
        endpoint,
        &cmd.key,
        StatusCode::CREATED,
        &receipt,
        None,
    )
    .await
}

/// `POST /prices/{id}/submit`: one price alone; a pair half is refused, publish the pair instead.
/// # Errors
/// Returns the canonical refusal.
pub async fn submit_price(db: &Db, cmd: Command, id: Uuid) -> Result<Response, CanonicalError> {
    support::retry_unit_capture(db, || async {
        if let Some(replay) = support::replay(
            db,
            cmd.tenant(),
            &format!("/bss-pricing/v1/prices/{id}/submit"),
            &cmd.key,
            &cmd.digest,
        )
        .await?
        {
            return Ok(replay);
        }

        let observations = observe_prices(db, &cmd, &[id]).await?;
        let sink = cmd.outbox.clone();
        let cmd = cmd.clone();
        support::unit_transaction_observed_with_events(db, &sink, move |tx, outbox| {
            let observations = observations.clone();
            let cmd = cmd.clone();
            Box::pin(async move {
                let endpoint = format!("/bss-pricing/v1/prices/{id}/submit");
                if let Some(replay) =
                    support::claim(tx, cmd.tenant(), &endpoint, &cmd.key, &cmd.digest).await?
                {
                    return Ok(replay);
                }
                observations.check_local(tx, cmd.tenant()).await?;
                let price = price_repo::find(tx, &cmd.scope, cmd.tenant(), id)
                    .await?
                    .ok_or_else(|| support::missing_what("price"))?;
                if price.paired_price_id.is_some() {
                    return Err(support::invalid("price_ids", "PAIR_SPLIT").into());
                }
                let entry = price_book_entry_repo::find(
                    tx,
                    &AccessScope::for_tenant(cmd.tenant()),
                    cmd.tenant(),
                    price.price_book_entry_id,
                )
                .await?
                .ok_or_else(support::missing_entry)?;
                let mut subject =
                    PricesSubject::new(cmd.ctx.clone(), cmd.hub.clone(), entry.book_id, cmd.now());
                subject.meter_observations = observations;
                record_prices(tx, &outbox, &cmd, &endpoint, (subject, None), &[id]).await
            })
        })
        .await
    })
    .await
    .map_err(Into::into)
}

/// `POST /plan-revisions/{id}/submit`: one unlocked draft revision whose checks are all green
/// becomes a `plan_revision` unit carrying the submitter's `note` (D-464, capped by the door);
/// quorum zero publishes it in the same transaction. The receipt's revision says when it was
/// submitted and, applied at once, approved (D-461), and a pending one its vote progress (D-462).
/// # Errors
/// 404 for a revision the tenant does not hold; 409 `REVISION_NOT_DRAFT`; 400
/// `REVISION_CHECKS_RED` with the red checks and no unit; 409 `ROW_LOCKED_PENDING` for a lost
/// lock; 503 when the registry cannot answer.
pub async fn submit_revision(
    db: &Db,
    cmd: Command,
    id: Uuid,
    note: Option<String>,
) -> Result<Response, CanonicalError> {
    support::retry_unit_capture(db, || async {
        if let Some(replay) = support::replay(
            db,
            cmd.tenant(),
            &format!("/bss-pricing/v1/plan-revisions/{id}/submit"),
            &cmd.key,
            &cmd.digest,
        )
        .await?
        {
            return Ok(replay);
        }

        let observations = observe_revision(db, &cmd, id).await?;
        let sink = cmd.outbox.clone();
        let cmd = cmd.clone();
        let note = note.clone();
        support::unit_transaction_observed_with_events(db, &sink, move |tx, outbox| {
            let observations = observations.clone();
            let (cmd, note) = (cmd.clone(), note.clone());
            Box::pin(async move {
                let endpoint = format!("/bss-pricing/v1/plan-revisions/{id}/submit");
                if let Some(replay) =
                    support::claim(tx, cmd.tenant(), &endpoint, &cmd.key, &cmd.digest).await?
                {
                    return Ok(replay);
                }
                observations.check_local(tx, cmd.tenant()).await?;
                let r = plans::find_revision(tx, &cmd.scope, cmd.tenant(), id).await?;
                if !plans::open_draft(&r) {
                    return Err(support::conflict("REVISION_NOT_DRAFT").into());
                }
                let now = cmd.now();
                // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-4
                let mut subject =
                    PlanRevisionSubject::new(cmd.ctx.clone(), cmd.hub.clone(), id, now);
                subject.meter_observations = observations;
                let submission = Submission {
                    ref_id: id,
                    common_effective_date: None,
                    note,
                    now,
                };
                let submitted = record(
                    tx,
                    &outbox,
                    &cmd,
                    &Subject::PlanRevision(subject),
                    submission,
                    &[id],
                )
                .await?;
                // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-4
                let children = AccessScope::for_tenant(cmd.tenant());
                let r = plans::find_revision(tx, &children, cmd.tenant(), id).await?;
                let items = plan_item_repo::for_revision(tx, &children, cmd.tenant(), id).await?;
                // A write answers what it wrote (D-453): the unit in hand names its instants (D-461)
                // and, still pending, its progress (D-462). Its item authors and its decisions are
                // read once each, and both the progress and the unit are built from them (the phase
                // 9 review's R45).
                let (authors, decisions) = rows_of(tx, &cmd.store(), submitted.unit.id).await?;
                let approval = plans::progress_of(&submitted.unit, &decisions);
                let revision = PricingPlanRevisionDto::of(&r, items)?
                    .with_units(&plans::instants_of(&submitted.unit), approval);
                let mut unit = PricingApprovalUnitDto::of(
                    submitted.unit,
                    &authors,
                    decisions,
                    cmd.ctx.subject_id(),
                    &cmd.approve_scope,
                    &cmd.submit_scope,
                )?;
                name_books(tx, cmd.tenant(), std::slice::from_mut(&mut unit)).await?;
                let receipt = PricingPlanRevisionSubmitReceipt {
                    applied: submitted.applied,
                    unit,
                    revision,
                };
                support::answer(
                    tx,
                    cmd.tenant(),
                    &endpoint,
                    &cmd.key,
                    StatusCode::CREATED,
                    &receipt,
                    None,
                )
                .await
            })
        })
        .await
    })
    .await
    .map_err(Into::into)
}

/// Every draft price of a book with its entry, chain and predecessor, in proposal order.
async fn proposals(
    tx: &impl DBRunner,
    tenant: Uuid,
    book: Uuid,
) -> Result<Vec<PricingProposedPrice>, DoorError> {
    let children = AccessScope::for_tenant(tenant);
    let entries = price_book_entry_repo::for_book(tx, &children, tenant, book).await?;
    // The book's prices in ONE statement, matched by id through maps (PS-16).
    let mut grouped = price_repo::by_entry(
        price_repo::for_entries(
            tx,
            &children,
            tenant,
            &entries.iter().map(|p| p.id).collect::<Vec<_>>(),
        )
        .await?,
    );
    let mut stored: BTreeMap<Uuid, entity::price::Model> = BTreeMap::new();
    // The chains are prices only; a draft `cancel` or `end` is listed beside them (D-520, D-521).
    let mut prices = Vec::new();
    let mut changes = Vec::new();
    for p in &entries {
        let model = price_book_entry_repo::model_of(p)?;
        for m in grouped.remove(&p.id).unwrap_or_default() {
            if price_repo::is_price(&m) {
                prices.push(price_repo::to_domain(&m, model)?);
            } else {
                changes.push(price_repo::to_domain(&m, model)?);
            }
            stored.insert(m.id, m);
        }
    }
    let policies =
        crate::infra::storage::repo::usage_policy_repo::for_entries(tx, tenant, &entries).await?;
    let by_id: BTreeMap<Uuid, &entity::price_book_entry::Model> =
        entries.iter().map(|p| (p.id, p)).collect();
    let owners: Vec<(Uuid, Uuid)> = entries.iter().map(|p| (p.id, p.book_id)).collect();
    let mut out = Vec::new();
    let mut drafts = price::proposed_prices(book, &owners, &prices);
    drafts.extend(price::proposed_prices(book, &owners, &changes));
    drafts.sort_by_key(|r| (r.effective_from, r.price_book_entry_id, r.version_no));
    for r in drafts {
        let Some(m) = stored.get(&r.id) else {
            continue;
        };
        if m.pending_unit_id.is_some() {
            continue;
        }
        let entry = by_id
            .get(&r.price_book_entry_id)
            .copied()
            .cloned()
            .ok_or_else(|| RepoError::CorruptRow(format!("price {} has no entry", r.id)))?;
        // A price's `before` is its predecessor; a change's is the price it names.
        let before = match m.target_price_id {
            Some(target) => stored.get(&target),
            None => price::in_force_before(&prices, r).and_then(|b| stored.get(&b.id)),
        }
        .cloned()
        .map(|b| PricingPriceDto::of(b, &entry.model))
        .transpose()?;
        let price = PricingPriceDto::of(m.clone(), &entry.model)?;
        let policy = policies.get(&entry.id).cloned();
        let entry = PricingPriceBookEntryDto::from_stored(entry, policy)?;
        out.push(PricingProposedPrice {
            price,
            entry,
            chain: r.dim_value.clone().unwrap_or_else(|| "default".into()),
            before,
            pair_partner_id: r.paired_price_id,
            selected: true,
        });
    }
    Ok(out)
}

/// `GET /price-books/{id}/publish-changes`: the body, whose actors the door names (D-519).
/// # Errors
/// Returns a missing book or storage failure.
pub async fn publish_list(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    book: Uuid,
) -> Result<PricingPublishChanges, DoorError> {
    let model = book_repo::find(tx, scope, tenant, book)
        .await?
        .ok_or_else(support::missing)?;
    let prices = proposals(tx, tenant, book).await?;
    let entries: BTreeSet<Uuid> = prices.iter().map(|r| r.entry.id).collect();
    let plans = crate::infra::prices::plans_reading(tx, tenant, &entries, plans::today()).await?;
    let impact = crate::infra::prices::impact_of(prices.len(), entries.len(), &plans);
    Ok(PricingPublishChanges {
        book: PriceBookDto::from(model),
        prices,
        impact,
    })
}

/// `POST /price-books/{id}/publish-changes`: the ticked drafts (all when omitted), their pair
/// partners pulled in and recorded as `added_partner`, an optional common start and the
/// submitter's optional note (D-464, capped by the door).
/// # Errors
/// Returns the canonical refusal.
pub async fn publish(
    db: &Db,
    cmd: Command,
    book: Uuid,
    input: PricingPublishChangesRequest,
) -> Result<Response, CanonicalError> {
    support::retry_unit_capture(db, || async {
        if let Some(replay) = support::replay(
            db,
            cmd.tenant(),
            &format!("/bss-pricing/v1/price-books/{book}/publish-changes"),
            &cmd.key,
            &cmd.digest,
        )
        .await?
        {
            return Ok(replay);
        }

        let date = support::date(input.common_effective_date.clone(), "common_effective_date")?;
        let conn = db.conn().map_err(DoorError::from)?;
        book_repo::find(&conn, &cmd.scope, cmd.tenant(), book)
            .await
            .map_err(DoorError::from)?
            .ok_or_else(support::missing)?;
        let children = AccessScope::for_tenant(cmd.tenant());
        let entries = price_book_entry_repo::for_book(&conn, &children, cmd.tenant(), book)
            .await
            .map_err(DoorError::from)?;
        let prices = price_repo::for_entries(
            &conn,
            &children,
            cmd.tenant(),
            &entries.iter().map(|e| e.id).collect::<Vec<_>>(),
        )
        .await
        .map_err(DoorError::from)?;
        let selected =
            crate::infra::meter_semantics::publish_selection(prices, input.price_ids.as_deref());
        let entry_ids = selected.iter().map(|p| p.price_book_entry_id).collect();
        let observations = observe_entries(
            db,
            &cmd,
            entry_ids,
            Vec::new(),
            crate::infra::meter_semantics::Selection::Publish {
                book,
                ids: input.price_ids.clone(),
                rows: selected,
            },
        )
        .await?;
        let sink = cmd.outbox.clone();
        let cmd = cmd.clone();
        let input = input.clone();
        support::unit_transaction_observed_with_events(db, &sink, move |tx, outbox| {
            let observations = observations.clone();
            let (cmd, input) = (cmd.clone(), input.clone());
            Box::pin(async move {
                let endpoint = format!("/bss-pricing/v1/price-books/{book}/publish-changes");
                if let Some(replay) =
                    support::claim(tx, cmd.tenant(), &endpoint, &cmd.key, &cmd.digest).await?
                {
                    return Ok(replay);
                }
                observations.check_local(tx, cmd.tenant()).await?;
                book_repo::find(tx, &cmd.scope, cmd.tenant(), book)
                    .await?
                    .ok_or_else(support::missing)?;
                let children = AccessScope::for_tenant(cmd.tenant());
                // The book's prices in ONE statement, in the order its entries list them (PS-16).
                let entries =
                    price_book_entry_repo::for_book(tx, &children, cmd.tenant(), book).await?;
                let mut grouped = price_repo::by_entry(
                    price_repo::for_entries(
                        tx,
                        &children,
                        cmd.tenant(),
                        &entries.iter().map(|p| p.id).collect::<Vec<_>>(),
                    )
                    .await?,
                );
                let owned: Vec<entity::price::Model> = entries
                    .iter()
                    .flat_map(|p| grouped.remove(&p.id).unwrap_or_default())
                    .collect();
                let draft = |m: &entity::price::Model| {
                    m.state == PriceState::Draft.as_str() && m.pending_unit_id.is_none()
                };
                let selected: Vec<Uuid> = match input.price_ids {
                    None => owned.iter().filter(|m| draft(m)).map(|m| m.id).collect(),
                    Some(ids) => {
                        for id in &ids {
                            let Some(m) = owned.iter().find(|m| m.id == *id) else {
                                return Err(
                                    support::invalid("price_ids", "PRICE_NOT_IN_BOOK").into()
                                );
                            };
                            if !draft(m) {
                                return Err(support::conflict("PRICE_NOT_DRAFT").into());
                            }
                        }
                        ids
                    }
                };
                if selected.is_empty() {
                    return Err(support::invalid("price_ids", "NO_DRAFT_PRICES").into());
                }
                let chosen: BTreeSet<Uuid> = selected.iter().copied().collect();
                let mut added: Vec<Uuid> = owned
                    .iter()
                    .filter(|m| chosen.contains(&m.id))
                    .filter_map(|m| m.paired_price_id)
                    .filter(|partner| !chosen.contains(partner))
                    .collect();
                added.sort_unstable();
                added.dedup();
                let mut subject =
                    PricesSubject::new(cmd.ctx.clone(), cmd.hub.clone(), book, cmd.now());
                subject.meter_observations = observations;
                subject.common_effective_date = date;
                subject.added_partner = added;
                record_prices(
                    tx,
                    &outbox,
                    &cmd,
                    &endpoint,
                    (subject, input.note.clone()),
                    &selected,
                )
                .await
            })
        })
        .await
    })
    .await
    .map_err(Into::into)
}

/// A known unit state filter.
/// # Errors
/// Unknown states are refused with `UNIT_STATE_INVALID`.
pub fn state_filter(state: Option<&str>) -> Result<Option<UnitState>, CanonicalError> {
    state
        .map(|s| UnitState::parse(s).ok_or_else(|| support::invalid("state", "UNIT_STATE_INVALID")))
        .transpose()
}
/// What one read of the unit list asks for: its narrowing, its page (which carries its order, or
/// a cursor that carries its own) and whether it reads the live impact (D-458, D-470).
#[derive(Clone)]
pub struct UnitListRequest {
    pub filter: approval_repo::UnitListFilter,
    pub page: toolkit_odata::ODataQuery,
    /// `false` (`impact=false`): no plan is read and every unit answers `impact: null`.
    pub impact: bool,
    pub approve_scope: AccessScope,
    pub submit_scope: AccessScope,
}
/// `GET /approval-units`: one page in submission order (D-458), oldest or newest first (D-470),
/// each unit with every generation's decisions, whether the caller `ctx` may approve it (D-471)
/// and, unless the request declines it, the same live impact as the card. The tenant and the
/// reader both come from `ctx`, so they cannot be swapped (the phase 9 review's R7). The page, its
/// units' items, their decisions and the plans their impact names are read set-based: a fixed
/// number of statements whatever the page's size, and no plan read without the impact.
///
/// The page is a value. The HTTP door names its actors (D-519) and answers it; the inbox reads it
/// without parsing a response body back out of JSON.
/// # Errors
/// Returns a cursor the pager refuses (400) or storage failures.
pub async fn read_unit_page(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    ctx: &SecurityContext,
    request: &UnitListRequest,
) -> Result<PricingApprovalUnitList, DoorError> {
    let (tenant, reader) = (ctx.subject_tenant_id(), ctx.subject_id());
    let page = approval_repo::page_units(tx, scope, tenant, &request.filter, &request.page)
        .await
        .map_err(|e| match e {
            approval_repo::UnitListError::Query(e) => DoorError::Api(e.into()),
            approval_repo::UnitListError::Repo(e) => DoorError::Repo(e),
        })?;
    let ids: Vec<Uuid> = page.items.iter().map(|u| u.id).collect();
    // The items are read whatever the impact: whether the reader may approve judges their
    // authors (D-471). Without the impact, their authors alone are read (the phase 9 review's
    // R46).
    let (mut touched, mut authors) = if request.impact {
        let touched = approval_repo::items_of_units(tx, scope, tenant, &ids).await?;
        let authors = touched
            .iter()
            .map(|(id, items)| (*id, authors_of(items)))
            .collect();
        (touched, authors)
    } else {
        (
            BTreeMap::new(),
            approval_repo::item_authors_of_units(tx, scope, tenant, &ids).await?,
        )
    };
    let mut decisions = approval_repo::decisions_of_units(tx, scope, tenant, &ids).await?;
    let mut kinds = Vec::with_capacity(page.items.len());
    let mut entries = BTreeSet::new();
    for unit in &page.items {
        let kind = Kind::of(unit)?;
        if kind == Kind::Prices
            && let Some(items) = touched.get(&unit.id)
        {
            entries.extend(crate::infra::prices::entries_of_items(items));
        }
        kinds.push(kind);
    }
    let reading = if request.impact {
        Some(
            crate::infra::prices::PlansReading::load(
                tx,
                tenant,
                &entries,
                OffsetDateTime::now_utc().date(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut items = Vec::with_capacity(page.items.len());
    for (unit, kind) in page.items.into_iter().zip(kinds) {
        let id = unit.id;
        let touched = touched.remove(&id).unwrap_or_default();
        let authors = authors.remove(&id).unwrap_or_default();
        let decisions = decisions.remove(&id).unwrap_or_default();
        let mut dto = PricingApprovalUnitDto::of(
            unit,
            &authors,
            decisions,
            reader,
            &request.approve_scope,
            &request.submit_scope,
        )?;
        dto.impact = reading
            .as_ref()
            .map(|reading| kind.impact_from(reading, &touched));
        items.push(dto);
    }
    name_books(tx, tenant, &mut items).await?;
    Ok(PricingApprovalUnitList {
        items,
        page_info: page.page_info,
    })
}
/// `GET /approval-units/counts` (D-470): the units the list's narrowing keeps, by state and by
/// kind, in ONE grouped statement, which the door reads outside any transaction: one statement is
/// its own snapshot.
/// # Errors
/// Storage failures; a stored kind pricing does not record, or a state outside the unit's set, is
/// a corrupt row (500), as on every unit door.
pub async fn count_units(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    filter: &approval_repo::UnitListFilter,
) -> Result<Response, DoorError> {
    let mut by_state = PricingApprovalUnitStateCounts::default();
    let mut by_kind = PricingApprovalUnitKindCounts::default();
    let mut total = 0_u64;
    for row in approval_repo::count_units(tx, scope, tenant, filter).await? {
        *match row.state {
            UnitState::Pending => &mut by_state.pending,
            UnitState::Approved => &mut by_state.approved,
            UnitState::Rejected => &mut by_state.rejected,
            UnitState::Withdrawn => &mut by_state.withdrawn,
        } += row.units;
        *match row.kind {
            Kind::Prices => &mut by_kind.prices,
            Kind::PlanRevision => &mut by_kind.plan_revision,
        } += row.units;
        total += row.units;
    }
    Ok(support::response(
        StatusCode::OK,
        &PricingApprovalUnitCounts {
            by_state,
            by_kind,
            total,
        },
        None,
    )?)
}
/// `GET /approval-units/{id}`: the stored snapshot, the decisions, the live impact and whether
/// the caller `ctx` may approve it (D-471). The tenant and the reader both come from `ctx` (the
/// phase 9 review's R9). The door names its actors (D-519).
/// # Errors
/// Returns a missing unit or storage failure.
pub async fn get_unit(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    ctx: &SecurityContext,
    id: Uuid,
    approve_scope: &AccessScope,
    submit_scope: &AccessScope,
) -> Result<PricingApprovalUnitDto, DoorError> {
    let (tenant, reader) = (ctx.subject_tenant_id(), ctx.subject_id());
    let store = PricingApprovalStore {
        scope: scope.clone(),
        tenant_id: tenant,
    };
    let unit = load_unit(tx, &store, id).await?;
    let kind = Kind::of(&unit)?;
    let items = store.items(tx, id).await.map_err(approval_failure)?;
    let decisions = store.decisions(tx, id).await.map_err(approval_failure)?;
    let mut dto = PricingApprovalUnitDto::of(
        unit,
        &authors_of(&items),
        decisions,
        reader,
        approve_scope,
        submit_scope,
    )?;
    dto.impact = Some(kind.impact(tx, tenant, &items).await?);
    name_books(tx, tenant, std::slice::from_mut(&mut dto)).await?;
    Ok(dto)
}

/// The subject a pending unit is judged by, chosen by its stored kind: for `prices`, its book,
/// shift and pulled-in partners.
/// # Errors
/// An unknown stored kind is a corrupt row (500), never judged as `prices`.
fn subject_of(
    cmd: &Command,
    unit: &Unit,
    action: Vote,
    now: OffsetDateTime,
) -> Result<Subject, DoorError> {
    match Kind::of(unit)? {
        Kind::Prices => {
            let mut subject =
                PricesSubject::new(cmd.ctx.clone(), cmd.hub.clone(), unit.ref_id, now);
            subject.common_effective_date = unit.common_effective_date;
            // Every `prices` unit records the partners publish-changes pulled in (an empty list
            // when none); one that does not read is a corrupt row, never "no partner" (PS-06).
            subject.added_partner = serde_json::from_value(unit.snapshot["added_partner"].clone())
                .map_err(|e| {
                    RepoError::CorruptRow(format!("unit {} added_partner: {e}", unit.id))
                })?;
            subject.release = if action == Vote::Reject {
                Release::Rejected
            } else {
                Release::Draft
            };
            Ok(Subject::Prices(subject))
        }
        Kind::PlanRevision => Ok(Subject::PlanRevision(PlanRevisionSubject::new(
            cmd.ctx.clone(),
            cmd.hub.clone(),
            unit.ref_id,
            now,
        ))),
    }
}
/// `POST /approval-units/{id}/approve|reject|withdraw`.
/// Content drift commits the refreshed unit and answers `UNIT_STALE` with the new generation.
/// # Errors
/// Returns the canonical refusal; a generation mismatch carries the current generation.
pub async fn vote(
    db: &Db,
    cmd: Command,
    id: Uuid,
    action: Vote,
    body: Option<PricingVoteRequest>,
) -> Result<Response, CanonicalError> {
    let result = support::retry_unit_capture(db, || async {
        if let Some(replay) = support::replay(
            db,
            cmd.tenant(),
            &format!("/bss-pricing/v1/approval-units/{id}/{}", action.path()),
            &cmd.key,
            &cmd.digest,
        )
        .await?
        {
            return Ok(replay);
        }

        let observations = if action == Vote::Withdraw {
            crate::infra::meter_semantics::Observations::default()
        } else {
            let conn = db.conn().map_err(DoorError::from)?;
            let store = PricingApprovalStore {
                scope: cmd.scope.clone(),
                tenant_id: cmd.tenant(),
            };
            let unit = crate::infra::storage::repo::approval_repo::find_unit(
                &conn,
                &store.scope,
                store.tenant_id,
                id,
            )
            .await
            .map_err(approval_failure)?
            .ok_or_else(|| support::missing_what("approval_unit"))?;
            if unit.state == UnitState::Pending {
                match Kind::of(&unit).map_err(DoorError::from)? {
                    Kind::PlanRevision => observe_revision(db, &cmd, unit.ref_id).await?,
                    Kind::Prices => {
                        let items = crate::infra::storage::repo::approval_repo::items_of_units(
                            &conn,
                            &store.scope,
                            store.tenant_id,
                            &[id],
                        )
                        .await
                        .map_err(DoorError::from)?
                        .remove(&id)
                        .unwrap_or_default();
                        observe_prices(
                            db,
                            &cmd,
                            &items.iter().map(|i| i.item_id).collect::<Vec<_>>(),
                        )
                        .await?
                    }
                }
            } else {
                crate::infra::meter_semantics::Observations::default()
            }
        };
        let sink = cmd.outbox.clone();
        let cmd = cmd.clone();
        let body = body.clone();
        support::unit_transaction_observed_with_events(db, &sink, move |tx, outbox| {
            let observations = observations.clone();
            let (cmd, body) = (cmd.clone(), body.clone());
            Box::pin(
                async move { vote_in(tx, &outbox, &cmd, id, action, body, observations).await },
            )
        })
        .await
    })
    .await;
    match result {
        Err(DoorError::Generation { current }) => {
            Ok(support::generation_problem("GENERATION_MISMATCH", current).into_response())
        }
        other => other.map_err(Into::into),
    }
}
async fn vote_in(
    tx: &DbTx<'_>,
    outbox: &TxOutbox,
    cmd: &Command,
    id: Uuid,
    action: Vote,
    body: Option<PricingVoteRequest>,
    observations: crate::infra::meter_semantics::Observations,
) -> Result<Response, DoorError> {
    let endpoint = format!("/bss-pricing/v1/approval-units/{id}/{}", action.path());
    let store = PricingApprovalStore {
        scope: cmd.scope.clone(),
        tenant_id: cmd.tenant(),
    };
    let unit = load_unit(tx, &store, id).await?;
    if let Some(replay) = support::claim(tx, cmd.tenant(), &endpoint, &cmd.key, &cmd.digest).await?
    {
        return Ok(replay);
    }
    if unit.state != UnitState::Pending {
        return Err(support::conflict("UNIT_ALREADY_DECIDED").into());
    }
    observations.check_local(tx, cmd.tenant()).await?;
    let now = cmd.now();
    let mut subject = subject_of(cmd, &unit, action, now)?;
    match &mut subject {
        Subject::Prices(s) => s.meter_observations = observations,
        Subject::PlanRevision(s) => s.meter_observations = observations,
    }
    let actor = cmd.ctx.subject_id();
    let seen = || {
        body.as_ref()
            .map(|b| b.generation)
            .ok_or_else(|| DoorError::from(support::invalid("generation", "GENERATION_REQUIRED")))
    };
    let note = body.as_ref().and_then(|b| b.note.clone());
    let outcome = match action {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-1
        Vote::Approve => Engine::approve(
            &store,
            &subject,
            tx,
            id,
            actor,
            seen()?,
            note.as_deref(),
            now,
        )
        .await
        .map_err(refusal(&subject))?,
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-1
        Vote::Reject => {
            let note = note
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| support::invalid("note", "NOTE_REQUIRED"))?;
            // A stale unit is refreshed as on approve; a Products refusal of the re-read keeps
            // its own status and code (D-402, DESIGN §3.3).
            match Engine::reject(&store, &subject, tx, id, actor, seen()?, note, now)
                .await
                .map_err(refusal(&subject))?
            {
                RejectOutcome::Refreshed { generation } => ApproveOutcome::Refreshed { generation },
                RejectOutcome::Rejected => ApproveOutcome::Applied,
            }
        }
        Vote::Withdraw => {
            Engine::withdraw(&store, &subject, tx, id, actor, now)
                .await
                .map_err(approval_failure)?;
            ApproveOutcome::Applied
        }
    };
    let (label, audit, have, need) = match outcome {
        ApproveOutcome::Refreshed { generation } => {
            support::audit(
                tx,
                &cmd.ctx,
                cmd.correlation,
                "approval.refreshed",
                id,
                unit.version,
            )
            .await?;
            let problem = support::generation_problem("UNIT_STALE", generation);
            return support::answer(
                tx,
                cmd.tenant(),
                &endpoint,
                &cmd.key,
                StatusCode::BAD_REQUEST,
                &problem,
                None,
            )
            .await;
        }
        ApproveOutcome::Pending { have, need } => (
            PricingVoteOutcome::Pending,
            "approval.vote",
            Some(have),
            Some(need),
        ),
        ApproveOutcome::Applied => {
            if action == Vote::Approve {
                published(tx, outbox, cmd, &store, &subject, id, now).await?;
            }
            decided(tx, outbox, cmd, &store, id, now).await?;
            match action {
                Vote::Approve => (PricingVoteOutcome::Applied, "approval.approved", None, None),
                Vote::Reject => (
                    PricingVoteOutcome::Rejected,
                    "approval.rejected",
                    None,
                    None,
                ),
                Vote::Withdraw => (
                    PricingVoteOutcome::Withdrawn,
                    "approval.withdrawn",
                    None,
                    None,
                ),
            }
        }
    };
    let unit = load_unit(tx, &store, id).await?;
    support::audit(tx, &cmd.ctx, cmd.correlation, audit, id, unit.version).await?;
    let receipt = PricingVoteReceipt {
        have,
        need,
        outcome: label,
        unit: unit_dto(
            tx,
            &store,
            unit,
            actor,
            &cmd.approve_scope,
            &cmd.submit_scope,
        )
        .await?,
    };
    support::answer(
        tx,
        cmd.tenant(),
        &endpoint,
        &cmd.key,
        StatusCode::OK,
        &receipt,
        None,
    )
    .await
}

/// A strong decimal validator over the whole policy, like the dimension registry's.
fn policy_tag(policy: &bss_approval::Policy) -> Result<u64, CanonicalError> {
    let hash = crate::api::rest::preconditions::request_digest(&(
        policy.default_quorum,
        &policy.overrides,
    ))
    .map_err(CanonicalError::from)?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash[..8]);
    Ok(u64::from_be_bytes(bytes))
}
/// `GET /approval-policy`: the tenant default (fail-safe one) and kind overrides.
/// # Errors
/// Returns storage failures.
pub async fn get_policy(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<Response, DoorError> {
    let policy = approval_repo::read_policy(tx, scope, tenant).await?;
    let tag = policy_tag(&policy)?;
    Ok(support::response(
        StatusCode::OK,
        &PricingApprovalPolicyDto::from(policy),
        Some(tag),
    )?)
}
/// `GET /approval-policy/{kind}/effective` (D-481): the quorum a submit of `kind` needs, the
/// kind's override or the tenant default. One statement. The door has already judged the
/// caller's grant. The policy's resource column is `kind` (text), so this read is the
/// tenant's, as `stored_contexts` is: a resource constraint must not compare `kind` to a uuid.
/// # Errors
/// Storage failures.
pub async fn effective_quorum(
    tx: &impl DBRunner,
    tenant: Uuid,
    kind: Kind,
) -> Result<Response, DoorError> {
    let scope = AccessScope::for_tenant(tenant);
    let policy = approval_repo::read_policy(tx, &scope, tenant).await?;
    Ok(support::response(
        StatusCode::OK,
        &PricingEffectivePolicyDto {
            kind: kind.into(),
            quorum_required: policy.quorum_for(kind.as_str()),
        },
        None,
    )?)
}
/// `DELETE /approval-policy/{kind}` (D-435): remove one kind's override at the policy the caller
/// read, so the kind follows the default again; answers the policy and its new tag. The default
/// (`*`) is never deleted: a tenant always has a quorum to fall back to.
/// # Errors
/// 400 `POLICY_DEFAULT_REQUIRED` or `POLICY_KIND_INVALID`; 409 `STALE_REVISION`; 404 when the
/// kind has no override.
pub async fn reset_policy(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    version: u64,
    kind: &str,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    if kind == "*" {
        return Err(support::invalid("kind", "POLICY_DEFAULT_REQUIRED").into());
    }
    if Kind::parse(kind).is_none() {
        return Err(support::invalid("kind", "POLICY_KIND_INVALID").into());
    }
    let policy = approval_repo::read_policy(tx, scope, tenant).await?;
    if policy_tag(&policy)? != version {
        return Err(support::conflict("STALE_REVISION").into());
    }
    if !policy.overrides.contains_key(kind)
        || approval_repo::delete_policy(tx, scope, tenant, kind).await? == 0
    {
        return Err(support::missing_what("approval_policy_override").into());
    }
    support::audit(tx, ctx, correlation, "approval_policy.reset", tenant, 0).await?;
    let policy = approval_repo::read_policy(tx, scope, tenant).await?;
    let tag = policy_tag(&policy)?;
    Ok(support::response(
        StatusCode::OK,
        &PricingApprovalPolicyDto::from(policy),
        Some(tag),
    )?)
}
/// `PUT /approval-policy`: set the default (`*`) or one kind's quorum (`prices`,
/// `plan_revision`) under If-Match.
/// # Errors
/// Returns `POLICY_KIND_INVALID`, `QUORUM_INVALID` or `STALE_REVISION`.
pub async fn put_policy(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    version: u64,
    input: PricingApprovalPolicyPut,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let kind = input.kind.unwrap_or_else(|| "*".into());
    if kind != "*" && Kind::parse(&kind).is_none() {
        return Err(support::invalid("kind", "POLICY_KIND_INVALID").into());
    }
    if i32::try_from(input.quorum).is_err() {
        return Err(support::invalid("quorum", "QUORUM_INVALID").into());
    }
    if policy_tag(&approval_repo::read_policy(tx, scope, tenant).await?)? != version {
        return Err(support::conflict("STALE_REVISION").into());
    }
    approval_repo::write_policy(tx, scope, tenant, &kind, input.quorum).await?;
    support::audit(tx, ctx, correlation, "approval_policy.write", tenant, 0).await?;
    let policy = approval_repo::read_policy(tx, scope, tenant).await?;
    let tag = policy_tag(&policy)?;
    Ok(support::response(
        StatusCode::OK,
        &PricingApprovalPolicyDto::from(policy),
        Some(tag),
    )?)
}

/// Detach the local entry selection before resolving E1. The subject rechecks it in its
/// existing transaction. This helper is invoked only after the door's authorization.
async fn observe_entries(
    db: &Db,
    cmd: &Command,
    ids: Vec<Uuid>,
    extra_skus: Vec<Uuid>,
    selection: crate::infra::meter_semantics::Selection,
) -> Result<crate::infra::meter_semantics::Observations, CanonicalError> {
    let conn = db.conn().map_err(DoorError::from)?;
    let entries = price_book_entry_repo::find_many(
        &conn,
        &AccessScope::for_tenant(cmd.tenant()),
        cmd.tenant(),
        &ids,
    )
    .await
    .map_err(DoorError::from)?;
    Ok(crate::infra::meter_semantics::Observations::capture(
        &conn, &cmd.hub, &cmd.ctx, entries, extra_skus, selection,
    )
    .await?)
}
async fn observe_revision(
    db: &Db,
    cmd: &Command,
    id: Uuid,
) -> Result<crate::infra::meter_semantics::Observations, CanonicalError> {
    let conn = db.conn().map_err(DoorError::from)?;
    let revision = plans::find_revision(&conn, &cmd.scope, cmd.tenant(), id).await?;
    if revision.state != "draft" && revision.state != "pending" {
        return Ok(crate::infra::meter_semantics::Observations::default());
    }
    let items = plan_item_repo::for_revision(
        &conn,
        &AccessScope::for_tenant(cmd.tenant()),
        cmd.tenant(),
        id,
    )
    .await
    .map_err(DoorError::from)?;
    let entry_ids = items.iter().filter_map(|i| i.price_book_entry_id).collect();
    let sku_ids = items.iter().map(|i| i.sku_id).collect();
    observe_entries(
        db,
        cmd,
        entry_ids,
        sku_ids,
        crate::infra::meter_semantics::Selection::Revision { id, items },
    )
    .await
}
async fn observe_prices(
    db: &Db,
    cmd: &Command,
    ids: &[Uuid],
) -> Result<crate::infra::meter_semantics::Observations, CanonicalError> {
    let conn = db.conn().map_err(DoorError::from)?;
    let prices = price_repo::find_many(&conn, &cmd.scope, cmd.tenant(), ids)
        .await
        .map_err(DoorError::from)?;
    let entry_ids = prices
        .iter()
        .filter(|p| p.state == "draft" || p.state == "pending")
        .map(|p| p.price_book_entry_id)
        .collect();
    observe_entries(
        db,
        cmd,
        entry_ids,
        Vec::new(),
        crate::infra::meter_semantics::Selection::Prices {
            ids: ids.to_vec(),
            scope: cmd.scope.clone(),
            rows: prices,
        },
    )
    .await
}
#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;

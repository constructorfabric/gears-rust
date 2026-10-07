//! Shared durable executor: local transitions are conditional, remote calls are idempotent.
//!
//! One machine drives both kinds of reference (D-407): a price book entry and a plan item. It
//! dispatches by [`RefKind`] at every hook that touches the reference itself — the work input and
//! its endpoint, the write (Tx B), the confirm (Tx C), the receipt, the refusals of the write,
//! the SKU re-read's lifecycle rules, and the loss and its event. The entry's hooks live here;
//! the plan item's in [`plan_item`].
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-reference-protocol:p1
pub mod plan_item;
use crate::{
    api::rest::authoring::{
        AuthoringState,
        dto::{PricingPlanItemCreate, PricingPriceBookEntryCreate},
        support::{self, DoorError},
    },
    domain::{
        price_book_entry::{
            Model, OpState, ReferenceState, charge_kind_for, default_model, model_allowed,
        },
        reference_op::{self, Effect, Event, Op, OpKind, RefKind},
    },
    infra::storage::{
        RepoError,
        entity::{plan_item as plan_item_entity, price_book_entry, reference_op as entity},
        repo::{
            idempotency_repo as idem, plan_revision_repo, price_book_entry_repo,
            reference_op_repo as ops,
        },
    },
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bss_products_sdk::{
    PRICING_SYSTEM_ACTOR, ReferenceRegistryV1,
    models::{Lifecycle, ReferenceKind},
};
pub use plan_item::attach_op;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicUsize, Ordering};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// Everything needed after losing the original door future, encoded in the op outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Work {
    pub target: Target,
    pub correlation: Uuid,
    pub refusal: Option<Receipt>,
    pub receipt: Option<Receipt>,
    /// [`CANCELLED`] once a create was given up before its reservation outcome was known:
    /// no entry was written, the Idempotency-Key claim was released in that same
    /// transaction, and the cancellation releases whatever reservation exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Why the op releases its reference, when the op says: [`BOOK_ARCHIVED_REASON`] for a
    /// `release` (D-522). Absent from every other op, and from every op stored before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
/// The reason a `release` op carries: its entry's book was archived (D-522).
pub const BOOK_ARCHIVED_REASON: &str = "book_archived";
/// An entry's persisted create input (D-401): what its create writes, rebuilt from the entry by
/// every later op of it (a rereserve, a delete). Its JSON is the door's request body field for
/// field. `model` (D-427) is absent from an op stored before `m20260926_000013`: such a create
/// takes the model of the entry that already holds its key (book, SKU, charge kind, period), so it
/// meets `ENTRY_KEY_TAKEN`; else the model that migration gives an entry without prices, its charge
/// kind's default (`default_model`). A rereserve or a delete never writes the model, so its entry
/// keeps its own.
/// The policy an entry op keeps when it does not carry the declaration itself.
///
/// Stored rows written as a JSON array `[policy_id, version, digest]` still read. A new row
/// writes the named fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsagePolicyReference {
    pub policy_id: Uuid,
    pub version: i64,
    pub digest: String,
}
impl<'de> Deserialize<'de> for UsagePolicyReference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Named {
                policy_id: Uuid,
                version: i64,
                digest: String,
            },
            Legacy(Uuid, i64, String),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Named {
                policy_id,
                version,
                digest,
            }
            | Wire::Legacy(policy_id, version, digest) => Ok(Self {
                policy_id,
                version,
                digest,
            }),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryInput {
    /// D-503: exact declaration captured as the authorized caller before Tx A.
    #[serde(default)]
    pub meter_evidence: Option<Box<crate::infra::usage_policy_wire::MeterEvidence>>,
    /// Absent only for operations persisted before D-502; public requests cannot set it.
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub usage_rating_policy: Option<Box<crate::infra::usage_policy_wire::UsageRatingPolicyInput>>,
    /// Later operations preserve the existing reference, including an absent legacy policy.
    /// A row stored as a positional array still reads; a new row writes the named fields.
    #[serde(default)]
    pub usage_policy_reference: Option<UsagePolicyReference>,
    pub sku_id: Uuid,
    pub period: Option<String>,
    pub dimension_key: Option<String>,
    pub invoice_line_override: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}
impl From<PricingPriceBookEntryCreate> for EntryInput {
    fn from(input: PricingPriceBookEntryCreate) -> Self {
        Self {
            meter_evidence: None,
            schema_version: Some(3),
            usage_rating_policy: input.usage_rating_policy.map(|policy| {
                Box::new(crate::infra::usage_policy_wire::UsageRatingPolicyInput::from(policy))
            }),
            usage_policy_reference: None,
            sku_id: input.sku_id,
            period: input.period,
            dimension_key: input.dimension_key,
            invoice_line_override: input.invoice_line_override,
            model: Some(input.model),
        }
    }
}
impl EntryInput {
    /// The input of an entry that exists, for the ops that follow its create.
    #[must_use]
    pub fn of(entry: &price_book_entry::Model) -> Self {
        Self {
            meter_evidence: None,
            schema_version: Some(1),
            usage_rating_policy: None,
            usage_policy_reference: entry
                .usage_policy_id
                .zip(entry.usage_policy_version)
                .zip(entry.usage_policy_digest.clone())
                .map(|((policy_id, version), digest)| UsagePolicyReference {
                    policy_id,
                    version,
                    digest,
                }),
            sku_id: entry.sku_id,
            period: entry.period.clone(),
            dimension_key: entry.dimension_key.clone(),
            invoice_line_override: entry.invoice_line_override.clone(),
            model: Some(entry.model.clone()),
        }
    }
}
/// A plan item's persisted create input (whole-branch review PS-19): what its create writes,
/// kept apart from the request `PricingPlanItemCreate`, whose `deny_unknown_fields` would make a
/// stored op corrupt the day a field of the wire is renamed or removed. Its JSON is the request
/// body's, field for field; it denies no unknown field, and a field added later takes
/// `#[serde(default)]`. An op stored before D-467 also carries `treatment`, `included_qty` and
/// `qty_min`, which are ignored: its item is written as every item is from D-467 on
/// (`plan::stored_treatment`, no quantity), and one without an entry stays a legacy entry-less row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemInput {
    pub sku_id: Uuid,
    pub price_book_entry_id: Option<Uuid>,
}
impl From<PricingPlanItemCreate> for ItemInput {
    fn from(input: PricingPlanItemCreate) -> Self {
        Self {
            sku_id: input.sku_id,
            price_book_entry_id: input.price_book_entry_id,
        }
    }
}
/// The work input of each kind: what its create writes, and where its Idempotency-Key lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// An entry of `book_id`; every op of an entry carries its create input.
    PriceBookEntry { book_id: Uuid, input: EntryInput },
    /// An item of `revision_id`. Only a create carries its input; an attach, a rereserve and a
    /// delete find the item by the op's `ref_id`.
    PlanItem {
        revision_id: Uuid,
        input: Option<ItemInput>,
    },
}
/// The reference an op works for: its kind, its id and the SKU it reserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ref {
    pub kind: RefKind,
    pub id: Uuid,
    pub sku_id: Uuid,
}
/// Products spells the reference kinds as pricing does.
#[must_use]
pub const fn products_kind(kind: RefKind) -> ReferenceKind {
    match kind {
        RefKind::Entry => ReferenceKind::PriceBookEntry,
        RefKind::PlanItem => ReferenceKind::PlanItem,
    }
}
/// The recorded outcome of a create cancelled before its reservation outcome was known.
pub const CANCELLED: &str = "cancelled";
/// Who drives an op. A door drives the work it just began, under the requesting principal;
/// the ticker resumes abandoned work under the pricing system actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Door,
    Ticker,
}
/// Store the exact body rendering, including problem bodies, inside the JSON replay store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub status: u16,
    pub body: String,
    pub etag: Option<String>,
}
impl Receipt {
    /// Render a stored response without serializing its body a second time.
    /// # Errors
    /// Rejects corrupt stored status or header values.
    pub fn response(&self) -> Result<Response, CanonicalError> {
        let status = StatusCode::from_u16(self.status).map_err(|_| corrupt())?;
        let mut response = (status, self.body.clone()).into_response();
        response.headers_mut().insert(
            "content-type",
            if status.is_client_error() || status.is_server_error() {
                "application/problem+json"
            } else {
                "application/json"
            }
            .parse()
            .map_err(|_| corrupt())?,
        );
        if let Some(tag) = &self.etag {
            response
                .headers_mut()
                .insert("etag", tag.parse().map_err(|_| corrupt())?);
        }
        Ok(response)
    }
    pub async fn error(error: CanonicalError) -> Result<Self, CanonicalError> {
        let response = error.into_response();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .map_err(|_| corrupt())?;
        Ok(Self {
            status,
            body: String::from_utf8(bytes.to_vec()).map_err(|_| corrupt())?,
            etag: None,
        })
    }
    pub async fn entry(
        tx: &impl DBRunner,
        model: price_book_entry::Model,
    ) -> Result<Self, CanonicalError> {
        let etag = Some(format!("\"{}\"", model.version));
        let body = match crate::api::rest::authoring::dto::price_book_entry_json(tx, model).await {
            Ok(body) => body,
            Err(crate::api::rest::authoring::dto::EntryJsonError::Storage(error)) => {
                return Err(stored_failure(error));
            }
            Err(crate::api::rest::authoring::dto::EntryJsonError::Serialize) => {
                return Err(corrupt());
            }
        };
        Ok(Self {
            status: 201,
            body,
            etag,
        })
    }
    /// A created plan item's answer: 201, its body and its version.
    pub fn item(model: plan_item_entity::Model) -> Result<Self, CanonicalError> {
        let etag = Some(format!("\"{}\"", model.version));
        Ok(Self {
            status: 201,
            body: serde_json::to_string(
                &crate::api::rest::authoring::dto::PricingPlanItemDto::try_from(model)
                    .map_err(stored_failure)?,
            )
            .map_err(|_| corrupt())?,
            etag,
        })
    }
}
fn corrupt() -> CanonicalError {
    CanonicalError::internal("invalid durable pricing reference work").create()
}
/// An observation the op's state does not admit: the op, its state and the event, kept in the
/// diagnostic apart from every other corrupt record (PS-04).
fn illegal(op: &entity::Model, error: &reference_op::IllegalTransition) -> CanonicalError {
    CanonicalError::internal(format!("pricing reference op {}: {error}", op.op_id)).create()
}
/// A stored token outside its closed set in the answer being written (D-439): a storage failure.
fn stored_failure(error: crate::infra::storage::RepoError) -> CanonicalError {
    crate::api::rest::authoring::support::DoorError::from(error).into()
}
/// A connection the pool refused (asked for inside a transaction): a storage failure logged with
/// its cause, never a corrupt op record (the phase 9 review's R21).
fn conn_failure(error: toolkit_db::DbError) -> CanonicalError {
    stored_failure(error.into())
}
impl Work {
    /// Decode persisted recovery input.
    /// # Errors
    /// A missing or malformed record is surfaced instead of dropped.
    pub fn read(op: &entity::Model) -> Result<Self, CanonicalError> {
        serde_json::from_str(op.outcome.as_deref().ok_or_else(corrupt)?).map_err(|_| corrupt())
    }
    pub fn encode(&self) -> Result<String, CanonicalError> {
        serde_json::to_string(self).map_err(|_| corrupt())
    }
    /// Where the create's Idempotency-Key lives: the door that began it.
    #[must_use]
    pub fn endpoint(&self) -> String {
        match &self.target {
            Target::PriceBookEntry { book_id, .. } => {
                format!("/bss-pricing/v1/price-books/{book_id}/entries")
            }
            Target::PlanItem { revision_id, .. } => {
                format!("/bss-pricing/v1/plan-revisions/{revision_id}/items")
            }
        }
    }
    /// Whether this create was cancelled before its reservation outcome was known.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.outcome.as_deref() == Some(CANCELLED)
    }
}
/// Start a durable record inside Tx A or the removal's transaction.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn new_op(
    ctx: &SecurityContext,
    reference: Ref,
    work: &Work,
    kind: OpKind,
    reservation_id: Option<Uuid>,
    key: Option<String>,
    now: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    Ok(entity::Model {
        op_id: Uuid::now_v7(),
        tenant_id: ctx.subject_tenant_id(),
        kind: kind.as_str().into(),
        ref_kind: reference.kind.as_str().into(),
        ref_id: reference.id,
        sku_id: reference.sku_id,
        reservation_id,
        idempotency_key: key,
        state: if matches!(kind, OpKind::Delete | OpKind::Release) {
            OpState::Releasing
        } else {
            OpState::Reserving
        }
        .as_str()
        .into(),
        outcome: Some(work.encode()?),
        attempts: 0,
        next_attempt_at: now + IN_FLIGHT_GRACE,
        last_error: None,
        created_by: ctx.subject_id(),
        created_at: now,
        updated_at: now,
    })
}
/// How long a door that just created or advanced an op keeps it before the ticker may take
/// it over. The door drives its op to completion without waiting on `next_attempt_at`; the
/// grace only keeps the one-second ticker from racing a live request with a second registry
/// caller under another actor. An abandoned op (a crash, a dropped request) is due after it.
pub const IN_FLIGHT_GRACE: time::Duration = time::Duration::seconds(30);
pub use crate::infra::clock::{Clock, WallClock};
/// Backoff remains bounded even after years of failures.
#[must_use]
pub fn backoff(attempts: i32) -> time::Duration {
    time::Duration::seconds((1_i64 << attempts.clamp(0, 9)).min(300))
}
fn parse_state(op: &entity::Model) -> Result<OpState, CanonicalError> {
    op.state.parse().map_err(|_| corrupt())
}
fn parse_ref_kind(op: &entity::Model) -> Result<RefKind, CanonicalError> {
    op.ref_kind.parse().map_err(|_| corrupt())
}
/// Whether the op re-reserves a reference that already exists: a rereserve, or the attach of a
/// copied item (D-413). Ending without a live receipt makes that reference lost.
fn replaces_a_receipt(op: &entity::Model) -> bool {
    op.kind == OpKind::Rereserve.as_str() || op.kind == OpKind::Attach.as_str()
}
/// Whether a definite refusal that is not a losing one is retried rather than ending the op
/// (D-401, D-413). A rereserve always is (D-401). An attach is retried only while the refusal may
/// be its caller's own (a door caller's grant): Products authorizes the ticker's system actor to
/// the tenant, so a refusal given to it is about the SKU (Products no longer knows it, say) and
/// would never change — the item is lost instead of an attach retried forever.
fn retries_a_refusal(op: &entity::Model, ctx: &SecurityContext) -> bool {
    op.kind == OpKind::Rereserve.as_str()
        || (op.kind == OpKind::Attach.as_str() && ctx.subject_id() != PRICING_SYSTEM_ACTOR)
}
/// Apply exactly the observation used to perform the external call. A stale observer retries.
async fn advance(
    tx: &impl DBRunner,
    observed: &entity::Model,
    work: &Work,
    event: Event,
    clock: &dyn Clock,
) -> Result<(OpState, Vec<Effect>), DoorError> {
    let scope = AccessScope::for_tenant(observed.tenant_id);
    let current = ops::find(tx, &scope, observed.tenant_id, observed.op_id)
        .await?
        .ok_or_else(corrupt)?;
    if current != *observed {
        return Err(RepoError::Conflict {
            code: ops::REFERENCE_OP_CONTENDED,
        }
        .into());
    }
    let from = parse_state(observed)?;
    let (next, effects) = reference_op::next(
        Op {
            state: from,
            reservation_id: observed.reservation_id,
            refusal: observed.last_error.clone(),
        },
        event,
    )
    .map_err(|e| illegal(observed, &e))?;
    let retry = effects.contains(&Effect::Retry);
    let attempts = if retry {
        observed.attempts.saturating_add(1)
    } else {
        observed.attempts
    };
    let now = clock.now();
    ops::transition(
        tx,
        &scope,
        observed.op_id,
        from,
        next.state,
        &ops::TransitionFields {
            reservation_id: next.reservation_id,
            outcome: Some(work.encode()?),
            attempts,
            next_attempt_at: if retry {
                now + (backoff(attempts) + time::Duration::milliseconds(clock.jitter_millis()))
                    .min(time::Duration::seconds(300))
            } else {
                now + IN_FLIGHT_GRACE
            },
            // A retry names the registry's unavailability, unless the op already recorded the
            // refusal it met (a cancelling create's): `next` reads that refusal back from here.
            last_error: if retry {
                next.refusal.or_else(|| Some("REGISTRY_UNAVAILABLE".into()))
            } else {
                next.refusal
            },
            updated_at: now,
        },
    )
    .await?;
    Ok((next.state, effects))
}
/// A re-reservation or an attach that ended without a live receipt leaves its reference lost:
/// the state, the kind's durable event and the audit record commit together.
async fn mark_lost(
    tx: &(impl DBRunner + Sync),
    outbox: &super::events::TxOutbox,
    ctx: &SecurityContext,
    work: &Work,
    op: &entity::Model,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    match parse_ref_kind(op)? {
        RefKind::Entry => mark_entry_lost(tx, outbox, ctx, work, op, now).await,
        RefKind::PlanItem => plan_item::mark_lost(tx, outbox, ctx, work, op, now).await,
    }
}
/// A re-reservation that ended without a live receipt leaves its entry lost: the
/// state, the durable `PriceBookEntryReferenceLost` event and the audit record commit together.
/// A `released` entry (D-522: its book was archived) stays `released`: its reference was let go on
/// purpose, not lost, and an unarchive lists it as not re-reserved.
async fn mark_entry_lost(
    tx: &(impl DBRunner + Sync),
    outbox: &super::events::TxOutbox,
    ctx: &SecurityContext,
    work: &Work,
    op: &entity::Model,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let scope = AccessScope::for_tenant(op.tenant_id);
    let Some(mut entry) = price_book_entry_repo::find(tx, &scope, op.tenant_id, op.ref_id).await?
    else {
        return Ok(());
    };
    if entry.reference_state == ReferenceState::Lost.as_str()
        || entry.reference_state == ReferenceState::Released.as_str()
    {
        // A lost entry whose re-reservation is refused again stays lost, announced once; a
        // released one stays released (D-522).
        return Ok(());
    }
    price_book_entry_repo::set_reference(
        tx,
        &scope,
        op.tenant_id,
        op.ref_id,
        entry.version,
        ReferenceState::Lost,
        entry.reservation_id,
        now,
    )
    .await?;
    super::reference_events::lost(outbox, tx, &entry, ctx.subject_id(), now).await?;
    entry.reference_state = ReferenceState::Lost.as_str().into();
    entry.version += 1;
    support::audit(
        tx,
        ctx,
        work.correlation,
        "PriceBookEntryReferenceLost",
        entry.id,
        entry.version,
    )
    .await?;
    Ok(())
}
/// Answer the op's Idempotency-Key with its receipt or refusal, once, in the completing transaction.
async fn answer_key(
    tx: &(impl DBRunner + Sync),
    op: &entity::Model,
    work: &Work,
) -> Result<(), DoorError> {
    let Some(key) = &op.idempotency_key else {
        return Ok(());
    };
    if work.cancelled() {
        // The claim was released when the create was cancelled; the key may now belong to a
        // fresh attempt, which this op must never answer.
        return Ok(());
    }
    let scope = AccessScope::for_tenant(op.tenant_id);
    let receipt = work
        .receipt
        .as_ref()
        .or(work.refusal.as_ref())
        .ok_or_else(corrupt)?;
    if idem::answer_idempotency_key(
        tx,
        &scope,
        op.tenant_id,
        &work.endpoint(),
        key,
        i32::from(receipt.status),
        support::value(receipt)?,
        // A durable op may answer long after its claim: the answer is kept a full retention from
        // now, or the next same-key retry would find it expired and take the key over (D-429).
        Some(crate::infra::storage::stored_now() + time::Duration::hours(24)),
    )
    .await?
        != idem::IdempotencyAnswer::Recorded
    {
        return Err(corrupt().into());
    }
    Ok(())
}
/// Give up a create before its write (spec §13: a 503 writes nothing). In
/// one transaction the op moves `reserving → cancelling`, is recorded [`CANCELLED`], and its
/// Idempotency-Key claim is released, so a same-key retry runs afresh with a new entry id.
/// The cancellation then releases whatever reservation the unanswered call made.
async fn abandon(
    state: &AuthoringState,
    op: &entity::Model,
    mut work: Work,
    clock: Arc<dyn Clock>,
) -> Result<(), DoorError> {
    work.outcome = Some(CANCELLED.into());
    let op = op.clone();
    support::transaction_door(&state.db.db(), move |tx| {
        let (op, work, clock) = (op.clone(), work.clone(), clock.clone());
        Box::pin(async move {
            advance(tx, &op, &work, Event::ReservationUnknown, clock.as_ref()).await?;
            if let Some(key) = &op.idempotency_key {
                idem::release_idempotency_claim(
                    tx,
                    &AccessScope::for_tenant(op.tenant_id),
                    op.tenant_id,
                    &work.endpoint(),
                    key,
                )
                .await?;
            }
            Ok(())
        })
    })
    .await
}
/// A create still before its write (`reserving`), with or without a receipt.
fn reserving_create(op: &entity::Model, state: OpState) -> bool {
    state == OpState::Reserving && op.kind == OpKind::Create.as_str()
}
/// A create in `reserving` that never learned a reservation id.
fn unreserved_create(op: &entity::Model, state: OpState) -> bool {
    reserving_create(op, state) && op.reservation_id.is_none()
}
/// What the loop does with an op before any registry call.
enum Gate {
    /// The op finished: hand back its stored answer.
    Finished,
    /// A door's create was cancelled before its reservation outcome was known.
    Cancelled,
    /// The ticker found a create it must not reserve for: cancel it.
    Abandon,
    /// Observe the registry and commit the observation.
    Observe,
}
fn gate(caller: Caller, op: &entity::Model, work: &Work, current: OpState) -> Gate {
    if caller == Caller::Door && work.cancelled() {
        // Cancelled before its reservation outcome was known, by this door or, past the
        // in-flight grace, by the ticker: nothing was written and the key is free again.
        Gate::Cancelled
    } else if current == OpState::Done {
        Gate::Finished
    } else if caller == Caller::Ticker && unreserved_create(op, current) {
        Gate::Abandon
    } else {
        Gate::Observe
    }
}
/// How many exhausted transaction budgets a door spends on a create that is
/// still `reserving` before it cancels and answers 409 `CONTENDED`.
const PRE_WRITE_CONTENTION_ROUNDS: u32 = 8;
/// Whether a failed transaction's error is the lost compare-and-swap of a racing driver, read
/// from the typed error before it is rendered (whole-branch review PS-42).
fn contended(error: &DoorError) -> bool {
    matches!(
        error,
        DoorError::Repo(RepoError::Conflict {
            code: ops::REFERENCE_OP_CONTENDED
        })
    )
}
/// The door's 409 after one transaction's retry budget, distinct from a lost
/// compare-and-swap of the op row ([`contended`]).
fn transaction_contended(error: &DoorError) -> bool {
    matches!(
        error,
        DoorError::Repo(RepoError::Conflict {
            code: support::CONTENDED
        })
    )
}
fn answered_contended(error: &CanonicalError) -> bool {
    error_code(error).as_deref() == Some(support::CONTENDED)
}
/// The code of a failed transaction's typed error: a repository conflict's own, or the reason of
/// a refusal the gear built with its code (PS-42). Anything else has none.
fn door_code(error: &DoorError) -> Option<String> {
    match error {
        DoorError::Repo(RepoError::Conflict { code }) => Some((*code).to_owned()),
        DoorError::Api(error) => error_code(error),
        DoorError::Repo(_) | DoorError::Generation { .. } | DoorError::SelectionMoved => None,
    }
}
/// Drive a durable op until terminal completion or the next scheduled retry.
///
/// A door that gets no definite answer before the write cancels its create and answers 503
/// (nothing written, key released, any receipt released by the cancellation). Any other error
/// that ends a door's drive after its reserve and before its write (a 409 `CONTENDED`, a 500)
/// cancels the create the same way before it is answered. The ticker never makes a first
/// reservation on a user's behalf: it cancels a create still `reserving` without a reservation
/// id the same way.
/// # Errors
/// Returns registry unavailability or a storage failure; the operation remains durable.
pub async fn drive(
    state: &Arc<AuthoringState>,
    ctx: &SecurityContext,
    id: Uuid,
    clock: Arc<dyn Clock>,
    caller: Caller,
) -> Result<Option<Receipt>, CanonicalError> {
    let tenant = ctx.subject_tenant_id();
    let scope = AccessScope::for_tenant(tenant);
    let mut pre_write_rounds = 0u32;
    for _ in 0..32 {
        let op = ops::find(&state.db.conn().map_err(conn_failure)?, &scope, tenant, id)
            .await
            .map_err(|e| CanonicalError::from(DoorError::Repo(e)))?
            .ok_or_else(corrupt)?;
        let work = Work::read(&op)?;
        let current = parse_state(&op)?;
        match gate(caller, &op, &work, current) {
            Gate::Cancelled => return Err(support::unavailable()),
            Gate::Finished => return Ok(work.receipt.or(work.refusal)),
            Gate::Abandon => match abandon(state, &op, work, clock.clone()).await {
                Err(error) if !contended(&error) => return Err(error.into()),
                _ => continue,
            },
            Gate::Observe => {}
        }
        match step(
            state,
            ctx,
            &op,
            work.clone(),
            current,
            caller,
            clock.clone(),
        )
        .await
        {
            // One transaction budget of serialization failures is not the end of the
            // drive. Two creates overlap on the policy intern, and Postgres aborts one
            // with a read/write dependency while the entry key is still free or already
            // taken. Trying the step again lets the survivor answer 201 and the other
            // 409 ENTRY_KEY_TAKEN. A create still reserving after several such budgets
            // is cancelled, so a lasting contention still answers 409 CONTENDED and
            // writes nothing (D-401). A confirm that fails the same way is tried again:
            // the entry is already written, and CONTENDED is not that answer.
            Err(error) if caller == Caller::Door && answered_contended(&error) => {
                if reserving_create(&op, current) && op.reservation_id.is_some() {
                    pre_write_rounds += 1;
                    if pre_write_rounds >= PRE_WRITE_CONTENTION_ROUNDS {
                        match cancel_then(state, caller, &op, work, current, clock.clone(), error)
                            .await
                        {
                            Ok(()) => continue,
                            Err(error) => return Err(error),
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            other => other?,
        }
    }
    Err(unavailable())
}
/// One observation and its commit. `Ok` loops again; `Err` ends the drive with that answer.
async fn step(
    state: &AuthoringState,
    ctx: &SecurityContext,
    op: &entity::Model,
    work: Work,
    current: OpState,
    caller: Caller,
    clock: Arc<dyn Clock>,
) -> Result<(), CanonicalError> {
    warn_past_threshold(op);
    let registry = super::reference_registry::resolve(&state.hub);
    let carried = match carried(state, op, current, clock.as_ref()).await {
        Ok(carried) => carried,
        Err(error) => return cancel_then(state, caller, op, work, current, clock, error).await,
    };
    let (event, write, refusal) = match observe(registry, ctx, op, carried).await {
        Ok(observation) => observation,
        Err(error) => return cancel_then(state, caller, op, work, current, clock, error).await,
    };
    if caller == Caller::Door
        && event == Event::RegistryUnavailable
        && reserving_create(op, current)
    {
        return give_up(state, op, work, clock).await;
    }
    let retry = matches!(
        event,
        Event::RegistryUnavailable | Event::ConfirmFailed | Event::ReleaseFailed
    );
    let mut observed = work.clone();
    if let Some(refusal) = refusal {
        observed.refusal = Some(refusal);
    }
    match commit_observation(state, ctx, op, observed, event, write, clock.clone()).await {
        Ok(()) if retry => Err(unavailable()),
        Err(error)
            if refuses_the_write(parse_ref_kind(op)?, &error) && current == OpState::Reserving =>
        {
            cancel(state, op, work, error.into(), clock).await
        }
        Err(error) if transaction_contended(&error) => Err(error.into()),
        Err(error) if !contended(&error) => {
            cancel_then(state, caller, op, work, current, clock, error.into()).await
        }
        _ => Ok(()),
    }
}
/// A door's create that holds a receipt but is not written yet, and whose drive ends with
/// `error` (409 `CONTENDED`, a 500): cancel it first, exactly as [`give_up`] does, so an
/// answered error never becomes an entry later. A lost race means another driver moved the op
/// first, and the loop re-reads it. A failed cancellation is logged and the error is answered
/// as it is: the ticker rule applies to that op unchanged.
async fn cancel_then(
    state: &AuthoringState,
    caller: Caller,
    op: &entity::Model,
    work: Work,
    current: OpState,
    clock: Arc<dyn Clock>,
    error: CanonicalError,
) -> Result<(), CanonicalError> {
    if caller != Caller::Door || !reserving_create(op, current) || op.reservation_id.is_none() {
        return Err(error);
    }
    match abandon(state, op, work, clock).await {
        Ok(()) => Err(error),
        Err(lost) if contended(&lost) => Ok(()),
        Err(failed) => {
            let failed = CanonicalError::from(failed);
            tracing::warn!(op_id=%op.op_id, error=%failed, diagnostic=failed.diagnostic().unwrap_or_default(), "pricing create not cancelled before its error answer");
            Err(error)
        }
    }
}
/// The one warning of an op retried ten times or more, naming what an operator needs to find it
/// (PS-54).
fn warn_past_threshold(op: &entity::Model) {
    if op.attempts >= 10 {
        tracing::warn!(
            op_id = %op.op_id,
            attempts = op.attempts,
            tenant_id = %op.tenant_id,
            kind = %op.kind,
            state = %op.state,
            ref_kind = %op.ref_kind,
            ref_id = %op.ref_id,
            sku_id = %op.sku_id,
            last_error = op.last_error.as_deref().unwrap_or_default(),
            "pricing reference operation retry threshold reached"
        );
    }
}
/// The door got no definite answer before the write (the reserve, or the SKU re-read after a
/// successful reserve): cancel the create and answer 503, so a 503 never becomes an entry. A
/// lost race means another driver moved the op first; the loop re-reads it.
async fn give_up(
    state: &AuthoringState,
    op: &entity::Model,
    work: Work,
    clock: Arc<dyn Clock>,
) -> Result<(), CanonicalError> {
    match abandon(state, op, work, clock).await {
        Ok(()) => Err(support::unavailable()),
        Err(error) if contended(&error) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
/// A local refusal of Tx B that cancels the op (and releases its reservation), per kind, judged
/// on the typed error before it is rendered (PS-42).
fn refuses_the_write(kind: RefKind, error: &DoorError) -> bool {
    let Some(code) = door_code(error) else {
        return false;
    };
    match kind {
        RefKind::Entry => matches!(
            code.as_str(),
            "ENTRY_KEY_TAKEN"
                | "DIM_NOT_DECLARED"
                | "BOOK_NOT_FOUND"
                | "BOOK_ARCHIVED"
                | "CHARGE_KIND_SKU_TYPE"
                | "ENTRY_NOT_FOUND"
        ),
        RefKind::PlanItem => plan_item::REFUSES_THE_WRITE.contains(&code.as_str()),
    }
}
/// Commit one observation in one transaction: the entry write it carries, the completion
/// work of a finishing op and the op's own compare-and-swap transition, or none of them.
async fn commit_observation(
    state: &AuthoringState,
    ctx: &SecurityContext,
    op: &entity::Model,
    work: Work,
    event: Event,
    write: Option<Write>,
    clock: Arc<dyn Clock>,
) -> Result<(), DoorError> {
    let (op, ctx, db) = (op.clone(), ctx.clone(), state.db.db());
    support::transaction_door_with_events(&db, &state.outbox, move |tx, outbox| {
        let (op, work, ctx, clock, event, write) = (
            op.clone(),
            work.clone(),
            ctx.clone(),
            clock.clone(),
            event.clone(),
            write.clone(),
        );
        Box::pin(
            async move { commit(tx, &outbox, &ctx, &op, work, event, write, clock.as_ref()).await },
        )
    })
    .await
}
#[expect(
    clippy::too_many_arguments,
    reason = "the observed op, its work and the observation are the transaction's operands"
)]
async fn commit(
    tx: &(impl DBRunner + Sync),
    outbox: &super::events::TxOutbox,
    ctx: &SecurityContext,
    op: &entity::Model,
    mut work: Work,
    event: Event,
    write: Option<Write>,
    clock: &dyn Clock,
) -> Result<(), DoorError> {
    let scope = AccessScope::for_tenant(op.tenant_id);
    if ops::find(tx, &scope, op.tenant_id, op.op_id)
        .await?
        .as_ref()
        != Some(op)
    {
        return Err(RepoError::Conflict {
            code: ops::REFERENCE_OP_CONTENDED,
        }
        .into());
    }
    let now = clock.now();
    let (planned, effects) = reference_op::next(
        Op {
            state: parse_state(op)?,
            reservation_id: op.reservation_id,
            refusal: op.last_error.clone(),
        },
        event.clone(),
    )
    .map_err(|e| illegal(op, &e))?;
    match write {
        Some(Write::Entry(entry)) => write_entry(tx, &scope, op, entry, now).await?,
        Some(Write::Item(item)) => plan_item::write(tx, &scope, item).await?,
        Some(Write::ItemReceipt(receipt)) => {
            plan_item::write_receipt(tx, &scope, op, receipt, now).await?;
        }
        None => {}
    }
    if planned.state == OpState::Done {
        if op.state == OpState::Written.as_str() {
            work.receipt = Some(match parse_ref_kind(op)? {
                RefKind::Entry => finish_written(tx, ctx, op, &work, &effects, now).await?,
                RefKind::PlanItem => {
                    plan_item::finish_written(tx, ctx, op, &work, &effects, now).await?
                }
            });
        } else if replaces_a_receipt(op) {
            mark_lost(tx, outbox, ctx, &work, op, now).await?;
        }
        answer_key(tx, op, &work).await?;
    }
    advance(tx, op, &work, event, clock).await?;
    Ok(())
}
/// Counters a test installs around one door call. Absent outside that scope.
#[cfg(feature = "test-support")]
struct CreateFaults {
    writes: AtomicUsize,
    confirms: AtomicUsize,
}
#[cfg(feature = "test-support")]
tokio::task_local! {
    static CREATE_FAULTS: CreateFaults;
}
/// Run `future` so the next `writes` entry inserts and the next `confirms`
/// confirms fail as retryable contention, then proceed. The counters live on
/// the calling task, so a neighbour test does not see them.
#[cfg(feature = "test-support")]
pub async fn with_create_faults<T>(
    writes: usize,
    confirms: usize,
    future: impl std::future::Future<Output = T>,
) -> T {
    CREATE_FAULTS
        .scope(
            CreateFaults {
                writes: AtomicUsize::new(writes),
                confirms: AtomicUsize::new(confirms),
            },
            future,
        )
        .await
}
#[cfg(feature = "test-support")]
fn take_fault(slot: impl Fn(&CreateFaults) -> &AtomicUsize) -> bool {
    CREATE_FAULTS
        .try_with(|faults| {
            slot(faults)
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        })
        .unwrap_or(false)
}
/// A driver error both engines classify as retryable contention.
#[cfg(feature = "test-support")]
fn contention_fault() -> DoorError {
    RepoError::Driver {
        context: "entry create".into(),
        source: sea_orm::DbErr::Custom(
            "could not serialize access due to concurrent update (code: 5) database is locked"
                .into(),
        ),
    }
    .into()
}
/// Tx B: a create inserts its entry; a rereserve re-points its entry at the new receipt. Neither
/// writes into an archived book (D-522): the write is refused `BOOK_ARCHIVED`, and the op is
/// cancelled, which releases the reservation it made.
async fn write_entry(
    tx: &(impl DBRunner + Sync),
    scope: &AccessScope,
    op: &entity::Model,
    mut entry: price_book_entry::Model,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    if crate::infra::storage::repo::book_repo::find(tx, scope, op.tenant_id, entry.book_id)
        .await?
        .is_some_and(|book| book.archived_at.is_some())
    {
        return Err(support::conflict("BOOK_ARCHIVED").into());
    }
    if op.kind != OpKind::Rereserve.as_str() {
        #[cfg(feature = "test-support")]
        if take_fault(|faults| &faults.writes) {
            return Err(contention_fault());
        }
        if let Some(model) = taken_model(tx, scope, op, &entry).await? {
            entry.model = model;
        }
        let Target::PriceBookEntry { input, .. } = Work::read(op)?.target else {
            return Err(corrupt().into());
        };
        if let Some(content) = &input.usage_rating_policy {
            let policy = crate::infra::storage::repo::usage_policy_repo::intern(
                tx,
                scope,
                op.tenant_id,
                op.created_by,
                content,
                now,
            )
            .await?;
            entry.usage_policy_id = Some(policy.policy_id);
            entry.usage_policy_version = Some(policy.version.parse().map_err(|_| corrupt())?);
            entry.usage_policy_digest = Some(policy.digest);
        }
        price_book_entry_repo::insert(tx, scope, entry).await?;
        return Ok(());
    }
    // A lost entry may be deleted while its re-reservation is in flight: cancel, which
    // releases the new reservation.
    let current = price_book_entry_repo::find(tx, scope, op.tenant_id, op.ref_id)
        .await?
        .ok_or_else(|| support::conflict("ENTRY_NOT_FOUND"))?;
    if current.charge_kind != entry.charge_kind {
        return Err(support::conflict("CHARGE_KIND_SKU_TYPE").into());
    }
    price_book_entry_repo::set_reference(
        tx,
        scope,
        op.tenant_id,
        op.ref_id,
        current.version,
        ReferenceState::ConfirmationPending,
        entry.reservation_id,
        now,
    )
    .await?;
    Ok(())
}
/// The model of the entry that already holds a create's key, for a create stored before
/// `m20260926_000013` (its input has no model, D-427). Such a create was posted under the key of
/// its day — book, SKU, charge kind and period — and [`model_to_write`] gave it the charge kind's
/// default. When an entry holds that key, its model replaces the default, so the insert meets
/// `ENTRY_KEY_TAKEN` as the contract the create was called under answers, and no second entry is
/// written beside it in a model nobody chose. Read in Tx B, the insert's own transaction.
async fn taken_model(
    tx: &(impl DBRunner + Sync),
    scope: &AccessScope,
    op: &entity::Model,
    entry: &price_book_entry::Model,
) -> Result<Option<String>, DoorError> {
    let Target::PriceBookEntry { input, .. } = Work::read(op)?.target else {
        return Err(corrupt().into());
    };
    if input.model.is_some() {
        return Ok(None);
    }
    // The key index reads the period as `coalesce(period, '')`.
    let key = |e: &price_book_entry::Model| {
        (
            e.sku_id,
            e.charge_kind.clone(),
            e.period.clone().unwrap_or_default(),
        )
    };
    Ok(
        price_book_entry_repo::for_book(tx, scope, entry.tenant_id, entry.book_id)
            .await?
            .into_iter()
            .find(|holder| {
                key(holder) == key(entry) && holder.usage_policy_digest == entry.usage_policy_digest
            })
            .map(|holder| holder.model),
    )
}
/// Tx C. A confirmed receipt confirms the entry. A receipt released before its confirm keeps
/// the entry `confirmation_pending` and starts a `rereserve_entry` op in this transaction;
/// that op alone decides between confirmed and lost (D-401), so a create is never answered
/// `lost` for a reservation that can still be replaced.
async fn finish_written(
    tx: &(impl DBRunner + Sync),
    ctx: &SecurityContext,
    op: &entity::Model,
    work: &Work,
    effects: &[Effect],
    now: OffsetDateTime,
) -> Result<Receipt, DoorError> {
    #[cfg(feature = "test-support")]
    if take_fault(|faults| &faults.confirms) {
        return Err(contention_fault());
    }
    let scope = AccessScope::for_tenant(op.tenant_id);
    let mut entry = price_book_entry_repo::find(tx, &scope, op.tenant_id, op.ref_id)
        .await?
        .ok_or_else(corrupt)?;
    if effects.contains(&Effect::Rereserve) {
        // Due at once: no door drives this op, so no in-flight grace applies.
        ops::insert(tx, &scope, rereserve_op(ctx, &entry, now, now)?).await?;
        return Ok(Receipt::entry(tx, entry).await?);
    }
    price_book_entry_repo::set_reference(
        tx,
        &scope,
        op.tenant_id,
        op.ref_id,
        entry.version,
        ReferenceState::Confirmed,
        entry.reservation_id,
        now,
    )
    .await?;
    entry.reference_state = ReferenceState::Confirmed.as_str().into();
    entry.version += 1;
    entry.updated_at = now;
    support::audit(
        tx,
        ctx,
        work.correlation,
        "price_book_entry.confirm",
        entry.id,
        entry.version,
    )
    .await?;
    Ok(Receipt::entry(tx, entry).await?)
}
/// A `rereserve_entry` op for a live entry, due at `due`.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn rereserve_op(
    ctx: &SecurityContext,
    entry: &price_book_entry::Model,
    now: OffsetDateTime,
    due: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    let work = Work {
        target: Target::PriceBookEntry {
            book_id: entry.book_id,
            input: EntryInput::of(entry),
        },
        correlation: Uuid::now_v7(),
        refusal: None,
        receipt: None,
        outcome: None,
        reason: None,
    };
    let reference = Ref {
        kind: RefKind::Entry,
        id: entry.id,
        sku_id: entry.sku_id,
    };
    let mut op = new_op(ctx, reference, &work, OpKind::Rereserve, None, None, now)?;
    op.tenant_id = entry.tenant_id;
    op.next_attempt_at = due;
    Ok(op)
}
/// A `release` op for an entry of a book being archived (D-522): it releases the entry's
/// reservation, as a delete's op does, and records [`BOOK_ARCHIVED_REASON`]. Due after the
/// in-flight grace, so the archive door drives it first; the ticker finishes what the door does
/// not.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn release_op(
    ctx: &SecurityContext,
    entry: &price_book_entry::Model,
    correlation: Uuid,
    now: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    let work = Work {
        target: Target::PriceBookEntry {
            book_id: entry.book_id,
            input: EntryInput::of(entry),
        },
        correlation,
        refusal: None,
        receipt: None,
        outcome: None,
        reason: Some(BOOK_ARCHIVED_REASON.to_owned()),
    };
    let reference = Ref {
        kind: RefKind::Entry,
        id: entry.id,
        sku_id: entry.sku_id,
    };
    new_op(
        ctx,
        reference,
        &work,
        OpKind::Release,
        Some(entry.reservation_id),
        None,
        now,
    )
}
/// Products refusals that mean the SKU admits no reservation: fenced, retiring or retired.
/// Only these make a live entry lost; every other refusal of a re-reservation is retried.
pub const LOSING_REFUSALS: [&str; 3] = ["SKU_FENCED", "SKU_RETIRING", "SKU_RETIRED"];
fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail("REGISTRY_UNAVAILABLE: reference work will be retried")
        .create()
}
/// Stable registry business reason, independent of its resource error type.
#[must_use]
pub fn error_code(error: &CanonicalError) -> Option<String> {
    match error {
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
        _ => None,
    }
}
async fn cancel(
    state: &AuthoringState,
    op: &entity::Model,
    mut work: Work,
    error: CanonicalError,
    clock: Arc<dyn Clock>,
) -> Result<(), CanonicalError> {
    let code = error_code(&error).unwrap_or_else(|| "WRITE_REFUSED".into());
    // What the door refuses as input stays an input refusal (400, D-403) when Tx B finds it: a
    // key removed from the registry since the door checked it, an item's entry that is no
    // longer of its revision's book or never was of its SKU, or a revision already full.
    let error = match (parse_ref_kind(op)?, code.as_str()) {
        (RefKind::Entry, "DIM_NOT_DECLARED") => {
            support::invalid("dimension_key", "DIM_NOT_DECLARED")
        }
        (RefKind::PlanItem, "ITEM_BOOK_FOREIGN" | "ITEM_ENTRY_SKU_MISMATCH") => {
            support::invalid("price_book_entry_id", &code)
        }
        (RefKind::PlanItem, "REVISION_ITEMS_TOO_MANY") => support::invalid("items", &code),
        _ => error,
    };
    work.refusal = Some(Receipt::error(error).await?);
    let op = op.clone();
    support::transaction(&state.db.db(), move |tx| {
        let (op, work, clock, code) = (op.clone(), work.clone(), clock.clone(), code.clone());
        Box::pin(async move {
            advance(tx, &op, &work, Event::SkuRefused { code }, clock.as_ref()).await?;
            Ok(())
        })
    })
    .await
}
/// What Tx B writes for an observation, per kind.
#[derive(Debug, Clone)]
enum Write {
    /// A created entry, or a rereserved entry carrying its new receipt.
    Entry(price_book_entry::Model),
    /// A created plan item.
    Item(plan_item_entity::Model),
    /// The new receipt of an attached or rereserved plan item.
    ItemReceipt(Uuid),
}
type Observation = (Event, Option<Write>, Option<Receipt>);
/// Products 409 codes a retry can clear: a lost race, not a refusal.
const RETRYABLE_CONFLICTS: [&str; 2] = ["UNIT_CONTENDED", "CONTENDED"];
/// A Products answer that settles the call: a client error, except rate limiting (429) and
/// a contention conflict. Those, 5xx and timeouts are unavailability, never a refusal.
#[must_use]
pub fn definite_refusal(error: &CanonicalError) -> bool {
    let status = error.status_code();
    (400..500).contains(&status)
        && status != 429
        && !error_code(error).is_some_and(|code| RETRYABLE_CONFLICTS.contains(&code.as_str()))
}
/// Products answers 404 for a reservation id it does not hold (a restore from an older backup
/// is the known case). The reconciliation reads the same answer as released.
fn unknown_reservation(error: &CanonicalError) -> bool {
    error.status_code() == 404
}
/// A registry call the op retries: the op keeps only a code, so the call's own error is logged
/// here (PS-05).
fn unanswered(op: &entity::Model, call: &str, error: &CanonicalError) {
    tracing::warn!(
        op_id = %op.op_id,
        call,
        status = error.status_code(),
        error = %error,
        diagnostic = error.diagnostic().unwrap_or_default(),
        "pricing reference registry call will be retried"
    );
}
/// Whether a plan item's create may take a deprecated SKU (D-465): its plan's published revision
/// in effect on the clock's day carries it, as the item door judged. Read only where
/// [`observe_sku`] judges a create's SKU (a plan item's create, reserving, with its receipt);
/// `false` everywhere else, and for a revision that is gone (its write then refuses it). The read
/// is outside Tx B: a revision that stops carrying the SKU meanwhile is judged again by the checks
/// at submit and at apply (D-408).
async fn carried(
    state: &AuthoringState,
    op: &entity::Model,
    current: OpState,
    clock: &dyn Clock,
) -> Result<bool, CanonicalError> {
    if current != OpState::Reserving
        || op.reservation_id.is_none()
        || op.kind != OpKind::Create.as_str()
        || parse_ref_kind(op)? != RefKind::PlanItem
    {
        return Ok(false);
    }
    let Target::PlanItem { revision_id, .. } = Work::read(op)?.target else {
        return Err(corrupt());
    };
    let tenant = op.tenant_id;
    let conn = state.db.conn().map_err(conn_failure)?;
    let Some(revision) =
        plan_revision_repo::find(&conn, &AccessScope::for_tenant(tenant), tenant, revision_id)
            .await
            .map_err(stored_failure)?
    else {
        return Ok(false);
    };
    Ok(
        super::plan_revisions::published_skus(&conn, tenant, revision.plan_id, clock.now().date())
            .await
            .map_err(stored_failure)?
            .contains(&op.sku_id),
    )
}
async fn observe(
    registry: Result<Arc<dyn ReferenceRegistryV1>, CanonicalError>,
    ctx: &SecurityContext,
    op: &entity::Model,
    carried: bool,
) -> Result<Observation, CanonicalError> {
    let current = parse_state(op)?;
    let unavailable = || {
        (
            match current {
                OpState::Reserving => Event::RegistryUnavailable,
                OpState::Written => Event::ConfirmFailed,
                _ => Event::ReleaseFailed,
            },
            None,
            None,
        )
    };
    let cancelled = Work::read(op)?.cancelled();
    if matches!(current, OpState::Cancelling | OpState::Releasing)
        && op.reservation_id.is_none()
        && !cancelled
    {
        // A definite refusal before any receipt: nothing was reserved.
        return Ok((Event::Released, None, None));
    }
    let registry = match registry {
        Ok(registry) => registry,
        Err(error) => {
            unanswered(op, "registry lookup", &error);
            return Ok(unavailable());
        }
    };
    match current {
        OpState::Reserving if op.reservation_id.is_none() => {
            observe_reserve(registry.as_ref(), ctx, op).await
        }
        OpState::Reserving => observe_sku(registry.as_ref(), ctx, op, carried).await,
        OpState::Written => {
            let event = match registry
                .confirm(ctx, op.tenant_id, op.reservation_id.ok_or_else(corrupt)?)
                .await
            {
                Ok(()) => Event::Confirmed,
                // A reservation Products does not know (404, for example after a restore from
                // an older backup) is gone just like a released one: re-reserve the entry.
                Err(error)
                    if error_code(&error).as_deref() == Some("REFERENCE_RELEASED")
                        || unknown_reservation(&error) =>
                {
                    Event::ReleasedOnConfirm
                }
                Err(error) => {
                    unanswered(op, "confirm", &error);
                    Event::ConfirmFailed
                }
            };
            Ok((event, None, None))
        }
        OpState::Cancelling | OpState::Releasing => {
            observe_release(registry.as_ref(), ctx, op).await
        }
        OpState::Done => Err(corrupt()),
    }
}
/// A create's first reserve, before it knows a reservation id.
async fn observe_reserve(
    registry: &dyn ReferenceRegistryV1,
    ctx: &SecurityContext,
    op: &entity::Model,
) -> Result<Observation, CanonicalError> {
    match registry
        .reserve(
            ctx,
            op.tenant_id,
            op.sku_id,
            products_kind(parse_ref_kind(op)?),
            op.ref_id,
        )
        .await
    {
        Ok(receipt) => Ok((
            Event::Reserved {
                id: receipt.reservation_id,
            },
            None,
            None,
        )),
        Err(error) if definite_refusal(&error) => {
            let code = error_code(&error).unwrap_or_else(|| "SKU_REFUSED".into());
            if retries_a_refusal(op, ctx) && !LOSING_REFUSALS.contains(&code.as_str()) {
                // Only a SKU that admits no reservation loses a live entry or a copied item: an
                // attach has the rereserve shape (D-413). Any other refusal (the door caller's
                // own grant, say) is retried, and the ticker finishes it as the system actor; a
                // refusal given to the system actor ends an attach.
                unanswered(op, "reserve", &error);
                return Ok((Event::RegistryUnavailable, None, None));
            }
            Ok((
                Event::ReserveRefused { code },
                None,
                Some(Receipt::error(error).await?),
            ))
        }
        Err(error) => {
            unanswered(op, "reserve", &error);
            Ok((Event::RegistryUnavailable, None, None))
        }
    }
}
/// A cancellation's or a removal's release of its reservation.
async fn observe_release(
    registry: &dyn ReferenceRegistryV1,
    ctx: &SecurityContext,
    op: &entity::Model,
) -> Result<Observation, CanonicalError> {
    let tenant = op.tenant_id;
    let result = match op.reservation_id {
        // A reservation Products does not know holds nothing: it counts as released.
        Some(id) => match registry.release(ctx, tenant, id).await {
            Err(error) if unknown_reservation(&error) => Ok(()),
            other => other,
        },
        // The reserve outcome was never learned. Reserve is idempotent per logical reference, so
        // it answers the reservation the lost call made (or makes one), and releasing that leaves
        // none. A definite refusal means none can exist: a fence requires zero live references.
        None => match registry
            .reserve(
                ctx,
                tenant,
                op.sku_id,
                products_kind(parse_ref_kind(op)?),
                op.ref_id,
            )
            .await
        {
            Ok(receipt) => registry.release(ctx, tenant, receipt.reservation_id).await,
            Err(error) if definite_refusal(&error) => Ok(()),
            Err(error) => Err(error),
        },
    };
    if let Err(error) = &result {
        unanswered(op, "release", error);
    }
    Ok((
        if result.is_ok() {
            Event::Released
        } else {
            Event::ReleaseFailed
        },
        None,
        None,
    ))
}

async fn observe_sku(
    registry: &dyn ReferenceRegistryV1,
    ctx: &SecurityContext,
    op: &entity::Model,
    carried: bool,
) -> Result<Observation, CanonicalError> {
    let tenant = op.tenant_id;
    let sku = match registry.sku_for_write(ctx, tenant, op.sku_id).await {
        Ok(sku) => sku,
        // An op that replaces a receipt (an attach, a rereserve) holds a reference the SKU
        // already admitted: a refusal of the READ says nothing about the SKU (the caller's own
        // grant, say), so it is retried and the ticker finishes it as the system actor. Only a
        // lifecycle answer below refuses it (D-413 "the rereserve shape"), or, for an attach, a
        // refusal given to the system actor itself (`retries_a_refusal`).
        Err(error) if definite_refusal(&error) && retries_a_refusal(op, ctx) => {
            unanswered(op, "SKU re-read", &error);
            return Ok((Event::RegistryUnavailable, None, None));
        }
        Err(error) if definite_refusal(&error) => {
            return Ok((
                Event::SkuRefused {
                    code: "SKU_REFUSED".into(),
                },
                None,
                Some(Receipt::error(error).await?),
            ));
        }
        Err(error) => {
            unanswered(op, "SKU re-read", &error);
            return Ok((Event::RegistryUnavailable, None, None));
        }
    };
    let kind = parse_ref_kind(op)?;
    // The lifecycle rules of every kind: a draft, retiring or retired SKU takes no reference,
    // and a create takes no deprecated SKU, except a plan item's whose plan's published revision
    // in effect carries it (`carried`, D-465). An attach and a rereserve do: the reference they
    // replace already protected that SKU (D-413). The kind adds its own rule on the SKU's type.
    let refusal = if sku.retire_pending {
        Some("SKU_RETIRING")
    } else {
        match sku.lifecycle {
            Lifecycle::Draft => Some("SKU_DRAFT"),
            Lifecycle::Deprecated if op.kind == OpKind::Create.as_str() && !carried => {
                Some("SKU_DEPRECATED")
            }
            Lifecycle::Retired => Some("SKU_RETIRING"),
            Lifecycle::Published | Lifecycle::Deprecated => match kind {
                RefKind::Entry => charge_kind_for(sku.r#type).err().map(|e| e.code),
                RefKind::PlanItem => plan_item::type_refusal(sku.r#type),
            },
        }
    };
    if let Some(code) = refusal {
        return Ok((
            Event::SkuRefused { code: code.into() },
            None,
            Some(Receipt::error(sku_refusal_answer(kind, code)).await?),
        ));
    }
    match kind {
        RefKind::Entry => entry_written(op, &sku).await,
        RefKind::PlanItem => plan_item::written(op),
    }
}
/// The answer a create's key records for a SKU its re-read refuses. An item's create answers what
/// the item door answers for the same SKU, a 400 on `sku_id` (D-403): `ITEM_SKU_DEPRECATED` for a
/// deprecated SKU, `ITEM_BUNDLE_SKU` for a bundle; the op's `SkuRefused` code stays the refusal's.
/// Every other refusal is a 409 with its code.
fn sku_refusal_answer(kind: RefKind, code: &'static str) -> CanonicalError {
    match (kind, code) {
        (RefKind::PlanItem, "SKU_DEPRECATED") => support::invalid("sku_id", "ITEM_SKU_DEPRECATED"),
        (RefKind::PlanItem, "ITEM_BUNDLE_SKU") => support::invalid("sku_id", "ITEM_BUNDLE_SKU"),
        _ => support::conflict(code),
    }
}
/// Tx B's judgement of the input's model (D-427) against the charge kind of the SKU type the
/// reservation froze: the model to write, or the code of its 400 refusal. An op stored before
/// `m20260926_000013` has none and resolves to the charge kind's default, unless an entry already
/// holds its key: then Tx B writes that entry's model ([`taken_model`]) and meets the key.
fn model_to_write(
    input: &EntryInput,
    kind: crate::domain::price_book_entry::ChargeKind,
) -> Result<Model, &'static str> {
    match input.model.as_deref() {
        None => Ok(default_model(kind)),
        Some(text) => match text.parse::<Model>() {
            Ok(model) if model_allowed(kind, model) => Ok(model),
            Ok(_) => Err("MODEL_KIND_CHARGEKIND_MISMATCH"),
            Err(_) => Err("MODEL_INVALID"),
        },
    }
}
/// The entry Tx B writes once its SKU admits it: the create's new entry, or the rereserved
/// entry with its new receipt.
async fn entry_written(
    op: &entity::Model,
    sku: &bss_products_sdk::models::Sku,
) -> Result<Observation, CanonicalError> {
    let Target::PriceBookEntry { book_id, input } = Work::read(op)?.target else {
        return Err(corrupt());
    };
    // The door checked the period before reserving; this re-read repeats it against the
    // type the reservation froze. Either way it is an input refusal: 400 (D-403).
    if !crate::domain::price_book_entry::period_valid(sku.r#type, input.period.as_deref()) {
        return Ok((
            Event::SkuRefused {
                code: "ENTRY_PERIOD_INVALID".into(),
            },
            None,
            Some(Receipt::error(support::invalid("period", "ENTRY_PERIOD_INVALID")).await?),
        ));
    }
    // The door judged the model against a fresh read; this re-read judges it again against the
    // type the reservation froze (D-427). A refusal is an input refusal: 400 (D-403).
    let kind = charge_kind_for(sku.r#type).map_err(|_| corrupt())?;
    let model = match model_to_write(&input, kind) {
        Ok(model) => model,
        Err(code) => {
            return Ok((
                Event::SkuRefused { code: code.into() },
                None,
                Some(Receipt::error(support::invalid("model", code)).await?),
            ));
        }
    };
    if op.kind == OpKind::Create.as_str() {
        let refusal = match (input.schema_version, &input.usage_rating_policy, kind) {
            (Some(1..=3), Some(policy), crate::domain::price_book_entry::ChargeKind::Usage) => {
                match &input.meter_evidence {
                    Some(evidence) => crate::infra::meter_semantics::validate(
                        &policy.as_ref().into(),
                        sku,
                        &evidence.as_ref().into(),
                    )
                    .err()
                    .map(|_| "METER_POLICY_MISMATCH"),
                    None if input.schema_version == Some(1) => {
                        crate::domain::usage_policy::validate_policy_shape(&policy.as_ref().into())
                            .err()
                            .map(|e| e.code)
                    }
                    None => Some("METER_EVIDENCE_MISSING"),
                }
            }
            (Some(1..=3), None, crate::domain::price_book_entry::ChargeKind::Usage) => {
                Some("MISSING_RATING_POLICY")
            }
            (Some(1..=3), Some(_), _) => Some("UNEXPECTED_RATING_POLICY"),
            (None | Some(1..=3), None, _) => None,
            _ => return Err(corrupt()),
        };
        if let Some(code) = refusal {
            return Ok((
                Event::SkuRefused { code: code.into() },
                None,
                Some(Receipt::error(support::invalid("usage_rating_policy", code)).await?),
            ));
        }
    }
    let entry = price_book_entry::Model {
        id: op.ref_id,
        tenant_id: op.tenant_id,
        book_id,
        sku_id: op.sku_id,
        charge_kind: kind.as_str().into(),
        period: input.period,
        model: model.as_str().into(),
        usage_policy_id: None,
        usage_policy_version: None,
        usage_policy_digest: None,
        usage_sku_version: match (kind, input.usage_rating_policy.is_some()) {
            (crate::domain::price_book_entry::ChargeKind::Usage, true) => {
                Some(sku.published_version)
            }
            _ => None,
        },
        dimension_key: input.dimension_key,
        invoice_line_override: input.invoice_line_override,
        reservation_id: op.reservation_id.ok_or_else(corrupt)?,
        reference_state: ReferenceState::ConfirmationPending.as_str().into(),
        version: 1,
        created_at: op.created_at,
        updated_at: op.updated_at,
    };
    Ok((Event::Written, Some(Write::Entry(entry)), None))
}

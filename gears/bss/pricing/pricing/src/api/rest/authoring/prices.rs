//! Draft prices: a single price, a temporary pair or one explicitly closed price; draft-only edits;
//! and the draft `cancel` or `end` of an approved price (D-520, D-521).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-temporary-pair:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-price-pending-guard:p1
use super::{
    dto::{PricingPriceCreate, PricingPriceCreated, PricingPriceDto, PricingPricePatch},
    support::{self, DoorError},
};
use crate::infra::reference_work::Clock;
use crate::{
    domain::{
        RuleError,
        currency::CurrencyScales,
        money,
        price::{self, Eligibility, Price, PriceState},
    },
    infra::{
        prices::{Change, PriceBookEntryContext, Stage},
        storage::{
            RepoError,
            entity::{self, price_book_entry},
            repo::{self, acceptance_repo, price_book_entry_repo, price_repo},
        },
    },
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rust_decimal::Decimal;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Attempts of the create transaction when a concurrent writer took the next `version_no`.
const VERSION_ATTEMPTS: u32 = 3;

/// What a client is told when a minimum fee breaks the price's rules (D-528).
const MIN_FEE_RULE: &str = "min_fee must not be negative and must be a multiple of the increment \
     of the price's currency_scale";
fn refuse(error: RuleError) -> DoorError {
    match error.code {
        "MIN_FEE_INVALID" => support::invalid_because("min_fee", error.code, MIN_FEE_RULE).into(),
        code => support::invalid(price::field_of(code), code).into(),
    }
}
/// A refusal of one half of a new or re-dated pair. The return a temporary price makes copies its
/// fee from the price it returns to, but takes today's scale: a fee that no longer fits that scale
/// is `MIN_FEE_INVALID` on a field the caller did not send, so the message says where it came from.
fn refuse_half(error: RuleError, half: &Price) -> DoorError {
    match (error.code, half.return_of_price_id) {
        ("MIN_FEE_INVALID", Some(back)) => support::invalid_because(
            "min_fee",
            error.code,
            &format!(
                "the min_fee is copied from price {back}, the price the temporary price returns \
                 to, and is not a multiple of the increment of today's currency_scale {}",
                half.currency.scale()
            ),
        )
        .into(),
        _ => refuse(error),
    }
}
/// A price body refusal, worded by its cause: a decimal sent as a JSON number is told to send
/// a string, invalid text to send plain decimal text, a value over a limit gets the limit, and a
/// sign error names the field. The violation's field is the refused decimal's path (D-538):
/// `price.rate`, `price.tiers[1].up_to`. A shape error (`PRICE_MISSING`) is refused as any other
/// rule, on `price`.
fn refuse_price(error: money::DecodeError) -> DoorError {
    match error {
        money::DecodeError::Shape => refuse(error.into()),
        money::DecodeError::JsonNumber { .. }
        | money::DecodeError::Numeric { .. }
        | money::DecodeError::NotCanonical { .. } => {
            support::invalid_because(&error.path(), error.code(), &error.message()).into()
        }
    }
}

/// A draft belongs to its author (D-404): only its creator edits or deletes it, so every
/// number in a unit is its item author's and separation of duties excludes the right person.
fn own_draft(m: &entity::price::Model, ctx: &SecurityContext) -> Result<(), DoorError> {
    if m.created_by == ctx.subject_id() {
        Ok(())
    } else {
        Err(support::forbidden("NOT_DRAFT_AUTHOR").into())
    }
}
fn parse_eligibility(text: &str) -> Result<Eligibility, DoorError> {
    text.parse()
        .map_err(|_| support::invalid("eligibility", "ELIGIBILITY_INVALID").into())
}
/// A minimum fee is a JSON string of plain decimal text, read by the shared parser and
/// normalized: `30.00` is stored as `30`, the one spelling the stored row is read back with. A JSON
/// number, and text with spaces, an exponent or more than 28 digits, is `MIN_FEE_INVALID`, its
/// message saying which; the currency increment and the sign are judged with the price's other
/// rules.
fn parse_min_fee(fee: Option<&serde_json::Value>) -> Result<Option<Decimal>, DoorError> {
    let refused = |message: &str| -> DoorError {
        support::invalid_because("min_fee", "MIN_FEE_INVALID", message).into()
    };
    fee.map(|value| match value {
        serde_json::Value::String(text) => {
            bss_money::parse_decimal(text).map_err(|error| refused(&min_fee_refusal(&error)))
        }
        serde_json::Value::Number(_) => Err(refused(money::DECIMALS_ARE_STRINGS)),
        _ => Err(refused(&min_fee_refusal(
            &bss_money::MoneyError::InvalidDecimal,
        ))),
    })
    .transpose()
}
/// What a client is told about a minimum fee the shared parser refused.
fn min_fee_refusal(error: &bss_money::MoneyError) -> String {
    match error {
        bss_money::MoneyError::AmountOutOfRange => {
            "min_fee has at most 28 digits in all".to_owned()
        }
        _ => format!(
            "min_fee: {}, with at most 28 fractional places",
            money::PLAIN_DECIMAL_TEXT
        ),
    }
}
/// The currency spec a new or edited draft of the entry takes: the book's currency at the scale
/// the currency table gives it today.
/// # Errors
/// 409 `BOOK_CURRENCY_INVALID` when the table no longer knows the book's currency (a non-ISO
/// code taken out of the deployment's table): no price can be written in it.
fn current_spec(
    pc: &PriceBookEntryContext,
    scales: &CurrencyScales,
) -> Result<bss_money::CurrencySpec, DoorError> {
    pc.current(scales).ok_or_else(|| {
        support::conflict_because(
            "BOOK_CURRENCY_INVALID",
            format!(
                "the currency table no longer lists the book's currency {}, so the book takes no \
                 new price",
                pc.currency
            ),
        )
        .into()
    })
}
/// The approved prices of an entry: the chain a draft is judged against at the door.
fn approved_of(prices: &[Price]) -> Vec<Price> {
    prices
        .iter()
        .filter(|r| r.state == PriceState::Approved)
        .cloned()
        .collect()
}
/// A stored row of the entry as the doors answer it on `today`, read as every read of a stored
/// price reads it ([`PriceBookEntryContext::decode`]).
fn shown(
    pc: &PriceBookEntryContext,
    m: entity::price::Model,
    today: time::Date,
) -> Result<PricingPriceDto, RepoError> {
    let price = pc.decode(&m)?;
    PricingPriceDto::at(m, &price, today)
}
fn price_json(r: &Price) -> Result<serde_json::Value, DoorError> {
    Ok(support::value(&r.price)?)
}
fn next_version(prices: &[entity::price::Model]) -> Result<i32, DoorError> {
    prices
        .iter()
        .map(|r| r.version_no)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| support::conflict("VERSION_EXHAUSTED").into())
}
fn stored(
    tenant: Uuid,
    r: &Price,
    note: Option<String>,
    author: Uuid,
    now: OffsetDateTime,
) -> Result<entity::price::Model, DoorError> {
    let (currency, currency_scale) = price_repo::currency_columns(&r.currency);
    Ok(entity::price::Model {
        id: r.id,
        tenant_id: tenant,
        price_book_entry_id: r.price_book_entry_id,
        version_no: r.version_no,
        dim_value: r.dim_value.clone(),
        price_json: price_json(r)?,
        currency,
        currency_scale,
        min_fee: r.min_fee.map(bss_money::canonical_decimal),
        eligibility: r.eligibility.as_str().into(),
        effective_from: r.effective_from,
        effective_to: r.effective_to,
        keep_for_bound: false,
        closed_explicitly: r.closed_explicitly,
        temporary_until: r.temporary_until,
        paired_price_id: None,
        return_of_price_id: r.return_of_price_id,
        state: PriceState::Draft.as_str().into(),
        change_kind: price::ChangeKind::Set.as_str().into(),
        target_price_id: None,
        cancelled_by_unit_id: None,
        pending_unit_id: None,
        approved_by_unit_id: None,
        note,
        created_by: author,
        approved_at: None,
        version: 1,
        created_at: now,
        updated_at: now,
    })
}
async fn live_entry(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<price_book_entry::Model, DoorError> {
    let entry = price_book_entry_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(support::missing_entry)?;
    if entry.reference_state == crate::domain::price_book_entry::ReferenceState::Lost.as_str() {
        return Err(support::conflict("ENTRY_REFERENCE_LOST").into());
    }
    // D-522: an archived book's entries take no new money and no change.
    support::writable_entry(tx, &entry).await?;
    Ok(entry)
}

/// What `POST /prices/{id}/cancel` and `POST /prices/{id}/end` ask (D-520, D-521). The wire has no
/// kind: the path names it, and only an end's body carries a date. So a request is a cancel or an
/// end of its price, never a `set`, and only an end has a new end.
#[derive(Clone, Copy, Debug)]
pub struct ChangeRequest {
    /// The cancel or end of the approved price it names; its id is nil until the row is written.
    pub change: Change,
    /// The day the guards judge on: the door's clock.
    pub today: time::Date,
}

/// `POST /prices/{id}/cancel` and `POST /prices/{id}/end` (D-520, D-521): a draft `cancel` or
/// `end` row that names the approved price, under the create's bounded retry (the entry's next
/// `version_no`). The author submits it as any draft price, alone or with the book's other
/// drafts; the guards run here, at submit and again at apply.
/// # Errors
/// Returns the canonical refusal of the last attempt.
pub async fn open_change(
    db: &toolkit_db::Db,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    request: ChangeRequest,
    key: String,
    digest: Vec<u8>,
) -> Result<Response, CanonicalError> {
    let mut attempt = 1;
    loop {
        let (scope, ctx, key, digest) = (scope.clone(), ctx.clone(), key.clone(), digest.clone());
        let result = support::transaction_door(db, move |tx| {
            let (scope, ctx, key, digest) =
                (scope.clone(), ctx.clone(), key.clone(), digest.clone());
            Box::pin(async move {
                open_change_in(tx, &scope, &ctx, correlation, request, &key, &digest).await
            })
        })
        .await;
        match result {
            Err(DoorError::Repo(RepoError::Conflict {
                code: repo::PRICE_VERSION_TAKEN,
            })) if attempt < VERSION_ATTEMPTS => attempt += 1,
            other => return other.map_err(Into::into),
        }
    }
}

async fn open_change_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    request: ChangeRequest,
    key: &str,
    digest: &[u8],
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let change = request.change;
    let kind = change.kind().as_str();
    let endpoint = format!("/bss-pricing/v1/prices/{}/{kind}", change.target());
    if let Some(replay) = support::claim(tx, tenant, &endpoint, key, digest).await? {
        return Ok(replay);
    }
    let target = price_repo::find(tx, scope, tenant, change.target())
        .await?
        .ok_or_else(|| support::missing_what("price"))?;
    let pc = PriceBookEntryContext::load(
        tx,
        tenant,
        &live_entry(tx, scope, tenant, target.price_book_entry_id).await?,
    )
    .await?;
    let chain = approved_of(&pc.domain_prices()?);
    // D-520: only a consumer's binding refuses a cancel, never the `keep_for_bound` mark alone.
    let bound = matches!(change, Change::Cancel { .. })
        && acceptance_repo::binds_price(tx, &AccessScope::for_tenant(tenant), tenant, target.id)
            .await?;
    crate::infra::prices::guard_change(
        &change,
        &chain,
        &pc.prices,
        request.today,
        Stage::Judge,
        bound,
    )
    .map_err(support::approval_failure)?;
    let now = crate::infra::storage::stored_now();
    // The row names the price and carries its money, its currency spec and its chain unchanged:
    // it is not a price of its own, and no chain, count or resolve reads it as one.
    let row = entity::price::Model {
        id: Uuid::now_v7(),
        tenant_id: tenant,
        price_book_entry_id: target.price_book_entry_id,
        version_no: next_version(&pc.prices)?,
        dim_value: target.dim_value.clone(),
        price_json: target.price_json.clone(),
        currency: target.currency.clone(),
        currency_scale: target.currency_scale,
        min_fee: target.min_fee.clone(),
        eligibility: target.eligibility.clone(),
        effective_from: target.effective_from,
        effective_to: change.end(),
        keep_for_bound: false,
        closed_explicitly: false,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        state: PriceState::Draft.as_str().into(),
        change_kind: kind.into(),
        target_price_id: Some(target.id),
        cancelled_by_unit_id: None,
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: ctx.subject_id(),
        approved_at: None,
        version: 1,
        created_at: now,
        updated_at: now,
    };
    let stored = price_repo::insert(tx, &AccessScope::for_tenant(tenant), row).await?;
    support::audit(
        tx,
        ctx,
        correlation,
        &format!("price.{kind}"),
        stored.id,
        stored.version,
    )
    .await?;
    let body = shown(&pc, stored, request.today)?;
    support::answer(
        tx,
        tenant,
        &endpoint,
        key,
        StatusCode::CREATED,
        &body,
        Some(1),
    )
    .await
}

/// Create under a bounded retry: the `(price_book_entry_id, version_no)` unique index arbitrates
/// two writers that read the same maximum; the loser recomputes in a fresh transaction. The new
/// prices take the book currency's scale in `scales`.
/// # Errors
/// Returns the canonical refusal of the last attempt.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity, replay operands, input and the currency table belong to \
              one transaction"
)]
pub async fn create(
    db: &toolkit_db::Db,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    price_book_entry_id: Uuid,
    key: String,
    digest: Vec<u8>,
    input: PricingPriceCreate,
    scales: Arc<CurrencyScales>,
    clock: Arc<dyn Clock>,
) -> Result<Response, CanonicalError> {
    let mut attempt = 1;
    loop {
        let (scope, ctx, key, digest, input, scales, clock) = (
            scope.clone(),
            ctx.clone(),
            key.clone(),
            digest.clone(),
            input.clone(),
            scales.clone(),
            clock.clone(),
        );
        let result = support::transaction_door(db, move |tx| {
            let (scope, ctx, key, digest, input, scales, clock) = (
                scope.clone(),
                ctx.clone(),
                key.clone(),
                digest.clone(),
                input.clone(),
                scales.clone(),
                clock.clone(),
            );
            Box::pin(async move {
                create_in(
                    tx,
                    &scope,
                    &ctx,
                    correlation,
                    price_book_entry_id,
                    &key,
                    &digest,
                    input,
                    &scales,
                    &*clock,
                )
                .await
            })
        })
        .await;
        match result {
            Err(DoorError::Repo(RepoError::Conflict {
                code: repo::PRICE_VERSION_TAKEN,
            })) if attempt < VERSION_ATTEMPTS => attempt += 1,
            other => return other.map_err(Into::into),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity, replay operands, input and the currency table belong to \
              one transaction"
)]
async fn create_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    price_book_entry_id: Uuid,
    key: &str,
    digest: &[u8],
    input: PricingPriceCreate,
    scales: &CurrencyScales,
    clock: &dyn Clock,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = format!("/bss-pricing/v1/price-book-entries/{price_book_entry_id}/prices");
    if let Some(replay) = support::claim(tx, tenant, &endpoint, key, digest).await? {
        return Ok(replay);
    }
    let pc = PriceBookEntryContext::load(
        tx,
        tenant,
        &live_entry(tx, scope, tenant, price_book_entry_id).await?,
    )
    .await?;
    // The state's clock (the wall clock, or a test's): the date the window is judged on.
    let now = crate::infra::storage::stored_instant(clock.now());
    // D-427: the money is judged against the entry's model; a price carries none of its own.
    let model = pc.model;
    // A new price, and the return a temporary one makes (a copy of it), take today's scale.
    let currency = current_spec(&pc, scales)?;
    let promo = Price {
        id: Uuid::now_v7(),
        price_book_entry_id,
        version_no: next_version(&pc.prices)?,
        dim_value: input.dim_value,
        model,
        currency,
        price: Some(money::decode(model, input.price).map_err(refuse_price)?),
        min_fee: parse_min_fee(input.min_fee.as_ref())?,
        eligibility: parse_eligibility(&input.eligibility)?,
        effective_from: price::parse_start(&input.effective_from).map_err(refuse)?,
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state: PriceState::Draft,
    };
    let siblings = pc.domain_prices()?;
    let prices = match input.temporary_until.as_deref() {
        Some(until) => {
            let end = price::validate_temporary(promo.effective_from, until).map_err(refuse)?;
            price::temporary(&siblings, promo, end, Uuid::now_v7()).map_err(refuse)?
        }
        None => vec![promo],
    };
    for r in &prices {
        if let Some(error) = pc.first_refusal(r, &siblings, now.date()) {
            return Err(refuse_half(error, r));
        }
    }
    // D-406: no price starts inside an approved temporary window, and no temporary spans an
    // approved start; the draft's own pair is judged with it.
    let mut around = approved_of(&siblings);
    around.extend(prices.iter().cloned());
    for r in &prices {
        if let Some(error) = price::window_crossing(r, &around) {
            return Err(refuse(error));
        }
    }
    let children = AccessScope::for_tenant(tenant);
    let mut items = Vec::with_capacity(prices.len());
    for r in &prices {
        let mut m = stored(tenant, r, input.note.clone(), ctx.subject_id(), now)?;
        // The pair's first half cannot name a partner that does not exist yet.
        if items.len() == 1 {
            m.paired_price_id = r.paired_price_id;
        }
        let m = price_repo::insert(tx, &children, m).await?;
        support::audit(tx, ctx, correlation, "price.create", m.id, m.version).await?;
        items.push(m);
    }
    if let [first, second] = items.as_mut_slice() {
        price_repo::link_pair(tx, &children, tenant, first.id, second.id).await?;
        first.paired_price_id = Some(second.id);
    }
    let body = PricingPriceCreated {
        items: items
            .into_iter()
            .map(|m| shown(&pc, m, now.date()))
            .collect::<Result<_, _>>()?,
    };
    support::answer(
        tx,
        tenant,
        &endpoint,
        key,
        StatusCode::CREATED,
        &body,
        Some(1),
    )
    .await
}

/// Change business fields of an unlocked draft at the version the caller read, under the
/// create's bounded retry: a return the new dates call for takes the entry's next `version_no`,
/// which a concurrent writer may take first. The edited draft, and its return, take the book
/// currency's scale in `scales` today.
/// # Errors
/// Returns the canonical refusal of the last attempt.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity, version, input and the currency table belong to one \
              transaction"
)]
pub async fn patch(
    db: &toolkit_db::Db,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    input: PricingPricePatch,
    scales: Arc<CurrencyScales>,
    clock: Arc<dyn Clock>,
) -> Result<Response, CanonicalError> {
    let mut attempt = 1;
    loop {
        let (scope, ctx, input, scales, clock) = (
            scope.clone(),
            ctx.clone(),
            input.clone(),
            scales.clone(),
            clock.clone(),
        );
        let result = support::transaction_door(db, move |tx| {
            let (scope, ctx, input, scales, clock) = (
                scope.clone(),
                ctx.clone(),
                input.clone(),
                scales.clone(),
                clock.clone(),
            );
            Box::pin(async move {
                patch_in(
                    tx,
                    &scope,
                    &ctx,
                    correlation,
                    id,
                    version,
                    input,
                    &scales,
                    &*clock,
                )
                .await
            })
        })
        .await;
        match result {
            Err(DoorError::Repo(RepoError::Conflict {
                code: repo::PRICE_VERSION_TAKEN,
            })) if attempt < VERSION_ATTEMPTS => attempt += 1,
            other => return other.map_err(Into::into),
        }
    }
}
/// A price the author may still change: a draft no unit locks.
fn unlocked_draft(m: &entity::price::Model) -> bool {
    m.state == PriceState::Draft.as_str() && m.pending_unit_id.is_none()
}
/// One draft's PATCH. A temporary pair keeps its chain; its return keeps its own dates (its start
/// is its pair's end); a price's temporariness is fixed. The temporary half's dates move: a PATCH
/// that sends `effective_from` or `temporary_until` to it re-derives the pair ([`redate`], D-443).
/// Every PATCH, whatever it changes, gives the draft the currency's scale of today, so a draft
/// the table changed under is edited and submitted again (`CURRENCY_SCALE_CHANGED`).
/// # Errors
/// Returns `PRICE_NOT_DRAFT`, `NOT_DRAFT_AUTHOR`, `STALE_REVISION`, `TEMPORARY_PRICE_FIXED`,
/// `BOOK_CURRENCY_INVALID` or a pure-rule refusal.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity, version, input and the currency table belong to one \
              transaction"
)]
async fn patch_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    input: PricingPricePatch,
    scales: &CurrencyScales,
    clock: &dyn Clock,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let m = price_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("price"))?;
    // D-520, D-521: a cancel or an end is not an editable price; its author deletes it and opens
    // another.
    if !unlocked_draft(&m) || !price_repo::is_price(&m) {
        return Err(support::conflict("PRICE_NOT_DRAFT").into());
    }
    own_draft(&m, ctx)?;
    support::check_version(version, m.version)?;
    let children = AccessScope::for_tenant(tenant);
    let pc = PriceBookEntryContext::load(
        tx,
        tenant,
        &live_entry(tx, &children, tenant, m.price_book_entry_id).await?,
    )
    .await?;
    let mut r = pc.decode(&m)?;
    r.currency = current_spec(&pc, scales)?;
    // The temporary half carries the end; its return names its partner and the price it restores.
    let promo = m.temporary_until.is_some();
    let temporary = promo || m.paired_price_id.is_some() || m.return_of_price_id.is_some();
    let fixed =
        |field: &str| -> DoorError { support::invalid(field, "TEMPORARY_PRICE_FIXED").into() };
    if let Some(value) = input.dim_value {
        if temporary && value != r.dim_value {
            return Err(fixed("dim_value"));
        }
        r.dim_value = value;
    }
    // Only the temporary half takes an end, and never `null`: no price becomes or stops being
    // temporary by a PATCH (D-443).
    let until = match input.temporary_until {
        None => None,
        Some(Some(text)) if promo => Some(text),
        Some(_) => return Err(fixed("temporary_until")),
    };
    if let Some(start) = input.effective_from.as_deref() {
        let start = price::parse_start(start).map_err(refuse)?;
        if temporary && !promo && start != r.effective_from {
            return Err(fixed("effective_from"));
        }
        r.effective_from = start;
    }
    if let Some(data) = input.price {
        r.price = Some(money::decode(r.model, data).map_err(refuse_price)?);
    }
    if let Some(fee) = input.min_fee {
        r.min_fee = parse_min_fee(fee.as_ref())?;
    }
    if let Some(eligibility) = input.eligibility.as_deref() {
        r.eligibility = parse_eligibility(eligibility)?;
    }
    // The state's clock (the wall clock, or a test's): the date the window is judged on.
    let now = crate::infra::storage::stored_instant(clock.now());
    let siblings = pc.domain_prices()?;
    let mut next = m.clone();
    if let Some(note) = input.note {
        next.note = note;
    }
    next.updated_at = now;
    if promo && (input.effective_from.is_some() || until.is_some()) {
        let dates = Redate {
            pc: &pc,
            siblings: &siblings,
            until: until.as_deref(),
            now,
        };
        return redate(tx, ctx, correlation, dates, r, next, version).await;
    }
    if let Some(error) = pc.first_refusal(&r, &siblings, now.date()) {
        return Err(refuse(error));
    }
    // D-406: a moved start may not land inside an approved temporary window. A temporary
    // price keeps its window here; submit judges it against the chain.
    if price::temporary_holding(&r, &approved_of(&siblings)).is_some() {
        return Err(refuse(RuleError::new("PRICE_INSIDE_TEMPORARY")));
    }
    shaped(&mut next, &r)?;
    price_repo::update_draft(tx, &children, next.clone()).await?;
    next.version += 1;
    support::audit(tx, ctx, correlation, "price.patch", id, next.version).await?;
    Ok(support::response(
        StatusCode::OK,
        &shown(&pc, next, now.date())?,
        Some(version + 1),
    )?)
}
/// Copy a price's business fields and pair columns onto its stored row.
fn shaped(row: &mut entity::price::Model, r: &Price) -> Result<(), DoorError> {
    row.dim_value.clone_from(&r.dim_value);
    row.price_json = price_json(r)?;
    (row.currency, row.currency_scale) = price_repo::currency_columns(&r.currency);
    row.min_fee = r.min_fee.map(bss_money::canonical_decimal);
    row.eligibility = r.eligibility.as_str().into();
    row.effective_from = r.effective_from;
    row.effective_to = r.effective_to;
    row.closed_explicitly = r.closed_explicitly;
    row.temporary_until = r.temporary_until;
    row.paired_price_id = r.paired_price_id;
    row.return_of_price_id = r.return_of_price_id;
    Ok(())
}
/// What a temporary draft's new dates are judged against: its entry, the entry's prices, the end
/// the PATCH sent (else the stored one) and the request's instant.
struct Redate<'a> {
    pc: &'a PriceBookEntryContext,
    siblings: &'a [Price],
    until: Option<&'a str>,
    now: OffsetDateTime,
}
/// D-443: re-run the pair builder over a temporary draft's new dates and reconcile the shape it
/// makes with the stored one in the request's transaction. The builder makes a pair (the return
/// restores the chain's price in force on the end), the promo alone (the chain's next approved
/// price starts exactly on the end) or one explicitly closed price (nothing of the chain is in
/// force there). Both halves are judged as the create judges them. The writes, in order:
/// - pair → pair: the promo at its version, then its return in place at the return's version
///   (the same id, number and author; start = the new end, money copied again from the restored
///   price — an edited return would be stale at submit anyway, D-391);
/// - pair → one price: the promo first, its link cleared, then the return is deleted
///   (`price.delete`);
/// - one price → pair: the return first, naming the promo and numbered after every price of the
///   entry (the unique `(entry, version_no)` index; never `promo.version_no + 1`), then the
///   promo, naming it (`price.create`, then `price.patch`);
/// - one price → one price: the promo.
///
/// The answer is the promo, its `paired_price_id` naming its partner now, or `null`.
async fn redate(
    tx: &impl DBRunner,
    ctx: &SecurityContext,
    correlation: Uuid,
    dates: Redate<'_>,
    promo: Price,
    mut next: entity::price::Model,
    version: u64,
) -> Result<Response, DoorError> {
    let end = match dates.until {
        Some(text) => {
            price::parse_start(text).map_err(|_| refuse(RuleError::new("WINDOW_END_INVALID")))?
        }
        None => next.temporary_until.ok_or_else(|| {
            RepoError::CorruptRow(format!("temporary price {} has no end", next.id))
        })?,
    };
    let partner = partner_of(tx, ctx, &next).await?;
    let shape = judged_shape(&dates, promo, end, partner.as_ref())?;
    // `price::temporary` answers the promo, alone or with its return (PS-35).
    let (first, returned) = match shape.as_slice() {
        [promo] => (promo, None),
        [promo, returned] => (promo, Some(returned)),
        _ => {
            return Err(CanonicalError::internal(format!(
                "a temporary price split into {} prices",
                shape.len()
            ))
            .create()
            .into());
        }
    };
    shaped(&mut next, first)?;
    let written = Written {
        ctx,
        correlation,
        now: dates.now,
    };
    next.version = reconcile(tx, &written, &next, partner, returned).await?;
    Ok(support::response(
        StatusCode::OK,
        &shown(dates.pc, next, dates.now.date())?,
        Some(version + 1),
    )?)
}
/// The stored partner of a temporary draft: the pair moves whole, so its return must be an
/// unlocked draft of the same author (409 `PRICE_NOT_DRAFT`, 403 `NOT_DRAFT_AUTHOR`).
async fn partner_of(
    tx: &impl DBRunner,
    ctx: &SecurityContext,
    promo: &entity::price::Model,
) -> Result<Option<entity::price::Model>, DoorError> {
    let Some(id) = promo.paired_price_id else {
        return Ok(None);
    };
    let tenant = ctx.subject_tenant_id();
    let p = price_repo::find(tx, &AccessScope::for_tenant(tenant), tenant, id)
        .await?
        .ok_or_else(|| {
            RepoError::CorruptRow(format!("price {} lost its partner {id}", promo.id))
        })?;
    if !unlocked_draft(&p) {
        return Err(support::conflict("PRICE_NOT_DRAFT").into());
    }
    own_draft(&p, ctx)?;
    Ok(Some(p))
}
/// The shape the builder makes of the promo on its new dates, its return (if any) numbered — a
/// kept return keeps its own number, a new one takes the entry's next — and both halves judged as
/// the create judges them.
fn judged_shape(
    dates: &Redate<'_>,
    promo: Price,
    end: time::Date,
    partner: Option<&entity::price::Model>,
) -> Result<Vec<Price>, DoorError> {
    let return_id = partner.map_or_else(Uuid::now_v7, |p| p.id);
    let mut shape = price::temporary(dates.siblings, promo, end, return_id).map_err(refuse)?;
    if let Some(returned) = shape.get_mut(1) {
        returned.version_no = match partner {
            Some(p) => p.version_no,
            None => next_version(&dates.pc.prices)?,
        };
    }
    for r in &shape {
        if let Some(error) = dates.pc.first_refusal(r, dates.siblings, dates.now.date()) {
            return Err(refuse_half(error, r));
        }
    }
    // D-406, as the create judges it: against the approved prices and the pair itself.
    let mut around = approved_of(dates.siblings);
    around.extend(shape.iter().cloned());
    for r in &shape {
        if let Some(error) = price::window_crossing(r, &around) {
            return Err(refuse(error));
        }
    }
    Ok(shape)
}
/// Who writes, under which correlation, at which instant.
struct Written<'a> {
    ctx: &'a SecurityContext,
    correlation: Uuid,
    now: OffsetDateTime,
}
/// Write the promo's new shape and reconcile its partner, in [`redate`]'s order, each write with
/// its audit row. Returns the promo's new version.
async fn reconcile(
    tx: &impl DBRunner,
    w: &Written<'_>,
    next: &entity::price::Model,
    partner: Option<entity::price::Model>,
    returned: Option<&Price>,
) -> Result<i64, DoorError> {
    let tenant = w.ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    let promo = (next.id, next.version + 1);
    let audit = |action: &'static str, (id, version): (Uuid, i64)| {
        support::audit(tx, w.ctx, w.correlation, action, id, version)
    };
    match (partner, returned) {
        (Some(p), Some(returned)) => {
            price_repo::update_draft(tx, &children, next.clone()).await?;
            audit("price.patch", promo).await?;
            let mut kept = p.clone();
            shaped(&mut kept, returned)?;
            kept.updated_at = w.now;
            price_repo::update_draft(tx, &children, kept).await?;
            audit("price.patch", (p.id, p.version + 1)).await?;
        }
        (Some(p), None) => {
            price_repo::update_draft(tx, &children, next.clone()).await?;
            audit("price.patch", promo).await?;
            price_repo::delete_drafts(tx, &children, tenant, &[(p.id, p.version)]).await?;
            audit("price.delete", (p.id, p.version)).await?;
        }
        (None, Some(returned)) => {
            let mut m = stored(
                tenant,
                returned,
                next.note.clone(),
                w.ctx.subject_id(),
                w.now,
            )?;
            m.paired_price_id = returned.paired_price_id;
            let m = price_repo::insert(tx, &children, m).await?;
            audit("price.create", (m.id, m.version)).await?;
            price_repo::update_draft(tx, &children, next.clone()).await?;
            audit("price.patch", promo).await?;
        }
        (None, None) => {
            price_repo::update_draft(tx, &children, next.clone()).await?;
            audit("price.patch", promo).await?;
        }
    }
    Ok(promo.1)
}

/// Delete an unlocked draft at its version; a pair half takes its partner with it.
/// # Errors
/// Returns `PRICE_NOT_DRAFT`, `NOT_DRAFT_AUTHOR` or `STALE_REVISION`.
pub async fn delete(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let m = price_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("price"))?;
    if !unlocked_draft(&m) {
        return Err(support::conflict("PRICE_NOT_DRAFT").into());
    }
    own_draft(&m, ctx)?;
    support::check_version(version, m.version)?;
    let children = AccessScope::for_tenant(tenant);
    let mut targets = vec![(m.id, m.version)];
    if let Some(partner) = m.paired_price_id
        && let Some(p) = price_repo::find(tx, &children, tenant, partner).await?
    {
        if !unlocked_draft(&p) {
            return Err(support::conflict("PRICE_NOT_DRAFT").into());
        }
        own_draft(&p, ctx)?;
        targets.push((p.id, p.version));
    }
    price_repo::delete_drafts(tx, &children, tenant, &targets).await?;
    for (price, version) in targets {
        support::audit(tx, ctx, correlation, "price.delete", price, version).await?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
#[path = "prices_tests.rs"]
mod tests;

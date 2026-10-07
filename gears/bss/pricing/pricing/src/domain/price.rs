//! Price validation, per-value chains, temporary pairs and dated metering continuity.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-dimension-fallback:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-temporary-value-fallback:p1
use super::{
    RuleError,
    money::{self, PriceData},
    price_book_entry::{ChargeKind, Model, model_allowed},
};
use rust_decimal::Decimal;
use time::Date;
use uuid::Uuid;

string_enum!(Eligibility {All=>"all", New=>"new"});
string_enum!(PriceState {Draft=>"draft", Pending=>"pending", Approved=>"approved", Rejected=>"rejected", Cancelled=>"cancelled"});
// Matrix row 10's display state: a draft, pending or rejected price shows its state; an approved
// one shows where its window stands today; a cancelled price shows `cancelled` (D-520).
string_enum!(DisplayStatus {Draft=>"draft", Pending=>"pending", Rejected=>"rejected", Scheduled=>"scheduled", Active=>"active", Superseded=>"superseded", Cancelled=>"cancelled"});
// What a price row asks the `prices` unit to do (D-520, D-521). `set` is today's price.
// `cancel` and `end` name another price and carry no money of their own.
string_enum!(ChangeKind {Set=>"set", Cancel=>"cancel", End=>"end"});

#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the spec's nouns: a Price names its PriceBookEntry and carries its money as `price`"
)]
pub struct Price {
    pub id: Uuid,
    pub price_book_entry_id: Uuid,
    pub version_no: i32,
    pub dim_value: Option<String>,
    pub model: Model,
    pub price: Option<PriceData>,
    pub min_fee: Option<Decimal>,
    pub eligibility: Eligibility,
    pub effective_from: Date,
    pub effective_to: Option<Date>,
    pub temporary_until: Option<Date>,
    pub paired_price_id: Option<Uuid>,
    pub return_of_price_id: Option<Uuid>,
    pub closed_explicitly: bool,
    pub state: PriceState,
}
/// Parse a start before constructing a typed price.
/// # Errors
/// Returns `WINDOW_START_INVALID` for absent or invalid calendar dates.
pub fn parse_start(text: &str) -> Result<Date, RuleError> {
    Date::parse(text, &time::format_description::well_known::Iso8601::DATE)
        .map_err(|_| RuleError::new("WINDOW_START_INVALID"))
}
/// Validate all independent authoring rules. Currency scale is supplied by the book context.
#[must_use]
pub fn validate(
    price: &Price,
    kind: ChargeKind,
    values: Option<&[String]>,
    siblings: &[Price],
    today: Date,
    minor_digits: u32,
) -> Vec<RuleError> {
    let mut errors = Vec::new();
    if !model_allowed(kind, price.model) {
        errors.push(RuleError::new("MODEL_KIND_CHARGEKIND_MISMATCH"));
    }
    if let Some(data) = &price.price {
        errors.extend(money::validate(price.model, data));
    } else {
        errors.push(RuleError::new("PRICE_MISSING"));
    }
    if price.state != PriceState::Approved && price.effective_from < today {
        errors.push(RuleError::new("WINDOW_START_IN_PAST"));
    }
    if siblings.iter().any(|other| {
        other.id != price.id
            && other.price_book_entry_id == price.price_book_entry_id
            && other.dim_value == price.dim_value
            // A cancelled price has left the chain (D-520).
            && other.state == PriceState::Approved
            && other.effective_from == price.effective_from
    }) {
        errors.push(RuleError::new("WINDOW_OVERLAP"));
    }
    if let Some(value) = &price.dim_value {
        match values {
            None => errors.push(RuleError::new("DIM_NOT_DECLARED")),
            Some(allowed) if !allowed.contains(value) => {
                errors.push(RuleError::new("DIM_VALUE_UNKNOWN"));
            }
            Some(_) => {}
        }
    }
    if price
        .min_fee
        .is_some_and(|fee| fee < Decimal::ZERO || fee.scale() > minor_digits)
    {
        errors.push(RuleError::new("MIN_FEE_INVALID"));
    }
    errors
}
/// Display state combines stored approval and the window at the requested date.
#[must_use]
pub fn status(price: &Price, today: Date) -> &'static str {
    window_status(price.state, price.effective_from, price.effective_to, today)
}
/// The same display state from the stored columns alone.
#[must_use]
pub fn window_status(state: PriceState, from: Date, to: Option<Date>, today: Date) -> &'static str {
    window_display(state, from, to, today).as_str()
}
/// [`window_status`] as its closed set (D-439).
#[must_use]
pub fn window_display(
    state: PriceState,
    from: Date,
    to: Option<Date>,
    today: Date,
) -> DisplayStatus {
    match state {
        PriceState::Draft => DisplayStatus::Draft,
        PriceState::Pending => DisplayStatus::Pending,
        PriceState::Rejected => DisplayStatus::Rejected,
        PriceState::Cancelled => DisplayStatus::Cancelled,
        PriceState::Approved if to.is_some_and(|end| end <= today) => DisplayStatus::Superseded,
        PriceState::Approved if from > today => DisplayStatus::Scheduled,
        PriceState::Approved => DisplayStatus::Active,
    }
}
/// A `cancel` or `end` row's display state (D-520, D-521). It has no window of its own: a draft,
/// pending or rejected change shows its state, and an applied one shows `superseded`, so no
/// window narrowing lists it as a price in force.
#[must_use]
pub fn change_display(state: PriceState) -> DisplayStatus {
    match state {
        PriceState::Draft => DisplayStatus::Draft,
        PriceState::Pending => DisplayStatus::Pending,
        PriceState::Rejected => DisplayStatus::Rejected,
        PriceState::Cancelled => DisplayStatus::Cancelled,
        PriceState::Approved => DisplayStatus::Superseded,
    }
}
/// Approved prices of exactly one chain, ordered by start and version.
#[must_use]
pub fn approved_prices<'a>(
    prices: &'a [Price],
    price_book_entry_id: Uuid,
    dim: Option<&str>,
) -> Vec<&'a Price> {
    let mut chain: Vec<_> = prices
        .iter()
        .filter(|r| {
            r.price_book_entry_id == price_book_entry_id
                && r.dim_value.as_deref() == dim
                && r.state == PriceState::Approved
        })
        .collect();
    chain.sort_by_key(|r| (r.effective_from, r.version_no));
    chain
}
/// Recompute implicit ends; an explicit end is kept unless a successor starts inside it.
pub fn normalize_windows(prices: &mut [Price]) {
    // A cancelled price is not approved, so it leaves the chain and its predecessor
    // re-opens onto the next surviving start (D-520).
    let mut order: Vec<usize> = (0..prices.len())
        .filter(|i| prices[*i].state == PriceState::Approved)
        .collect();
    order.sort_by_key(|i| {
        (
            prices[*i].price_book_entry_id,
            prices[*i].dim_value.clone(),
            prices[*i].effective_from,
            prices[*i].version_no,
        )
    });
    for (pos, index) in order.iter().copied().enumerate() {
        let next_start = order.get(pos + 1).and_then(|next| {
            let r = &prices[*next];
            (r.price_book_entry_id == prices[index].price_book_entry_id
                && r.dim_value == prices[index].dim_value)
                .then_some(r.effective_from)
        });
        prices[index].effective_to = if prices[index].closed_explicitly {
            // An explicit end survives every normalisation, but a successor that
            // starts inside it still closes it: one price in force per chain and date.
            match (prices[index].effective_to, next_start) {
                (Some(end), Some(next)) => Some(end.min(next)),
                (end, next) => end.or(next),
            }
        } else {
            next_start
        };
    }
}
/// Close an approved price at `end` (D-521).
///
/// The current end is the next approved start of the same chain, or the shorter of that
/// start and an explicit end (D-390). `end` must be after the price's start and no later
/// than that current end. A price with no current end may close on any later date. The
/// result is explicitly closed at `end`. A successor cannot start inside a valid `end`,
/// because `end` is not after the next start.
///
/// The door still refuses an end that is not after today (`END_DATE_INVALID`) and a price
/// that has already ended (`PRICE_ALREADY_ENDED`). This function is the window rule only.
///
/// # Errors
/// `END_DATE_INVALID` when `target` is not an approved price of `chain`, when `end` is not
/// after the start, or when `end` is after the current end.
pub fn end_price(chain: &[Price], target: Uuid, end: Date) -> Result<Price, RuleError> {
    let Some(price) = chain.iter().find(|row| row.id == target) else {
        return Err(RuleError::new("END_DATE_INVALID"));
    };
    if price.state != PriceState::Approved || end <= price.effective_from {
        return Err(RuleError::new("END_DATE_INVALID"));
    }
    let next_start = chain
        .iter()
        .filter(|row| {
            row.id != price.id
                && row.state == PriceState::Approved
                && row.price_book_entry_id == price.price_book_entry_id
                && row.dim_value == price.dim_value
                && row.effective_from > price.effective_from
        })
        .map(|row| row.effective_from)
        .min();
    let current = if price.closed_explicitly {
        match (price.effective_to, next_start) {
            (Some(explicit), Some(next)) => Some(explicit.min(next)),
            (explicit, next) => explicit.or(next),
        }
    } else {
        next_start
    };
    if current.is_some_and(|until| end > until) {
        return Err(RuleError::new("END_DATE_INVALID"));
    }
    let mut closed = price.clone();
    closed.effective_to = Some(end);
    closed.closed_explicitly = true;
    Ok(closed)
}
/// The price of exactly one chain (no default fallback) in force on a date.
#[must_use]
pub fn own_version_at<'a>(
    prices: &'a [Price],
    price_book_entry_id: Uuid,
    date: Date,
    dim: Option<&str>,
) -> Option<&'a Price> {
    approved_prices(prices, price_book_entry_id, dim)
        .into_iter()
        .rev()
        .find(|r| r.effective_from <= date && r.effective_to.is_none_or(|end| date < end))
}
/// One price of an entry's default chain as the next-price rule reads it (D-472): the columns
/// the choice depends on.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainPrice {
    pub id: Uuid,
    pub state: PriceState,
    pub effective_from: Date,
    pub version_no: i32,
    pub created_at: time::OffsetDateTime,
}
/// The default chain's next price after `day` (D-472), of one entry's default-chain prices, the
/// headline rule beside [`own_version_at`]: its earliest approved price that starts after the day
/// (a scheduled price; two approved prices of a chain never share a start, and the highest
/// `version_no` would win as it wins in force), else its newest draft or pending price — the
/// highest `version_no`, then the latest `created_at`, then the highest id — else `None`. A
/// rejected price is never one.
#[must_use]
pub fn next_of(chain: &[ChainPrice], day: Date) -> Option<&ChainPrice> {
    chain
        .iter()
        .filter(|p| p.state == PriceState::Approved && p.effective_from > day)
        .min_by(|a, b| {
            a.effective_from
                .cmp(&b.effective_from)
                .then(b.version_no.cmp(&a.version_no))
        })
        .or_else(|| {
            chain
                .iter()
                .filter(|p| matches!(p.state, PriceState::Draft | PriceState::Pending))
                .max_by_key(|p| (p.version_no, p.created_at, p.id))
        })
}
/// Prefer the value's in-force price, then the default chain.
#[must_use]
pub fn version_at<'a>(
    prices: &'a [Price],
    price_book_entry_id: Uuid,
    date: Date,
    dim: Option<&str>,
) -> Option<&'a Price> {
    own_version_at(prices, price_book_entry_id, date, dim)
        .or_else(|| dim.and_then(|_| own_version_at(prices, price_book_entry_id, date, None)))
}
/// The last approved price of the exact chain, only if unbounded.
#[must_use]
pub fn open_tail<'a>(
    prices: &'a [Price],
    price_book_entry_id: Uuid,
    dim: Option<&str>,
) -> Option<&'a Price> {
    approved_prices(prices, price_book_entry_id, dim)
        .last()
        .copied()
        .filter(|r| r.effective_to.is_none())
}
#[toolkit_macros::domain_model]
#[derive(Debug)]
pub struct Coverage<'a> {
    pub version: Option<&'a Price>,
    pub missing: Vec<String>,
    pub closing: Vec<String>,
}
/// Check present coverage and an open tail for every declared value.
#[must_use]
pub fn coverage_on<'a>(
    prices: &'a [Price],
    price_book_entry_id: Uuid,
    date: Date,
    values: &[String],
) -> Coverage<'a> {
    let mut result = Coverage {
        version: version_at(prices, price_book_entry_id, date, None),
        missing: Vec::new(),
        closing: Vec::new(),
    };
    let dims: Vec<Option<&str>> = if values.is_empty() {
        vec![None]
    } else {
        values.iter().map(|s| Some(s.as_str())).collect()
    };
    for dim in dims {
        if let Some(price) = version_at(prices, price_book_entry_id, date, dim) {
            result.version = result.version.or(Some(price));
            if open_tail(prices, price_book_entry_id, dim).is_none()
                && open_tail(prices, price_book_entry_id, None).is_none()
            {
                result.closing.push(dim.unwrap_or_default().to_owned());
            }
        } else {
            result.missing.push(dim.unwrap_or_default().to_owned());
        }
    }
    result
}
/// Validate a temporary end before constructing a pair.
/// # Errors
/// Returns `WINDOW_END_INVALID` for invalid dates or a nonpositive duration.
pub fn validate_temporary(start: Date, until: &str) -> Result<Date, RuleError> {
    let end = parse_start(until).map_err(|_| RuleError::new("WINDOW_END_INVALID"))?;
    if end <= start {
        Err(RuleError::new("WINDOW_END_INVALID"))
    } else {
        Ok(end)
    }
}
/// Build a pair on an owned chain, or a single explicitly closed price on an empty chain, or
/// the promo alone when the chain's next price starts exactly on the end.
/// # Errors
/// Refuses a nonpositive duration or an exhausted version number.
pub fn temporary(
    prices: &[Price],
    mut promo: Price,
    until: Date,
    return_id: Uuid,
) -> Result<Vec<Price>, RuleError> {
    if until <= promo.effective_from {
        return Err(RuleError::new("WINDOW_END_INVALID"));
    }
    promo.temporary_until = Some(until);
    promo.paired_price_id = None;
    promo.return_of_price_id = None;
    // Return only to the chain's own price in force on the end date. A chain that
    // has ended, or that starts later, is neither revived nor copied backwards:
    // the value then falls back to the default after the temporary price.
    let back = own_version_at(
        prices,
        promo.price_book_entry_id,
        until,
        promo.dim_value.as_deref(),
    );
    if back.is_some_and(|b| b.effective_from == until) {
        // The next approved price starts exactly on the end and already ends the promo: the
        // promo alone, which normalisation closes at that start; nothing to return to.
        promo.effective_to = Some(until);
        promo.closed_explicitly = false;
        return Ok(vec![promo]);
    }
    if let Some(back) = back {
        let mut returned = promo.clone();
        returned.id = return_id;
        returned.version_no = promo
            .version_no
            .checked_add(1)
            .ok_or_else(|| RuleError::new("VERSION_EXHAUSTED"))?;
        returned.effective_from = until;
        // Back to a price that itself ends (a temporary price — a pair nested in an outer pair —
        // or a closed value price): only until that price's own end, as an explicit end the
        // stored chain carries, after which the chain's next price (the outer return), else the
        // default, is in force again (D-425, phase 4 second review M1).
        returned.effective_to = back
            .temporary_until
            .or(back.effective_to.filter(|_| back.closed_explicitly));
        returned.temporary_until = None;
        returned.closed_explicitly = returned.effective_to.is_some();
        // The money only: the model is the entry's (D-427), the one the promo already carries.
        returned.price.clone_from(&back.price);
        returned.min_fee = back.min_fee;
        returned.return_of_price_id = Some(back.id);
        returned.paired_price_id = Some(promo.id);
        promo.paired_price_id = Some(returned.id);
        promo.closed_explicitly = false;
        promo.effective_to = Some(until);
        Ok(vec![promo, returned])
    } else {
        promo.effective_to = Some(until);
        promo.closed_explicitly = true;
        Ok(vec![promo])
    }
}
/// Whether a temporary price still matches its chain as it will stand once the unit applies
/// (D-391). `prices` are the approved prices outside the unit; `unit` is every price of the unit,
/// the temporary price and its partner included. The chain the return must honour is the
/// approved prices PLUS the unit's other prices: a price published in the same unit and in force on
/// the pair's end would otherwise be undone by a return copied from an older price. A pair's
/// return must restore the price in force on the (shifted) end, with that price's money. When
/// that price is itself a price of the unit, the pair is right only if it is another pair's
/// return naming the same restored price with the same money (two pairs on one chain, both
/// drafted against the same approved price). A temporary price without a return is right only
/// while nothing of its own chain is in force on its end, or while the next price starts exactly
/// there and so ends it.
#[must_use]
pub fn temporary_is_current(prices: &[Price], temporary: &Price, unit: &[Price]) -> bool {
    let Some(until) = temporary.temporary_until else {
        return true;
    };
    let mut future: Vec<Price> = prices.to_vec();
    future.extend(
        unit.iter()
            .filter(|r| {
                r.id != temporary.id
                    && Some(r.id) != temporary.paired_price_id
                    && r.price_book_entry_id == temporary.price_book_entry_id
            })
            .cloned()
            .map(|mut r| {
                r.state = PriceState::Approved;
                r
            }),
    );
    normalize_windows(&mut future);
    let back = own_version_at(
        &future,
        temporary.price_book_entry_id,
        until,
        temporary.dim_value.as_deref(),
    );
    match temporary.paired_price_id {
        Some(partner) => {
            let returned = unit.iter().find(|r| r.id == partner);
            match (returned, back) {
                (Some(r), Some(b)) => {
                    let restores = if unit.iter().any(|u| u.id == b.id) {
                        b.return_of_price_id.is_some()
                            && b.return_of_price_id == r.return_of_price_id
                    } else {
                        r.return_of_price_id == Some(b.id)
                    };
                    // Money and min fee; the model is the entry's (D-427), never a price's.
                    restores && r.price == b.price && r.min_fee == b.min_fee
                }
                _ => false,
            }
        }
        None => match back {
            Some(b) => b.effective_from == until,
            None => temporary.closed_explicitly,
        },
    }
}
/// The other prices of `price`'s chain (same entry, same dimension value), itself excluded.
fn chain_of<'a>(price: &'a Price, others: &'a [Price]) -> impl Iterator<Item = &'a Price> + 'a {
    others.iter().filter(move |o| {
        o.id != price.id
            && o.price_book_entry_id == price.price_book_entry_id
            && o.dim_value == price.dim_value
    })
}
/// D-406: the temporary price whose window `[effective_from, temporary_until)` holds the start
/// of `price`, when `price` is neither temporary nor a pair's return. Such a price would be
/// undone at the promo's end (by its return, or by its closed end). A pair's return starts on
/// its own promo's end and belongs to that pair: a nested pair's return is not refused here.
#[must_use]
pub fn temporary_holding<'a>(price: &'a Price, others: &'a [Price]) -> Option<&'a Price> {
    if price.temporary_until.is_some() || price.return_of_price_id.is_some() {
        return None;
    }
    chain_of(price, others).find(|o| {
        o.temporary_until.is_some_and(|until| {
            o.effective_from <= price.effective_from && price.effective_from < until
        })
    })
}
/// D-406: the price of `price`'s chain whose start falls strictly inside `price`'s temporary
/// window `(effective_from, temporary_until)`. Normalisation would cut the promo at that start.
/// A start exactly on `temporary_until` ends the promo and is not a crossing.
#[must_use]
pub fn start_spanned<'a>(price: &'a Price, others: &'a [Price]) -> Option<&'a Price> {
    let until = price.temporary_until?;
    chain_of(price, others)
        .find(|o| price.effective_from < o.effective_from && o.effective_from < until)
}
/// Both D-406 refusals for one price, in rule order: `PRICE_INSIDE_TEMPORARY`, then
/// `TEMPORARY_SPANS_A_CHANGE`. `others` are the approved prices and the other prices of the
/// same unit (or of the same draft).
#[must_use]
pub fn window_crossing(price: &Price, others: &[Price]) -> Option<RuleError> {
    if temporary_holding(price, others).is_some() {
        Some(RuleError::new("PRICE_INSIDE_TEMPORARY"))
    } else if start_spanned(price, others).is_some() {
        Some(RuleError::new("TEMPORARY_SPANS_A_CHANGE"))
    } else {
        None
    }
}
/// Shift a price and its explicit/temporary end by the same duration.
/// Call with the same displacement for the return partner.
/// # Errors
/// Refuses date overflow.
pub fn shift(price: &Price, new_from: Date) -> Result<Price, RuleError> {
    let delta = new_from - price.effective_from;
    let mut shifted = price.clone();
    shifted.effective_from = new_from;
    let move_date = |date: Date| {
        date.checked_add(delta)
            .ok_or_else(|| RuleError::new("WINDOW_END_INVALID"))
    };
    shifted.temporary_until = price.temporary_until.map(move_date).transpose()?;
    shifted.effective_to = price.effective_to.map(move_date).transpose()?;
    Ok(shifted)
}
/// Apply a common effective date to a selection (decision 7): every price starts on it,
/// except a pair's return half, which moves by its promo half's displacement so the
/// pair keeps its length. A return closed at its outer price's end keeps that end: the
/// outer price does not move. `None` moves nothing.
/// # Errors
/// Refuses date overflow.
pub fn shift_selection(prices: &[Price], date: Option<Date>) -> Result<Vec<Price>, RuleError> {
    let Some(date) = date else {
        return Ok(prices.to_vec());
    };
    prices
        .iter()
        .map(|r| {
            let promo = r
                .return_of_price_id
                .and(r.paired_price_id)
                .and_then(|partner| prices.iter().find(|p| p.id == partner));
            match promo {
                Some(promo) => {
                    let delta = date - promo.effective_from;
                    let start = r
                        .effective_from
                        .checked_add(delta)
                        .ok_or_else(|| RuleError::new("WINDOW_START_INVALID"))?;
                    let mut moved = shift(r, start)?;
                    if r.closed_explicitly {
                        moved.effective_to = r.effective_to;
                    }
                    Ok(moved)
                }
                None => shift(r, date),
            }
        })
        .collect()
}
/// The price of the same chain in force on the day before `price` starts, if any.
#[must_use]
pub fn in_force_before<'a>(chain: &'a [Price], price: &Price) -> Option<&'a Price> {
    let eve = price.effective_from.previous_day()?;
    own_version_at(
        chain,
        price.price_book_entry_id,
        eve,
        price.dim_value.as_deref(),
    )
}
/// The input field of a PRICE request a refusal code names, for the wire problem. A price
/// carries no `model` (D-427), so no code names one here: `CHAIN_MODEL_CHANGED` (package size or
/// dated metering) and a model refusal name the money, `price`. The entry create names `model`
/// itself.
#[must_use]
pub fn field_of(code: &str) -> &'static str {
    match code {
        "WINDOW_START_IN_PAST"
        | "WINDOW_START_INVALID"
        | "WINDOW_OVERLAP"
        | "PRICE_INSIDE_TEMPORARY" => "effective_from",
        "WINDOW_END_INVALID" | "PAIR_RETURN_STALE" | "TEMPORARY_SPANS_A_CHANGE" => {
            "temporary_until"
        }
        "END_DATE_INVALID" => "effective_to",
        "DIM_NOT_DECLARED" | "DIM_VALUE_UNKNOWN" => "dim_value",
        "MIN_FEE_INVALID" => "min_fee",
        "ELIGIBILITY_INVALID" => "eligibility",
        "PAIR_SPLIT" | "PRICE_NOT_IN_BOOK" | "PRICE_NOT_DRAFT" => "price_ids",
        _ => "price",
    }
}
/// Draft prices of one book ordered by start, entry id and version number.
#[must_use]
pub fn proposed_prices<'a>(
    book_id: Uuid,
    entry_books: &[(Uuid, Uuid)],
    prices: &'a [Price],
) -> Vec<&'a Price> {
    let mut proposed: Vec<_> = prices
        .iter()
        .filter(|r| {
            r.state == PriceState::Draft && entry_books.contains(&(r.price_book_entry_id, book_id))
        })
        .collect();
    proposed.sort_by_key(|r| (r.effective_from, r.price_book_entry_id, r.version_no));
    proposed
}
/// SKU version metering. Callers fetch each side as of that price's effective start (D-402).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkuMetering {
    pub unit: Option<String>,
    pub usage_type_ref: Option<String>,
}
/// Preserve package size and dated metering on usage chains. The model needs no comparison:
/// every price of a chain has its entry's model (D-427, D-402).
/// # Errors
/// Returns `CHAIN_MODEL_CHANGED` if any guarded attribute changes.
pub fn chain_guard(
    kind: ChargeKind,
    predecessor: &Price,
    before: &SkuMetering,
    successor: &Price,
    after: &SkuMetering,
) -> Result<(), RuleError> {
    if kind != ChargeKind::Usage {
        return Ok(());
    }
    let size = |r: &Price| match &r.price {
        Some(PriceData::Package { package_size, .. }) => Some(*package_size),
        _ => None,
    };
    if before != after
        || (predecessor.model == Model::Package && size(predecessor) != size(successor))
    {
        return Err(RuleError::new("CHAIN_MODEL_CHANGED"));
    }
    Ok(())
}
#[cfg(test)]
#[path = "price_tests.rs"]
mod tests;

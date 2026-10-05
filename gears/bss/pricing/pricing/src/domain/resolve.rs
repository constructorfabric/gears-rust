//! The consumer read contract as a pure function (D-419, D-420, D-421): per item of a plan
//! revision on one date, the chain matrix a subscription binds from, and the invoice inputs a
//! binding carries.
//!
//! Plain data only. The resolve door reads the revision's items, the entry each item names with
//! ALL its prices, today's registered values of each entry's dimension key, the `keep_for_bound`
//! price ids and the tenant settings in one transaction, and each SKU version as of the date
//! outside it; this module decides. Nothing here computes a total or picks a promotion (D-409,
//! D-415): a chain no price covers is `uncovered`, never an invented price and never a refusal.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-resolve-matrix:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-renewal-all-new:p1
use super::{
    RuleError,
    price::{self, Eligibility, Price, PriceState},
    price_book_entry::{ChargeKind, Model},
};
use bss_products_sdk::models::{BillingTiming, SkuVersion};
use std::collections::{BTreeMap, BTreeSet};
use time::Date;
use uuid::Uuid;

/// The most pins one request carries (`PINS_TOO_MANY`); a consumer with more splits its request
/// by item (D-419).
pub const MAX_PINS: usize = 1_000;

// Where a resolved invoice input came from (D-421).
string_enum!(Source {Entry=>"entry", Sku=>"sku", Tenant=>"tenant"});

/// A subscription's current binding for one item and value, sent back on renewal (D-419).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The bound price.
    pub price_id: Uuid,
    /// Set only for a default-chain price that this value was bound to (`price_id:dim_value`);
    /// `None` pins the price's own chain.
    pub dim_value: Option<String>,
}
/// The entry an item names, with every one of its prices.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: Uuid,
    pub charge_kind: ChargeKind,
    pub period: Option<String>,
    /// The entry's model (D-427): every price of the entry is money in it.
    pub model: Model,
    pub invoice_line_override: Option<String>,
    /// The values registered today for the entry's dimension key, in the registry's order;
    /// empty for an entry without a key.
    pub values: Vec<String>,
    /// All the entry's prices, of every state: bindings are chosen among the approved ones and
    /// a pin on any other is refused.
    pub prices: Vec<Price>,
}
/// One item of the revision as stored, with the entry it names: a SKU and its entry (D-467).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: Uuid,
    pub sku_id: Uuid,
    /// `None` for an item stored without an entry (a draft waiting for one, D-512, or a legacy
    /// item, D-467): it has no chains.
    pub entry: Option<Entry>,
}
/// Everything the matrix reads, as plain data.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveContext {
    pub items: Vec<Item>,
    /// The approved prices marked `keep_for_bound` (the domain `Price` has no such field).
    pub keep_for_bound: BTreeSet<Uuid>,
}
/// The price bound for one (item, value) on the date.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub price: Price,
    /// The pin the renewal walk started from; `None` for a signup.
    pub pinned_from: Option<Uuid>,
    pub keep_for_bound: bool,
}
impl Binding {
    /// The chain the bound price belongs to: the value, or `None` for the default chain.
    #[must_use]
    pub fn dim_used(&self) -> Option<&str> {
        self.price.dim_value.as_deref()
    }
    /// D-425: where the binding ends for its holder — the bound price's own end
    /// ([`own_end`]); `None` when nothing but a successor's start closes its stored window.
    #[must_use]
    pub fn ends_on(&self) -> Option<Date> {
        own_end(&self.price)
    }
}
/// One row of an item's matrix: the default chain (`dim_value` `None`) or one value.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub dim_value: Option<String>,
    pub binding: Option<Binding>,
}
impl Chain {
    /// Neither the value's own chain nor the default binds on the date.
    #[must_use]
    pub const fn uncovered(&self) -> bool {
        self.binding.is_none()
    }
}
/// One item of the revision resolved on the date: its stored fields and its matrix.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResolution {
    pub item_id: Uuid,
    pub sku_id: Uuid,
    pub price_book_entry_id: Option<Uuid>,
    pub charge_kind: Option<ChargeKind>,
    pub period: Option<String>,
    /// The entry's model (D-427); `None` for an item without an entry.
    pub model: Option<Model>,
    /// The default chain, then each value registered today in the registry's order, then any
    /// other value a pin names.
    pub chains: Vec<Chain>,
}
/// The tenant settings D-421 falls back to. `invoice_line_templates` is keyed by SKU type
/// (`recurring`, `usage`, `one_time`, `bundle`), as `PUT /settings` stores it.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantDefaults {
    pub default_timing: String,
    pub default_gl: Option<String>,
    pub default_tax_category: Option<String>,
    pub invoice_line_templates: BTreeMap<String, String>,
}
/// A resolved input and where it came from; both `None` when no source holds it.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: Option<String>,
    pub source: Option<Source>,
}
/// The invoice inputs one item's binding carries (D-421).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceInputs {
    pub invoice_line_template: Resolved,
    pub gl_code: Resolved,
    pub tax_category: Resolved,
    pub billing_timing: Resolved,
}

/// What one accepted pin binds: the pinned price, for one (item, value).
type Pinned<'a> = BTreeMap<(Uuid, Option<String>), &'a Price>;

/// Resolve every item of `ctx` on `date` with the caller's `pins` (D-419, D-420). Returns data
/// only, never a total.
/// # Errors
/// `PINS_TOO_MANY` above [`MAX_PINS`]; `PIN_FOREIGN` for the whole request when a pin names no
/// approved price of an entry an item names, or a value pin names a price that is not a
/// default-chain price; then `PIN_DUPLICATE` when two pins land on one (item, value).
pub fn matrix(
    ctx: &ResolveContext,
    date: Date,
    pins: &[Pin],
) -> Result<Vec<ItemResolution>, RuleError> {
    if pins.len() > MAX_PINS {
        return Err(RuleError::new("PINS_TOO_MANY"));
    }
    let pinned = judge_pins(ctx, pins)?;
    Ok(ctx
        .items
        .iter()
        .map(|item| resolve_item(ctx, item, date, &pinned))
        .collect())
}

/// Every (item, pinned price) a pin's price belongs to: the items whose entry holds it.
fn owners(ctx: &ResolveContext, price_id: Uuid) -> Vec<(Uuid, &Price)> {
    ctx.items
        .iter()
        .filter_map(|item| {
            let entry = item.entry.as_ref()?;
            let found = entry.prices.iter().find(|p| p.id == price_id)?;
            Some((item.id, found))
        })
        .collect()
}
/// D-419: every pin first proves its ownership (`PIN_FOREIGN` for the whole request), then each
/// (item, value) takes at most one pin (`PIN_DUPLICATE`). A value pin's value is not checked
/// against today's registry: a removed value still resolves for the subscription that holds it.
fn judge_pins<'a>(ctx: &'a ResolveContext, pins: &[Pin]) -> Result<Pinned<'a>, RuleError> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-1
    let mut judged = Vec::with_capacity(pins.len());
    for pin in pins {
        let owners = owners(ctx, pin.price_id);
        let valid = !owners.is_empty()
            && owners.iter().all(|(_, p)| {
                p.state == PriceState::Approved
                    && (pin.dim_value.is_none() || p.dim_value.is_none())
            });
        if !valid {
            return Err(RuleError::new("PIN_FOREIGN"));
        }
        judged.push((pin, owners));
    }
    let mut pinned = Pinned::new();
    for (pin, owners) in judged {
        for (item, p) in owners {
            let value = pin.dim_value.clone().or_else(|| p.dim_value.clone());
            if pinned.insert((item, value), p).is_some() {
                return Err(RuleError::new("PIN_DUPLICATE"));
            }
        }
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-1
    Ok(pinned)
}

fn resolve_item(ctx: &ResolveContext, item: &Item, date: Date, pinned: &Pinned) -> ItemResolution {
    let chains = item.entry.as_ref().map_or_else(Vec::new, |entry| {
        let mut values: Vec<Option<String>> = vec![None];
        values.extend(entry.values.iter().cloned().map(Some));
        for (_, value) in pinned.keys().filter(|(owner, _)| *owner == item.id) {
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
        values
            .into_iter()
            .map(|dim| {
                // @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-4
                let pin = pinned.get(&(item.id, dim.clone())).copied();
                let binding =
                    bind(entry, dim.as_deref(), pin, date).map(|(p, pinned_from)| Binding {
                        price: p.clone(),
                        pinned_from,
                        keep_for_bound: ctx.keep_for_bound.contains(&p.id),
                    });
                // @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-4
                Chain {
                    dim_value: dim,
                    binding,
                }
            })
            .collect()
    });
    ItemResolution {
        item_id: item.id,
        sku_id: item.sku_id,
        price_book_entry_id: item.entry.as_ref().map(|e| e.id),
        charge_kind: item.entry.as_ref().map(|e| e.charge_kind),
        period: item.entry.as_ref().and_then(|e| e.period.clone()),
        model: item.entry.as_ref().map(|e| e.model),
        chains,
    }
}

/// The binding of one (item, value) and the pin it came from.
fn bind<'a>(
    entry: &'a Entry,
    dim: Option<&str>,
    pin: Option<&'a Price>,
    date: Date,
) -> Option<(&'a Price, Option<Uuid>)> {
    // @cpt-begin:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-3
    match pin {
        // Rule 1: a signup binds the price in force, the value's own chain else the default.
        // @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-3
        None => price::version_at(&entry.prices, entry.id, date, dim).map(|p| (p, None)),
        // @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-3
        Some(pinned) => renewal(entry, dim, pinned, date).map(|p| (p, Some(pinned.id))),
    }
    // @cpt-end:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-3
}

/// Rules 2–4 for a pinned (item, value).
fn renewal<'a>(
    entry: &'a Entry,
    dim: Option<&str>,
    pinned: &'a Price,
    date: Date,
) -> Option<&'a Price> {
    // Rule 4: a default-chain pin of a value moves to the value's own chain when the own price
    // in force is `all` and started after the pin; a `new` own price does not move it.
    if let Some(value) = dim
        && pinned.dim_value.is_none()
        && let Some(own) = price::own_version_at(&entry.prices, entry.id, date, Some(value))
        && own.eligibility == Eligibility::All
        && own.effective_from > pinned.effective_from
    {
        return Some(own);
    }
    // Rule 2: the pinned walk.
    let reached = walk(&entry.prices, pinned, date);
    // Rule 3: a binding is always in force; otherwise the value binds as a signup would.
    if binds_on(reached, date) {
        Some(reached)
    } else {
        price::version_at(&entry.prices, entry.id, date, dim)
    }
}

/// Rule 2: from the pinned price, take each approved successor of its chain that starts by
/// `date` with eligibility `all`; stop before the first `new` one (spec §7.1, D-397).
fn walk<'a>(prices: &'a [Price], pinned: &'a Price, date: Date) -> &'a Price {
    // @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-2
    let chain = price::approved_prices(
        prices,
        pinned.price_book_entry_id,
        pinned.dim_value.as_deref(),
    );
    let mut reached = pinned;
    for next in chain.into_iter().skip_while(|p| p.id != pinned.id).skip(1) {
        if next.effective_from > date || next.eligibility == Eligibility::New {
            break;
        }
        reached = next;
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk:p1:inst-read-contract-events-renewal-walk-2
    reached
}

/// Rule 3: whether a price the walk reached still binds on `date`. It must have started, and its
/// own end ([`own_end`]) must not have passed. The start of a successor the walk did not take (a
/// `new` price) does not end it for the pin; that is what `keep_for_bound` marks.
fn binds_on(price: &Price, date: Date) -> bool {
    price.effective_from <= date && own_end(price).is_none_or(|end| date < end)
}
/// A price's own end (D-420 read precisely, D-425), read from the stored fields normalisation
/// does not rewrite: a temporary price's `temporary_until`, else an explicitly closed price's
/// `effective_to`; `None` for a price whose stored window only a successor's start closes. Never a
/// temporary price's normalised `effective_to`: a pair nested inside it cuts that at the inner
/// start, which is a successor the walk did not take, not the price's end (phase 4 review C-1).
/// An explicit close that a later pair cuts is stored as `min(end, next)` and ends there.
#[must_use]
pub fn own_end(price: &Price) -> Option<Date> {
    price
        .temporary_until
        .or_else(|| price.effective_to.filter(|_| price.closed_explicitly))
}

/// A stored text counts only when it holds more than blanks.
fn present(text: Option<&str>) -> Option<&str> {
    text.filter(|t| !t.trim().is_empty())
}
/// The first source that holds a text, in precedence order.
fn first(sources: &[(Option<&str>, Source)]) -> Resolved {
    sources
        .iter()
        .find_map(|(text, source)| present(*text).map(|t| (t, *source)))
        .map_or(
            Resolved {
                value: None,
                source: None,
            },
            |(t, source)| Resolved {
                value: Some(t.to_owned()),
                source: Some(source),
            },
        )
}
const fn timing(t: BillingTiming) -> &'static str {
    match t {
        BillingTiming::Advance => "advance",
        BillingTiming::Arrears => "arrears",
    }
}

/// D-421: the resolved invoice inputs of one item, each with its source. The invoice line is the
/// entry's override, then the SKU version's template, then the tenant template for the charge
/// kind (an item without an entry takes its SKU version's type, as the prototype does); GL code,
/// tax category and billing timing are the SKU version's, then the tenant's (PRD AC #13). A blank
/// text falls through to the next source.
#[must_use]
pub fn invoice_inputs(
    entry_override: Option<&str>,
    sku_version: Option<&SkuVersion>,
    defaults: &TenantDefaults,
    charge_kind: Option<ChargeKind>,
) -> InvoiceInputs {
    let sku = sku_version.map(|v| &v.content);
    // A charge kind's name is its SKU type's (`charge_kind_for`), the key the templates use.
    let kind = charge_kind
        .map(ChargeKind::as_str)
        .or_else(|| sku.map(|c| c.r#type.as_str()));
    let template = kind.and_then(|k| defaults.invoice_line_templates.get(k));
    InvoiceInputs {
        invoice_line_template: first(&[
            (entry_override, Source::Entry),
            (
                sku.and_then(|c| c.invoice_line_template.as_deref()),
                Source::Sku,
            ),
            (template.map(String::as_str), Source::Tenant),
        ]),
        gl_code: first(&[
            (sku.and_then(|c| c.gl_code.as_deref()), Source::Sku),
            (defaults.default_gl.as_deref(), Source::Tenant),
        ]),
        tax_category: first(&[
            (sku.and_then(|c| c.tax_category.as_deref()), Source::Sku),
            (defaults.default_tax_category.as_deref(), Source::Tenant),
        ]),
        billing_timing: first(&[
            (sku.and_then(|c| c.billing_timing).map(timing), Source::Sku),
            (Some(defaults.default_timing.as_str()), Source::Tenant),
        ]),
    }
}
#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;

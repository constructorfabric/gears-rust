//! Allocation precedence policies. Given a lump of cash and the open invoices it
//! may pay, a policy decides — purely, with no money math beyond `min` — how much
//! each invoice receives.
//!
//! Every policy is *sequential fill*, not proportional: order the candidates (an
//! optional `hint` jumps one to the front), give each `min(remaining, open)` until
//! the lump is exhausted. Any leftover (`lump - Σ given`) is implicit — it stays
//! in the unallocated pool and is NOT returned. Policies differ ONLY in the order
//! they walk the candidates; the fill itself is shared. Two orders exist today:
//! [`oldest_first`] (`original_posted_at` ascending) and [`highest_amount_first`]
//! (`open` descending), both stable and total so the same inputs always
//! split identically. [`select_split`] dispatches on a [`PrecedenceStrategy`].

use std::cmp::Ordering;

use crate::domain::exact_money::{ExactAmount, map_exact_error, matching_spec};
use bss_ledger_sdk::PostedMoney;
use rust_decimal::Decimal;
use time::OffsetDateTime;
use toolkit_macros::domain_model;

/// The default precedence policy id stamped on an allocation when none is chosen.
/// Equals [`PrecedenceStrategy::OldestFirst`]'s [`policy_ref`](PrecedenceStrategy::policy_ref);
/// bump the underlying `.vN` if a fill order ever changes — the policy is part of
/// the allocation's audit trail.
pub const DEFAULT_PRECEDENCE_POLICY: &str = "oldest-first.v1";

/// Which order a lump walks its candidates in. Every strategy shares the same
/// sequential-fill body (see module docs) and differs ONLY in the sort key. The
/// chosen strategy's [`policy_ref`](Self::policy_ref) is stamped onto the
/// allocation's audit trail.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrecedenceStrategy {
    /// `original_posted_at` ascending (`None` last), ties by `invoice_id` — pay
    /// the oldest receivable first. Policy id `oldest-first.v1`.
    OldestFirst,
    /// `open` descending, ties by `invoice_id` — pay the largest open
    /// balance first. Policy id `highest-amount-first.v1`.
    HighestAmountFirst,
}

impl PrecedenceStrategy {
    /// The stable policy id stamped on an allocation's audit trail. Inverse of
    /// [`parse`](Self::parse).
    #[must_use]
    pub fn policy_ref(self) -> &'static str {
        match self {
            Self::OldestFirst => "oldest-first.v1",
            Self::HighestAmountFirst => "highest-amount-first.v1",
        }
    }

    /// Parse a policy id back into a strategy — the inverse of
    /// [`policy_ref`](Self::policy_ref). Accepts exactly the ids it emits;
    /// anything else (incl. an unknown `.vN`) is `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "oldest-first.v1" => Some(Self::OldestFirst),
            "highest-amount-first.v1" => Some(Self::HighestAmountFirst),
            _ => None,
        }
    }
}

/// One open invoice a lump may be applied to. `open` is the remaining
/// receivable in major units; `original_posted_at` drives the oldest-first order
/// (`None` ⇒ sorts last, treated as newest).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// External invoice identity — carried onto the resulting [`Allocated`] and,
    /// downstream, the `invoice_id` dim of the CR AR line.
    pub invoice_id: String,
    /// Remaining open receivable in the invoice's major units. Candidates with
    /// `open <= 0` receive nothing (skipped during fill).
    pub open: PostedMoney,
    /// When the invoice was originally posted — the oldest-first sort key. `None`
    /// sorts after every `Some` (treated as the newest / last to be paid).
    pub original_posted_at: Option<OffsetDateTime>,
}

/// One invoice's share of a lump: it receives `amount` (always `> 0` — the
/// policy never emits a zero allocation). Consumed by
/// [`crate::domain::payment::allocation::build_allocation_entry`] as a split.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocated {
    pub invoice_id: String,
    pub amount: PostedMoney,
}

/// Split `lump` across `candidates` using `strategy`'s order, returning only
/// the invoices that received a positive amount (in fill order). The dispatcher
/// over [`oldest_first`] / [`highest_amount_first`]; see those for the shared
/// fill/`hint`/leftover semantics.
/// # Errors
/// Rejects any candidate with conflicting currency or stored scale, including skipped
/// candidates. Exact arithmetic and final-share bounds retain their named money errors.
pub fn select_split(
    candidates: &[Candidate],
    lump: &PostedMoney,
    hint: Option<&str>,
    strategy: PrecedenceStrategy,
) -> Result<Vec<Allocated>, crate::domain::error::DomainError> {
    match strategy {
        PrecedenceStrategy::OldestFirst => oldest_first(candidates, lump, hint),
        PrecedenceStrategy::HighestAmountFirst => highest_amount_first(candidates, lump, hint),
    }
}

/// Split `lump` across `candidates` oldest-first, returning only the
/// invoices that received a positive amount (in fill order).
///
/// Order: `original_posted_at` ascending, `None` last, ties broken by
/// `invoice_id` ascending (a stable, total order). See [`fill`] for the shared
/// `hint` / fill / leftover semantics this delegates to.
/// # Errors
/// Rejects any candidate with conflicting currency or stored scale, including skipped
/// candidates. Exact arithmetic and final-share bounds retain their named money errors.
pub fn oldest_first(
    candidates: &[Candidate],
    lump: &PostedMoney,
    hint: Option<&str>,
) -> Result<Vec<Allocated>, crate::domain::error::DomainError> {
    fill(candidates, lump, hint, |a, b| {
        // `None` (no posted_at) sorts AFTER any `Some` — treat as newest. Map to
        // an `(is_none, posted_at)` tuple so `false < true` puts `Some` first,
        // then break ties on `invoice_id`.
        let key_a = (a.original_posted_at.is_none(), a.original_posted_at);
        let key_b = (b.original_posted_at.is_none(), b.original_posted_at);
        key_a
            .cmp(&key_b)
            .then_with(|| a.invoice_id.cmp(&b.invoice_id))
    })
}

/// Split `lump` across `candidates` largest-open-balance first, returning
/// only the invoices that received a positive amount (in fill order).
///
/// Order: `open` descending, ties broken by `invoice_id` ascending (a
/// stable, total order). See [`fill`] for the shared `hint` / fill / leftover
/// semantics this delegates to.
/// # Errors
/// Rejects any candidate with conflicting currency or stored scale, including skipped
/// candidates. Exact arithmetic and final-share bounds retain their named money errors.
pub fn highest_amount_first(
    candidates: &[Candidate],
    lump: &PostedMoney,
    hint: Option<&str>,
) -> Result<Vec<Allocated>, crate::domain::error::DomainError> {
    fill(candidates, lump, hint, |a, b| {
        // Largest open first (`b` vs `a` ⇒ descending), then `invoice_id`
        // ascending so equal balances stay deterministic.
        b.open
            .amount()
            .cmp(&a.open.amount())
            .then_with(|| a.invoice_id.cmp(&b.invoice_id))
    })
}

/// Shared sequential-fill core. Orders `candidates` by `order` (a stable sort, so
/// the comparator alone defines precedence), applies the `hint`, then fills.
///
/// `hint`: a hint that names a present candidate jumps to the FRONT; the rest keep
/// their `order`. A hint matching nothing is a no-op.
///
/// Fill: walk the ordered list; each candidate gets `give = min(remaining,
/// open)`; if `give > 0` it is pushed and `remaining -= give`. Candidates
/// with `open <= 0` are skipped. Stops once `remaining == 0`; any leftover
/// lump is implicit (left for the pool) and not returned. Exact major-unit arithmetic retains wide remaining balances;
/// only emitted shares are narrowed. No proportional or float math.
fn fill(
    candidates: &[Candidate],
    lump: &PostedMoney,
    hint: Option<&str>,
    order: impl Fn(&Candidate, &Candidate) -> Ordering,
) -> Result<Vec<Allocated>, crate::domain::error::DomainError> {
    for candidate in candidates {
        matching_spec(lump, &candidate.open)?;
    }
    // Order the candidates by reference (cheap: no clone of the open invoices).
    // A stable sort, so `order` alone defines precedence.
    let mut ordered: Vec<&Candidate> = candidates.iter().collect();
    ordered.sort_by(|a, b| order(a, b));

    if let Some(hint_id) = hint
        && let Some(pos) = ordered.iter().position(|c| c.invoice_id == hint_id)
    {
        let hinted = ordered.remove(pos);
        ordered.insert(0, hinted);
    }

    let mut remaining = ExactAmount::from_decimal(lump.amount());
    let zero = ExactAmount::from_decimal(Decimal::ZERO);
    let mut out = Vec::new();
    for cand in ordered {
        if remaining == zero || remaining.is_negative() {
            break;
        }
        if cand.open.amount() <= Decimal::ZERO {
            continue;
        }
        let open = ExactAmount::from_decimal(cand.open.amount());
        let difference = remaining.checked_sub(&open).map_err(map_exact_error)?;
        let give = if difference.is_negative() {
            remaining.clone()
        } else {
            open
        };
        remaining = remaining.checked_sub(&give).map_err(map_exact_error)?;
        out.push(Allocated {
            invoice_id: cand.invoice_id.clone(),
            amount: give
                .into_posted_exact(lump.currency().clone())
                .map_err(map_exact_error)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "precedence_tests.rs"]
mod tests;

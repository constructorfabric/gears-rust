//! Bundle composition's vocabulary, and the `RevShareReconciler` of
//! `design/08-bundles.md` `inst-rs-residual` (D-07) and `inst-rs-sum` (D-55).
//!
//! # The reconciler is the whole of D-07, and it is arithmetic
//!
//! Operators author percentages that come from contracts — 33.33%, three ways —
//! and three of those are 9999 basis points. D-07 accepts that: authoring is
//! tolerant to `|Σ(share_bp) + platform_cut_bp − 10000| ≤ 1 bp`, and **publish
//! normalizes** the group's nominated absorber so the published effective shares
//! sum to exactly 10000. The typed values stay beside the effective ones for
//! audit, which is why `pricing_bundle_revshare` carries two columns and not one
//! rewritten in place.
//!
//! Everything here is per **group** — one `(bundle, vendor SKU)` — because that
//! is the scope D-55 re-typed the rule onto: *"sum to 100% **per** included
//! vendor SKU"*. A reconciler ranging over a bundle would be answering a question
//! nobody asks.
//!
//! # Two refusals, and only one of them has a code of its own
//!
//! `RESIDUAL_OVER_TOLERANCE` is D-07's, declared in §5, and it is what a residual
//! over 1 bp gets — the six-way even split (6 × 1666 = 9996) that decision names
//! as the case an operator must reconcile by hand.
//!
//! `REVSHARE_UNBALANCED` is §5's code for *structural* malformation, and it is
//! what the other three refusals render under: a group with no party rows at all
//! (the shares are the allocation base, and a platform cut alone is not a split),
//! an absorber naming a party the group does not hold, and a normalization that
//! would push the absorber off the 0…10000 scale. **None of those three has a
//! code of its own in §5**, and a gear may mint `RepoError`/`DomainError`
//! variants freely while a wire code is the design set's to declare — so they
//! render under the existing one and the gap is written into the owed register
//! (B-5). The reading is not a stretch: in each case no member of the group can
//! take the residual, so the group cannot be made to sum to 10000, which is
//! exactly what "structurally malformed shares" describes.
//!
//! # The platform absorbs on its **cut**, a party absorbs on its **share**
//!
//! D-07 says "the group's `residual_absorber_party` has its **effective** share
//! adjusted", and for the platform sentinel that value is `platform_cut_bp` —
//! the platform holds no party row, by construction. Both are reported, so a
//! caller writing the normalization back never has to decide which column moved.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// The publish-time refusal for a residual the tolerance does not admit
/// (§5, **422 architectural**; `inst-rs-residual`, D-07).
///
/// Declared by §5's problem-response list — *"`RESIDUAL_OVER_TOLERANCE` (422 —
/// `|Σ − 10000| > 1 bp`; D-07)"* — so this is a code the design set names rather
/// than one minted here.
pub const RESIDUAL_OVER_TOLERANCE: &str = "RESIDUAL_OVER_TOLERANCE";

/// Structurally malformed shares, or a missing explicit platform cut
/// (§5, **422 architectural**; `inst-rs-sum`).
///
/// D-07 narrowed this code to exactly that when it deleted `RESIDUAL_UNASSIGNED`.
/// See the module doc for the three refusals that render under it and for why
/// they do.
pub const REVSHARE_UNBALANCED: &str = "REVSHARE_UNBALANCED";

/// Rev-share authored on an `own_price` bundle (§5, **422 architectural**;
/// `inst-rs-sum`, D-55).
pub const REVSHARE_BASIS_UNSUPPORTED: &str = "REVSHARE_BASIS_UNSUPPORTED";

/// The whole a group's shares and its platform cut must add up to: 100%, in
/// basis points.
pub const FULL_ALLOCATION_BP: i32 = 10_000;

/// The authoring tolerance, in basis points (B2, D-07). One, because the PRD's
/// 0.01% is one basis point and the worked example that motivated the whole
/// decision is exactly that far out.
pub const RESIDUAL_TOLERANCE_BP: i32 = 1;

/// The reserved absorber token standing for the platform itself (D-07).
///
/// It is `pricing_bundle_revshare_group.residual_absorber_party`'s default,
/// which is what makes an "unnominated" state unrepresentable, and
/// [`Party::new`] refuses it so a party can never collide with it.
pub const PLATFORM_SENTINEL: &str = "platform";

/// How a bundle arrives at its price (`inst-bb-declared`).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PriceBasis {
    /// The bundle sums its components' recurring amounts; it carries no price
    /// rows of its own (`inst-bb-rowless`).
    SumOfParts,
    /// The bundle carries its own price rows on the canonical scope key, and
    /// answers to every row-quantified plan rule (`inst-bb-own`).
    OwnPrice,
}

impl PriceBasis {
    /// Both bases, in §6's order.
    pub const ALL: &'static [Self] = &[Self::SumOfParts, Self::OwnPrice];

    /// The token `pricing_bundle.price_basis` stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SumOfParts => "sum_of_parts",
            Self::OwnPrice => "own_price",
        }
    }

    /// Read a stored token back.
    ///
    /// `None` is a value `chk_pricing_bundle_price_basis` would have refused, so
    /// it means a corrupt row rather than a caller error.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|b| b.as_str() == token)
    }
}

/// How a bundle's charges lay out on an invoice (B3, `inst-rs-itemization`).
///
/// **Either layout preserves the per-SKU rev-share**, which is the only thing
/// this axis promises: Marketplace accrues per SKU regardless of what the
/// invoice shows, so nothing downstream of the read model may branch on it for
/// allocation purposes.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InvoiceItemization {
    /// One line for the bundle.
    Aggregate,
    /// A line per component.
    Itemize,
}

impl InvoiceItemization {
    /// Both layouts, in §6's order.
    pub const ALL: &'static [Self] = &[Self::Aggregate, Self::Itemize];

    /// The token `pricing_bundle.invoice_itemization` stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aggregate => "aggregate",
            Self::Itemize => "itemize",
        }
    }

    /// Read a stored token back.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|l| l.as_str() == token)
    }
}

/// A rev-share party — one recipient within a `(bundle, vendor SKU)` group.
///
/// A newtype rather than a bare `String` because two invariants ride on it and
/// both are load-bearing: a party may not be blank, and a party may not be named
/// [`PLATFORM_SENTINEL`]. The second is what keeps
/// `residual_absorber_party` unambiguous — that column holds either a party of
/// the group or the sentinel, and if a party could spell the sentinel there
/// would be no way to tell D-07's default from a nomination.
/// `chk_pricing_bundle_revshare_party` is the physical floor under the same rule.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Party(String);

impl Party {
    /// A party, or `None` for a value the invariants refuse.
    #[must_use]
    pub fn new(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed == PLATFORM_SENTINEL {
            return None;
        }
        Some(Self(trimmed.to_owned()))
    }

    /// The stored value.
    #[must_use]
    pub fn get(&self) -> &str {
        &self.0
    }
}

/// Who takes the publish-time residual of one group (D-07).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Absorber {
    /// The platform, which holds no party row: the residual lands on the
    /// group's `platform_cut_bp`. This is the **default**, so an unnominated
    /// state cannot exist.
    Platform,
    /// A named party of this group; the residual lands on that party's effective
    /// share. An absorber naming a party the group does not hold is
    /// [`REVSHARE_UNBALANCED`].
    Party(Party),
}

impl Absorber {
    /// Read a stored `residual_absorber_party` value back.
    ///
    /// The sentinel is checked first, which is the only ordering that works:
    /// [`Party::new`] refuses the sentinel, so a fall-through would answer
    /// `None` for the default value of the column.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        if token == PLATFORM_SENTINEL {
            return Some(Self::Platform);
        }
        Party::new(token).map(Self::Party)
    }

    /// The token `pricing_bundle_revshare_group.residual_absorber_party` stores.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Platform => PLATFORM_SENTINEL,
            Self::Party(party) => party.get(),
        }
    }
}

/// One party's **typed** share of one group.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyShare {
    /// The recipient.
    pub party: Party,
    /// What the operator authored, in basis points.
    pub share_bp: i32,
}

/// One `(bundle, vendor SKU)` rev-share group, as the reconciler reads it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevShareGroup {
    /// The included vendor SKU this group allocates the revenue of.
    pub vendor_sku_id: Uuid,
    /// The group's explicit platform cut, in basis points.
    pub platform_cut_bp: i32,
    /// Who takes the residual (D-07). Defaults to [`Absorber::Platform`] at the
    /// column, so this is never absent.
    pub residual_absorber: Absorber,
    /// The parties and their typed shares.
    pub parties: Vec<PartyShare>,
}

/// What publish writes back for one group.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciledGroup {
    /// The vendor SKU this is the reconciliation of.
    pub vendor_sku_id: Uuid,
    /// The residual that was absorbed, signed: positive means the authored
    /// values were **short** of 10000 and the absorber gained it. Recorded
    /// because D-07 requires the adjustment to be auditable, not merely applied.
    pub adjustment_bp: i32,
    /// The platform cut after normalization. Differs from the authored one
    /// exactly when the absorber is [`Absorber::Platform`].
    pub effective_platform_cut_bp: i32,
    /// Each party's effective share, in the order the group listed them.
    pub effective_shares: Vec<(Party, i32)>,
}

impl ReconciledGroup {
    /// What the effective values add up to.
    ///
    /// Exists so the invariant can be **asserted** rather than trusted: D-07's
    /// promise is that this is [`FULL_ALLOCATION_BP`] for every group publish
    /// admits, and a caller writing the values back should be able to check it
    /// without re-deriving the sum.
    #[must_use]
    pub fn sums_to(&self) -> i32 {
        self.effective_platform_cut_bp
            + self.effective_shares.iter().map(|(_, bp)| *bp).sum::<i32>()
    }
}

/// A rev-share refusal, carrying the wire code the design set declares for it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevShareRefusal {
    /// `|Σ − 10000| > 1 bp` (D-07). Carries the residual so the operator is told
    /// how far out they are rather than only that they are.
    ResidualOverTolerance {
        /// The vendor SKU whose group failed.
        vendor_sku_id: Uuid,
        /// The signed residual, in basis points.
        residual_bp: i32,
    },
    /// Structurally malformed shares — see the module doc for the three states
    /// that reach here and why none of them has a code of its own.
    Unbalanced {
        /// The vendor SKU whose group failed.
        vendor_sku_id: Uuid,
        /// What is malformed about it, for the authoring surface.
        detail: String,
    },
    /// Rev-share on an `own_price` bundle (D-55).
    BasisUnsupported,
}

impl RevShareRefusal {
    /// The wire code §5 declares for this refusal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ResidualOverTolerance { .. } => RESIDUAL_OVER_TOLERANCE,
            Self::Unbalanced { .. } => REVSHARE_UNBALANCED,
            Self::BasisUnsupported => REVSHARE_BASIS_UNSUPPORTED,
        }
    }
}

/// Normalize one group onto its absorber (`inst-rs-residual`, D-07).
///
/// The published effective values sum to exactly [`FULL_ALLOCATION_BP`], and the
/// typed values are untouched — the caller keeps them, which is what makes the
/// adjustment auditable.
///
/// # Errors
/// [`RevShareRefusal::ResidualOverTolerance`] when the authored values are more
/// than [`RESIDUAL_TOLERANCE_BP`] from the whole;
/// [`RevShareRefusal::Unbalanced`] when no member of the group can take the
/// residual — an empty party list, an absorber outside the group, or a
/// normalization that would leave the absorber off the 0…10000 scale.
pub fn reconcile(group: &RevShareGroup) -> Result<ReconciledGroup, RevShareRefusal> {
    let unbalanced = |detail: &str| RevShareRefusal::Unbalanced {
        vendor_sku_id: group.vendor_sku_id,
        detail: detail.to_owned(),
    };

    // The shares are the allocation base. A group holding only a platform cut
    // allocates a vendor SKU's revenue to nobody, which is not a split at all.
    if group.parties.is_empty() {
        return Err(unbalanced(
            "a rev-share group must hold at least one party: the shares are the allocation base",
        ));
    }

    // **Checked, because nothing on this side of the store bounds the operands**. Every
    // `share_bp` and every `platform_cut_bp` the store holds is `CHECK`-bounded to `0..=10000`
    // — `chk_pricing_bundle_revshare_share_bp` (`pricing_bundle_revshare`) and
    // `chk_pricing_bundle_revshare_group_platform_cut_bp` (`pricing_bundle_revshare_group`) — and the party
    // list has no length bound at all, so the *sum* is unbounded even where each term is not.
    //
    // **`reconcile` does not reach this from the wire before persistence**, though
    // the shape invites the assumption. Both callers read stored rows —
    // `bundle_rules::check_rev_share`
    // over a `BundleComposition` assembled by `infra::bundle::assemble` from
    // `bundle_repo::load_composition`, and the commit path through
    // `load_composition_on`. `api::rest::bundles::draft_of` does take an unbounded
    // `platform_cut_bp: i32` off the wire, but it hands the draft to the repo write,
    // where the CHECK refuses it; it never reaches here. So this is defence in depth
    // against a future in-memory caller, not a live wire guard — which is exactly
    // why it must stay checked rather than be argued away as unreachable.
    //
    // Structural rather than residual: a total that does not exist is not a total
    // that is 4 bp out, and `RESIDUAL_OVER_TOLERANCE`'s whole payload is that
    // distance.
    let authored: i32 = group
        .parties
        .iter()
        .try_fold(group.platform_cut_bp, |acc, p| acc.checked_add(p.share_bp))
        .ok_or_else(|| {
            unbalanced(
                "the authored shares and platform cut do not sum inside the basis-point domain",
            )
        })?;
    // The same refusal for the same reason, one operator later: the `try_fold`
    // above admits any `authored` inside `i32`, and `10_000 - i32::MIN` is not.
    // A `checked_add` guarding a fold whose result is then subtracted unchecked
    // moves the panic one line down rather than removing it.
    let residual = FULL_ALLOCATION_BP.checked_sub(authored).ok_or_else(|| {
        unbalanced("the authored shares and platform cut do not sum inside the basis-point domain")
    })?;
    if residual.abs() > RESIDUAL_TOLERANCE_BP {
        return Err(RevShareRefusal::ResidualOverTolerance {
            vendor_sku_id: group.vendor_sku_id,
            residual_bp: residual,
        });
    }

    let mut effective_platform_cut_bp = group.platform_cut_bp;
    let mut effective_shares: Vec<(Party, i32)> = group
        .parties
        .iter()
        .map(|p| (p.party.clone(), p.share_bp))
        .collect();

    // Checked for the reason the fold above is, one stage later: the residual is
    // bounded by the tolerance, but the value it lands on is the caller's and is
    // not, so an operand at a bound wraps here having passed both guards. The
    // range check below reads whatever this produces, so it cannot stand in — it
    // would refuse for a distance that never existed.
    let absorb = |value: i32| {
        value
            .checked_add(residual)
            .ok_or_else(|| unbalanced("absorbing the residual does not fit the basis-point domain"))
    };
    match &group.residual_absorber {
        Absorber::Platform => effective_platform_cut_bp = absorb(effective_platform_cut_bp)?,
        Absorber::Party(absorber) => {
            let Some(slot) = effective_shares
                .iter_mut()
                .find(|(party, _)| party == absorber)
            else {
                return Err(unbalanced(&format!(
                    "the residual absorber `{}` is not a party of this group",
                    absorber.get()
                )));
            };
            slot.1 = absorb(slot.1)?;
        }
    }

    // The absorber may not be pushed off the scale it is measured on. Reached
    // only when the authored value sits at a bound, which the CHECKs admit.
    let absorbed = match &group.residual_absorber {
        Absorber::Platform => effective_platform_cut_bp,
        Absorber::Party(absorber) => effective_shares
            .iter()
            .find(|(party, _)| party == absorber)
            .map_or(0, |(_, bp)| *bp),
    };
    if !(0..=FULL_ALLOCATION_BP).contains(&absorbed) {
        return Err(unbalanced(&format!(
            "absorbing {residual} bp would move `{}` to {absorbed} bp, outside 0..={FULL_ALLOCATION_BP}",
            group.residual_absorber.as_str()
        )));
    }

    Ok(ReconciledGroup {
        vendor_sku_id: group.vendor_sku_id,
        adjustment_bp: residual,
        effective_platform_cut_bp,
        effective_shares,
    })
}

/// Does this basis admit rev-share at all (`inst-rs-sum`, D-55)?
///
/// An `own_price` bundle has one bundle amount and no per-vendor-SKU revenue to
/// allocate — no declared allocation base — so Marketplace cannot accrue per SKU
/// from it. Lifting this requires deciding an allocation base (component list
/// prices, say), which is a named Future gate and not a defect.
///
/// The check is on the **count** rather than on the groups themselves because
/// that is all it needs: the refusal is about rev-share existing, not about what
/// it says.
///
/// # Errors
/// [`RevShareRefusal::BasisUnsupported`] for `own_price` with any rev-share
/// group authored.
pub const fn check_basis_admits_rev_share(
    basis: PriceBasis,
    group_count: usize,
) -> Result<(), RevShareRefusal> {
    match basis {
        PriceBasis::SumOfParts => Ok(()),
        PriceBasis::OwnPrice if group_count == 0 => Ok(()),
        PriceBasis::OwnPrice => Err(RevShareRefusal::BasisUnsupported),
    }
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod bundle_tests;

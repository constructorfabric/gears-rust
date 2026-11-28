// Created: 2026-09-11 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Pure hierarchy reduction: turning every visible row of one reference into
//! a single effective outcome (ADR-0005, "Reducing a reference to one item").
//!
//! Shared, verbatim, by the point read
//! ([`crate::domain::secret::service::Service::resolve_credential`]) and the
//! collection read
//! ([`crate::domain::secret::service::Service::list`](../service/list/index.html))
//! so a change to visibility/priority rules cannot apply to one and miss the
//! other (ADR-0005, D3). Takes no [`crate::domain::secret::service::Service`]
//! state — it is a function of the candidate rows and the caller's tenant
//! chain alone, which is what lets it be called from either place without
//! threading `Service`'s private fields through a module boundary.

use credstore_sdk::{
    CredentialStatus, InheritanceStatus, OwnRow, OwnerId, SharingMode, TenantId, Validator,
};
use time::OffsetDateTime;
use toolkit_macros::domain_model;

use crate::domain::secret::model::{Fallback, SecretRow, SecretStatus};

/// Outcome of reducing every visible row of one reference to one item.
#[domain_model]
pub(crate) struct Reduced<'a> {
    /// The caller's own-tenant row, if any (of any status — a `declared`
    /// row still "holds" the reference, ADR-0004). Two-phase priority:
    /// `private` beats non-`private` at the caller's own tenant.
    pub own: Option<&'a SecretRow>,
    /// The nearest row that actually resolves — `active` (an expired one
    /// included: expiry applies to the secret, so the expired record stays
    /// decisive and nothing behind it is consulted), or `declared` with
    /// `fallback: none` (a suppressing row that competes and blocks). `None`
    /// iff nothing in `candidates` resolves.
    pub winner: Option<&'a SecretRow>,
    /// `own.or(winner)`: the row whose `sharing`/`expires_at`/
    /// `secret_type_uuid` describe the reference's effective record.
    pub effective: &'a SecretRow,
    /// The row a value may be served from: the decisive `winner`, but only
    /// when its `secret_type_uuid` equals the effective record's type (the
    /// read rule of ADR-0010, Inheritance). `None` when nothing resolves or
    /// when the caller's own row of one type sits over a decisive row of
    /// another — which `private` rows, exempt from the create-time type
    /// checks, or a race between creates can produce. Callers check this
    /// BEFORE any expiry test, so such a winner never yields `SecretExpired`,
    /// and never fall through to a farther row.
    pub value_row: Option<&'a SecretRow>,
    /// Whether the effective value is the caller's own, inherited, an
    /// override of an inherited value, or a suppressing record with no
    /// value.
    pub inheritance: InheritanceStatus,
}

impl Reduced<'_> {
    /// The caller's own row as the SDK reports it: its `status`
    /// (`declared`/`active`/`expired`, `expired` derived from `expires_at`
    /// here, at read time), `fallback`, `updated_at`, `owner_id` and the
    /// validator naming this generation — never a property of the effective
    /// row (ADR-0004, "Read actions follow the projection"). `None` when the
    /// caller holds no own row.
    #[must_use]
    pub(crate) fn own_row(&self) -> Option<OwnRow> {
        self.own.map(|o| own_row_of(o, OffsetDateTime::now_utc()))
    }
}

/// The SDK view of the caller's own row `o`, with `expired` judged at `now`.
#[must_use]
pub(crate) fn own_row_of(o: &SecretRow, now: OffsetDateTime) -> OwnRow {
    OwnRow {
        status: if o.is_expired(now) {
            CredentialStatus::Expired
        } else if o.status == SecretStatus::Active {
            CredentialStatus::Active
        } else {
            CredentialStatus::Declared
        },
        fallback: o.fallback.into(),
        updated_at: o.updated_at,
        owner_id: o.owner_id,
        validator: Validator {
            id: o.id,
            version: o.version,
        },
    }
}

/// A row is a resolution candidate (ADR-0008, Resolution): `active` (expired
/// or not — an expired record is still the decisive one), or `declared` with
/// `fallback: none` (a suppressing row that competes and, when nearest,
/// wins). A `declared`/`inherit` row never competes. Mirrors
/// `infra::storage::repo_impl::reads::resolution_eligible_condition`, which
/// applies the same predicate in SQL.
fn resolvable(r: &SecretRow) -> bool {
    match r.status {
        SecretStatus::Active => true,
        SecretStatus::Declared => r.fallback == Fallback::None,
    }
}

/// Reduce every visible row of one reference — `candidates`, in any order —
/// to a single outcome, or `None` if `candidates` is empty (nothing to
/// report: neither an own row nor a resolvable ancestor row exists).
///
/// The decisive row is chosen from all candidates, unclamped by type or
/// reference; the caller then applies the PDP scope to `effective` (the
/// record the caller sees) and serves a value only from `value_row`. When
/// the winner is of another type than `effective`, the inheritance status is
/// reported as `Own`, as if that winner did not exist.
///
/// `candidates` MUST be every row of the reference visible to `req`/`subject`
/// across `chain` (ADR-0005: "reduction sees every row a value read would
/// see", not only those a SQL clamp admitted) — the same set
/// [`crate::domain::secret::repo::SecretRepo::resolve_candidates`] returns
/// for a single reference. `subject` is not consulted here: the repo's own
/// visibility predicate already restricts `candidates`'s private rows to the
/// caller, so nothing about *which* rows compete depends on it a second
/// time — it is carried for signature symmetry with the SQL-side predicate
/// it mirrors, and so a caller need not special-case dropping it.
#[allow(
    clippy::ref_option,
    reason = "Reduced borrows into `candidates`; `Option<&SecretRow>` is the natural shape for \
              fields callers match on, and `&Option<_>` would only add an extra deref"
)]
pub(crate) fn reduce_reference<'a>(
    candidates: &'a [SecretRow],
    req: TenantId,
    _subject: OwnerId,
    chain: &[uuid::Uuid],
) -> Option<Reduced<'a>> {
    if candidates.is_empty() {
        return None;
    }

    // The caller's own row: two-phase priority, private beats non-private at
    // the same (own) tenant — mirrors `find_own`/`resolve_for_get`.
    let own = candidates
        .iter()
        .filter(|r| r.tenant_id == req)
        .min_by_key(|r| i32::from(r.sharing != SharingMode::Private));

    let pos = |t: TenantId| chain.iter().position(|c| *c == t.0).unwrap_or(usize::MAX);
    let winner = candidates.iter().filter(|r| resolvable(r)).min_by(|a, b| {
        pos(a.tenant_id)
            .cmp(&pos(b.tenant_id))
            .then((a.sharing != SharingMode::Private).cmp(&(b.sharing != SharingMode::Private)))
    });

    let effective = own.or(winner)?;

    // A winner of another type than the record the caller sees is not a
    // source of value, and does not shape the reported inheritance.
    let value_row = winner.filter(|w| w.secret_type_uuid == effective.secret_type_uuid);

    let inheritance = if let Some(w) = value_row {
        if w.status == SecretStatus::Declared {
            // A declared/none winner blocks the walk — the caller's own row
            // or an ancestor's, either way the outcome is the same name
            // (ADR-0008, Resolution).
            InheritanceStatus::Suppressed
        } else if own.is_some_and(|o| o.id == w.id) {
            let ancestor_candidate_exists = candidates.iter().any(|r| r.tenant_id != req);
            if ancestor_candidate_exists {
                InheritanceStatus::Overridden
            } else {
                InheritanceStatus::Own
            }
        } else {
            InheritanceStatus::Inherited
        }
    } else {
        // Nothing (of the effective type) resolves; the caller has an own
        // `declared`/`inherit` row with nothing behind it — reported as `Own`
        // (ADR-0008, "Resolution": the caller's own row reports `Own`).
        InheritanceStatus::Own
    };

    Some(Reduced {
        own,
        winner,
        effective,
        value_row,
        inheritance,
    })
}

#[cfg(test)]
#[path = "reduce_tests.rs"]
mod tests;

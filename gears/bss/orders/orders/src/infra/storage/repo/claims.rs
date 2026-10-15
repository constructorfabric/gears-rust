//! Transactional in-flight overlap claims (Foundation §3.6 step 17; DESIGN §3.7
//! `orders_inflight_overlap_claim`, OL-2; D-83, D-86, D-179, D-182).
//!
//! Runs inside the engine's transition transaction under the aggregate lock, before the
//! version append (step 18) and every other contribution. A collision is a row shortfall of
//! `ON CONFLICT … DO NOTHING`, never a raised unique violation, so the same transaction can
//! still persist the refusal's diagnostics, audit and settlement. Release only ever sets
//! `released_at` to database time; claims are never deleted.
use super::{
    AccessScope, EntityTrait, IntoActiveModel, LockedOrder, QuerySelect, ScopeError,
    SecureEntityExt, SecureInsertExt, TransactionRunner, order, validate_order,
};
use crate::domain::overlap::{
    self, AcquiredClaim, BlockedTuple, ClaimDirective, ClaimOutcome, ClaimRuleError, ClaimTuple,
    HeldClaim, OverlapConflict, OverlapScopeKey,
};
use crate::infra::storage::entity::inflight_overlap_claim as claim;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, DbErr, QueryFilter, QueryTrait};
use std::collections::BTreeSet;
use toolkit_db::secure::{TxAccessMode, TxConfig, TxIsolationLevel};
use uuid::Uuid;

/// Transition transactions run at READ COMMITTED (DESIGN §3.7): under snapshot isolation a
/// conflict with a claim committed after the snapshot raises a serialization failure instead
/// of reporting a shortfall, and the refusal would be decided on state the transaction can no
/// longer read. Explicit, so a changed database default cannot silently alter step 17.
#[must_use]
pub fn transition_tx_config() -> TxConfig {
    TxConfig {
        isolation: Some(TxIsolationLevel::ReadCommitted),
        access_mode: Some(TxAccessMode::ReadWrite),
    }
}

/// The engine's authority for a step-17 replacement, taken from the shared PEP.
pub struct ProposalAuthority<'p> {
    /// Complete proposed arrangement; its payer is the proposed version's payer.
    pub proposed: &'p order::Model,
    /// PDP scope that authorized the proposed arrangement.
    pub proposed_scope: &'p AccessScope,
    /// The caller's order-read scope: a refusal names a conflicting order only through it
    /// (D-179).
    pub disclosure_scope: &'p AccessScope,
    /// The D-188 reserved candidate version this attempt proposes. Recorded as data only; a
    /// refused attempt's version may never materialize (no FK, by design).
    pub proposed_version: i32,
}

/// Every variant aborts the whole transition transaction as an infrastructure outcome; a
/// collision is never one of these.
#[derive(Debug, thiserror::Error)]
pub enum ClaimStoreError {
    #[error(transparent)]
    Store(#[from] ScopeError),
    #[error(transparent)]
    Rule(#[from] ClaimRuleError),
    #[error("claim integrity: {0}")]
    Integrity(&'static str),
}

/// The PostgreSQL `transaction_isolation` value step 17 requires.
const READ_COMMITTED: &str = "read committed";

#[derive(Debug, sea_orm::FromQueryResult)]
struct StepContext {
    t: time::OffsetDateTime,
    isolation: String,
}

impl<T: TransactionRunner> LockedOrder<'_, T> {
    /// Step 17 for the locked order. The directive comes from
    /// [`ClaimDirective::for_transition`]; `authority` is required exactly for `Replace`.
    ///
    /// # Errors
    /// [`ClaimStoreError`] for storage failures, release-count mismatches and contract
    /// violations. The caller must abort the transaction; `Ok(Conflict)` is the only refusal.
    pub(crate) async fn maintain_claims(
        &self,
        directive: &ClaimDirective,
        authority: Option<&ProposalAuthority<'_>>,
    ) -> Result<ClaimOutcome, ClaimStoreError> {
        match (directive, authority) {
            (ClaimDirective::Retain, None) => Ok(ClaimOutcome::Retained),
            (ClaimDirective::ReleaseAll, None) => {
                self.step_context().await?;
                // 17.1: release-all is its own sub-step; it never reaches the acquisition
                // branch. Only Orders claims are touched: uncertain receiver capacity lives in
                // Subscriptions and is retained for reconciliation (D-182, D-198).
                let ids: Vec<Uuid> = self
                    .live_claims()
                    .await?
                    .into_iter()
                    .map(|c| c.claim_id)
                    .collect();
                self.release_claims(&ids).await?;
                Ok(ClaimOutcome::Released(ids))
            }
            (ClaimDirective::Replace(proposal), Some(authority)) => {
                self.replace_claims(proposal, authority).await
            }
            (ClaimDirective::Replace(_), None) => Err(ClaimStoreError::Integrity(
                "replacement requires the authorized proposed arrangement",
            )),
            (ClaimDirective::Retain | ClaimDirective::ReleaseAll, Some(_)) => Err(
                ClaimStoreError::Integrity("only a replacement takes a proposed arrangement"),
            ),
        }
    }

    async fn replace_claims(
        &self,
        proposal: &overlap::ProposedClaims,
        authority: &ProposalAuthority<'_>,
    ) -> Result<ClaimOutcome, ClaimStoreError> {
        let proposed = authority.proposed;
        // The proposed arrangement gets its own complete PDP validation; the current order's
        // grant cannot reserve a foreign payer/resource tuple.
        validate_order(proposed, authority.proposed_scope)?;
        if proposed.order_id != self.row.order_id
            || proposal.payer_tenant_id() != proposed.payer_tenant_id
            || proposal.resource_tenant_id() != self.row.resource_tenant_id
            || proposed.resource_tenant_id != self.row.resource_tenant_id
        {
            return Err(ClaimStoreError::Integrity(
                "proposal is outside the authorized arrangement",
            ));
        }
        if authority.proposed_version <= self.row.current_version {
            return Err(ClaimStoreError::Integrity(
                "claims are taken for a candidate version above the current one",
            ));
        }
        // Checked before any claim statement: a snapshot transaction would turn a collision
        // into a serialization failure, which no path may report (step 17).
        let claimed_at = self.step_context().await?.t;
        let held = self
            .live_claims()
            .await?
            .into_iter()
            .map(held_claim)
            .collect::<Result<Vec<_>, _>>()?;
        let plan = overlap::plan(&held, proposal)?;
        // 17.4: one row per missing tuple, offered one statement at a time in the shared
        // total order, so competing transactions wait on tuples in the same sequence.
        let mut acquired = Vec::with_capacity(plan.missing.len());
        for tuple in &plan.missing {
            if let Some(claim_id) = self.offer_tuple(authority, tuple, claimed_at).await? {
                acquired.push(AcquiredClaim {
                    claim_id,
                    tuple: tuple.clone(),
                });
            }
        }
        if acquired.len() < plan.missing.len() {
            // 17.5: release exactly the IDs this attempt inserted (count-checked, skipped when
            // empty); every claim held on entry stays live.
            let provisional: Vec<Uuid> = acquired.iter().map(|a| a.claim_id).collect();
            self.release_claims(&provisional).await?;
            let taken: BTreeSet<&ClaimTuple> = acquired.iter().map(|a| &a.tuple).collect();
            let mut blocked = Vec::new();
            for tuple in plan.missing.iter().filter(|t| !taken.contains(t)) {
                blocked.push(BlockedTuple {
                    tuple: tuple.clone(),
                    visible_holder: self
                        .visible_holder(tuple, authority.disclosure_scope)
                        .await?,
                });
            }
            return Ok(ClaimOutcome::Conflict(OverlapConflict {
                blocked,
                provisional_released: provisional,
            }));
        }
        // 17.6: only after complete acquisition.
        self.release_claims(&plan.superseded).await?;
        Ok(ClaimOutcome::Admitted {
            retained: plan.retained,
            acquired,
            superseded_released: plan.superseded,
        })
    }

    /// Live claims of this order, locked; the aggregate lock already serializes their writers.
    async fn live_claims(&self) -> Result<Vec<claim::Model>, ScopeError> {
        claim::Entity::find()
            .filter(claim::Column::OrderId.eq(self.row.order_id))
            .filter(claim::Column::ReleasedAt.is_null())
            .lock_exclusive()
            .secure()
            .scope_with(&self.child_scope())
            .all(self.tx)
            .await
    }

    /// Step-17 preconditions, read inside the transaction and projected from the locked order
    /// row so they exist whether or not claims do. `t` is the server-recorded reservation
    /// instant (`clock_timestamp()` after the aggregate lock wait; a later wait on a competing
    /// uncommitted tuple does not move it). The isolation check makes READ COMMITTED a
    /// fail-closed precondition of this repository rather than only the caller's convention.
    async fn step_context(&self) -> Result<StepContext, ClaimStoreError> {
        let rows = order::Entity::find_by_id(self.row.order_id)
            .secure()
            .scope_with(&self.child_scope())
            .project_all(self.tx, |q| {
                q.select_only()
                    .column_as(Expr::cust("clock_timestamp()"), "t")
                    .column_as(
                        Expr::cust("current_setting('transaction_isolation')"),
                        "isolation",
                    )
                    .into_model::<StepContext>()
            })
            .await?;
        let context = rows
            .into_iter()
            .next()
            .ok_or(ClaimStoreError::Integrity("database clock unavailable"))?;
        if context.isolation != READ_COMMITTED {
            return Err(ClaimStoreError::Integrity(
                "step 17 requires a READ COMMITTED transition transaction",
            ));
        }
        Ok(context)
    }

    /// The live holder of a blocked tuple, only if the caller may read that order.
    async fn visible_holder(
        &self,
        tuple: &ClaimTuple,
        disclosure_scope: &AccessScope,
    ) -> Result<Option<Uuid>, ScopeError> {
        let holders = claim::Entity::find()
            .select_only()
            .column(claim::Column::OrderId)
            .filter(claim::Column::PayerTenantId.eq(tuple.payer_tenant_id))
            .filter(claim::Column::ResourceTenantId.eq(tuple.resource_tenant_id))
            .filter(claim::Column::OverlapScopeKey.eq(tuple.overlap_scope_key.as_str()))
            .filter(claim::Column::ReleasedAt.is_null())
            .into_query();
        let holder = order::Entity::find()
            .filter(order::Column::OrderId.in_subquery(holders))
            .filter(order::Column::OrderId.ne(self.row.order_id))
            .secure()
            .scope_with(disclosure_scope)
            .one(self.tx)
            .await?;
        Ok(holder.map(|o| o.order_id))
    }

    /// Conflict-safe single-tuple insert. `Some` is the claim ID the database returned for
    /// this attempt's own insert; `None` is a live competitor on the same tuple. Private: the
    /// only reservation instant is the database clock read inside this transaction, and the
    /// row is built here from the authorized proposal, never supplied by a caller.
    async fn offer_tuple(
        &self,
        authority: &ProposalAuthority<'_>,
        tuple: &ClaimTuple,
        claimed_at: time::OffsetDateTime,
    ) -> Result<Option<Uuid>, ScopeError> {
        let proposed = authority.proposed;
        if tuple.payer_tenant_id != proposed.payer_tenant_id
            || tuple.resource_tenant_id != self.row.resource_tenant_id
        {
            return Err(ScopeError::Denied(
                "claim is outside the authorized proposed arrangement",
            ));
        }
        let row = claim::Model {
            claim_id: Uuid::new_v4(),
            payer_tenant_id: tuple.payer_tenant_id,
            resource_tenant_id: tuple.resource_tenant_id,
            overlap_scope_key: tuple.overlap_scope_key.as_str().to_owned(),
            order_id: self.row.order_id,
            version: authority.proposed_version,
            claimed_at,
            released_at: None,
        };
        let active = row.clone().into_active_model();
        let result = claim::Entity::insert(active.clone())
            .secure()
            .scope_with_model(&self.child_scope(), &active)?
            .on_conflict_raw(
                OnConflict::columns([
                    claim::Column::PayerTenantId,
                    claim::Column::ResourceTenantId,
                    claim::Column::OverlapScopeKey,
                ])
                .target_and_where(claim::Column::ReleasedAt.is_null())
                .do_nothing()
                .to_owned(),
            )
            .exec(self.tx)
            .await;
        match result {
            // The returned key, not the offered one, is what a shortfall release targets.
            Ok(inserted) if inserted.last_insert_id == row.claim_id => Ok(Some(row.claim_id)),
            Ok(_) => Err(ScopeError::Invalid(
                "inserted claim ID differs from the offer",
            )),
            Err(ScopeError::Db(DbErr::RecordNotInserted)) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn held_claim(row: claim::Model) -> Result<HeldClaim, ClaimStoreError> {
    Ok(HeldClaim {
        claim_id: row.claim_id,
        tuple: ClaimTuple {
            payer_tenant_id: row.payer_tenant_id,
            resource_tenant_id: row.resource_tenant_id,
            overlap_scope_key: OverlapScopeKey::try_from(row.overlap_scope_key)?,
        },
    })
}

//! `ChainVerifierJob` — daily re-walk of every tenant's tamper-evidence hash
//! chain (Slice 6, design §4.2).
//!
//! For each tenant that has a `chain_state` tip, the job walks the journal from
//! the tip back to genesis following the `prev_entry_id` / `prev_period_id`
//! back-pointers. For each entry it (a) reconstructs the canonical
//! [`NewEntry`] + `Vec<NewLine>` from the stored row, recomputes its `row_hash`
//! over the SAME encoder the in-posting seal uses
//! ([`crate::domain::chain::chain_row_hash`]), and compares it to the STORED
//! `row_hash`; and (b) verifies the back-link — the entry's stored `prev_hash`
//! must equal the older (parent) entry's stored `row_hash`, and a genesis entry
//! (`prev_entry_id IS NULL`) must carry
//! [`crate::domain::chain::genesis_prev_hash`].
//!
//! **On the first mismatch for a tenant** the job freezes the tenant tenant-wide
//! (`scope_freeze`, `period_id = 'ALL'`) on its own transaction, then emits a
//! Critical `TAMPER_VERIFY_FAILED` invariant alarm out-of-band — so the ledger
//! STOPS accepting writes for that tenant until an operator clears the freeze.
//! It then continues to the next tenant. A detected tamper is reported via the
//! freeze + alarm, NOT via an `Err` (mirrors how [`crate::infra::jobs::tieout`]
//! reports variances via alarms, not `Err`). A clean chain ⇒ no freeze, no
//! alarm.
//!
//! **Architecture:** like the tie-out job this is a **system-context,
//! cross-tenant** job. The all-tenants enumeration + the per-entry read-backs
//! run under the sanctioned all-tenants system scope
//! ([`AccessScope::allow_all`]); the freeze writes under
//! [`AccessScope::for_tenant`].

use std::collections::HashSet;
use std::sync::Arc;

use bss_ledger_sdk::{AccountClass, MappingStatus, Side, SourceDocType};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, QuerySelect};
use toolkit_db::secure::{AccessScope, SecureEntityExt, TxConfig};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::chain::{chain_row_hash, genesis_prev_hash};
use crate::domain::model::{NewEntry, NewLine};
use crate::domain::ports::metrics::LedgerMetricsPort;
use crate::infra::audit::event_type::AuditEventType;
use crate::infra::audit::store::SecuredAuditStore;
use crate::infra::events::payloads::{AlarmCategory, AlarmSeverity, LedgerInvariantAlarm};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::posting::freeze::ScopeFreezeRepo;
use crate::infra::storage::entity::{chain_state, journal_entry, journal_line};

/// `scope_freeze.scope` kind for a tenant-wide freeze.
const SCOPE_KIND_TENANT: &str = "tenant";
/// `scope_freeze.period_id` sentinel for a tenant-wide freeze.
const PERIOD_ALL: &str = "ALL";
/// `scope_freeze.set_by` actor recorded for a Verifier-raised freeze.
const SET_BY_VERIFIER: &str = "Verifier";

/// Single-column projection of `DISTINCT journal_entry.tenant_id` for the
/// tip-less check — avoids materializing every sealed header just to derive the
/// set of tenants that have sealed entries.
#[derive(Debug, FromQueryResult)]
struct SealedTenantRow {
    tenant_id: Uuid,
}

/// Failure of a chain-verifier run. Mirrors the tie-out job's error shape: it is
/// raised ONLY on an infrastructure fault (DB unreachable / read failure) — a
/// detected tamper is NOT an error (it is reported via a freeze + alarm).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// Storage / connection failure (driver text bounded by the caller).
    #[error("chain-verify db error: {0}")]
    Db(String),
}

/// Why a tenant's chain failed verification (kept internal — ids only, no PII).
#[derive(Debug)]
struct ChainBreak {
    /// The entry at which the break was detected.
    entry_id: Uuid,
    /// Period of that entry.
    period_id: String,
    /// Machine-readable break kind, for the freeze reason + alarm detail.
    kind: BreakKind,
}

/// The class of chain break the Verifier detected.
#[derive(Debug, Clone, Copy)]
enum BreakKind {
    /// `chain_row_hash` recompute disagreed with the stored `row_hash`.
    RowHashMismatch,
    /// The stored `prev_hash` did not equal the parent entry's stored `row_hash`.
    BrokenBackLink,
    /// A genesis entry's stored `prev_hash` was not `genesis_prev_hash(tenant)`.
    GenesisMismatch,
    /// A chained entry had no stored `row_hash` / a non-32-byte hash, or the
    /// back-pointer pair was half-NULL — a structurally corrupt row.
    CorruptRow,
    /// The walk revisited an entry — a cycle in the `prev_entry_id` pointers.
    Cycle,
    /// The tip-to-genesis walk verified clean, but the tenant has MORE sealed
    /// `journal_entry` rows than the walk visited — sealed rows orphaned outside
    /// the walked chain (a `chain_state` tip rolled back to an older entry, or a
    /// forked branch). The walk alone can't see them; the count reconciliation does.
    OrphanBranch,
}

impl BreakKind {
    /// Stable code for the freeze reason + alarm detail.
    fn code(self) -> &'static str {
        match self {
            Self::RowHashMismatch => "ROW_HASH_MISMATCH",
            Self::BrokenBackLink => "BROKEN_BACK_LINK",
            Self::GenesisMismatch => "GENESIS_MISMATCH",
            Self::CorruptRow => "CORRUPT_ROW",
            Self::Cycle => "CYCLE",
            Self::OrphanBranch => "ORPHAN_BRANCH",
        }
    }
}

/// Daily chain-verifier job over every tenant that has a chain.
pub struct ChainVerifierJob {
    db: DBProvider<DbError>,
    publisher: Arc<LedgerEventPublisher>,
    metrics: Arc<dyn LedgerMetricsPort>,
}

impl ChainVerifierJob {
    /// Build the job over one database provider, the event publisher (used
    /// out-of-band to emit the tamper alarm on a separate connection), and the
    /// metrics sink (the §9 tamper-verify / chain-length / freeze metrics).
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        publisher: Arc<LedgerEventPublisher>,
        metrics: Arc<dyn LedgerMetricsPort>,
    ) -> Self {
        Self {
            db,
            publisher,
            metrics,
        }
    }

    /// Re-walk every tenant's chain; freeze + alarm on the first break per
    /// tenant; continue to the next tenant.
    ///
    /// # Errors
    /// [`VerifyError::Db`] only on the up-front tenant *enumeration* failure (DB
    /// unreachable). A per-tenant read failure is logged and skipped — one flaky
    /// tenant must not starve the rest — and a detected tamper is reported via a
    /// freeze + alarm, never as `Err`.
    pub async fn run(&self) -> Result<(), VerifyError> {
        // Cross-tenant enumeration under the all-tenants system scope. Scoped to
        // a block so the connection is released before the per-tenant walks
        // (each walk opens its own connection).
        let chain_tenants: HashSet<Uuid> = {
            let conn = self
                .db
                .conn()
                .map_err(|e| VerifyError::Db(format!("conn: {e}")))?;
            let tips = chain_state::Entity::find()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .all(&conn)
                .await
                .map_err(|e| VerifyError::Db(format!("enumerate chain_state: {e}")))?;
            tips.into_iter().map(|t| t.tenant_id).collect()
        };

        let mut failed = 0_usize;
        for &tenant_id in &chain_tenants {
            match self.verify_tenant(tenant_id).await {
                Ok((walked, brk)) => {
                    // §9: one verifier run per tenant; `failed` splits the
                    // outcome (a detected break). `walked` is the number of
                    // entries the walk visited (the observed chain length).
                    self.metrics.tamper_verify_run(tenant_id, brk.is_some());
                    self.metrics.chain_length(tenant_id, walked);
                    if let Some(brk) = brk {
                        self.freeze_and_alarm(tenant_id, &brk).await;
                    }
                }
                Err(e) => {
                    // Isolate per-tenant infra failures: log and continue so a
                    // single flaky tenant doesn't abort the whole tick.
                    failed += 1;
                    tracing::error!(
                        tenant_id = %tenant_id,
                        error = %e,
                        "bss-ledger: chain-verify failed for tenant; continuing"
                    );
                }
            }
        }
        if failed > 0 {
            tracing::warn!(
                failed,
                "bss-ledger: chain-verify tick completed with per-tenant failures"
            );
        }

        // Defense-in-depth: the enumeration above is tip-keyed, so a
        // tenant whose `chain_state` tip row is missing is never walked. A tenant
        // that HAS sealed `journal_entry` rows but NO tip is therefore invisible —
        // exactly the shape of a tip deleted to hide tampering. Cross-check the
        // journal and alarm on any such tenant.
        self.alarm_tipless_sealed_tenants(&chain_tenants).await;

        Ok(())
    }

    /// Alarm on tenants that have sealed `journal_entry` rows but no
    /// `chain_state` tip. Alarm only — no freeze: without a tip the
    /// chain cannot be walked, and a missing tip is a forensic signal for an
    /// operator, not an automatic write-block. The journal chain is an unkeyed
    /// SHA-256, so this is defense-in-depth, not a strong control. An
    /// enumeration failure is logged and skipped (mirrors the per-tenant
    /// isolation in `run`); it never fails the tick.
    async fn alarm_tipless_sealed_tenants(&self, chain_tenants: &HashSet<Uuid>) {
        let conn = match self.db.conn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "bss-ledger: tip-less check: conn failed; skipping");
                return;
            }
        };
        // Sealed rows only (`row_hash IS NOT NULL`): an unsealed row has no chain
        // claim, so a missing tip for an all-unsealed tenant is not a break.
        // Project to DISTINCT tenant_id rather than materializing every sealed
        // header — this daily job only needs the set of tenants with sealed
        // entries, and the journal is the largest table in the gear. The scoped
        // `project_all` keeps the all-tenants system scope applied to the
        // projection (avoids the full-table `.all()` scan).
        let sealed_tenants = match journal_entry::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(Condition::all().add(journal_entry::Column::RowHash.is_not_null()))
            .project_all(&conn, |q| {
                q.select_only()
                    .column(journal_entry::Column::TenantId)
                    .distinct()
                    .into_model::<SealedTenantRow>()
            })
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "bss-ledger: tip-less check: enumerate sealed journal failed; skipping"
                );
                return;
            }
        };

        let tipless: HashSet<Uuid> = sealed_tenants
            .into_iter()
            .map(|r| r.tenant_id)
            .filter(|t| !chain_tenants.contains(t))
            .collect();

        for tenant_id in tipless {
            tracing::error!(
                tenant_id = %tenant_id,
                "bss-ledger: tenant has sealed journal entries but NO chain_state tip — \
                 possible tampering (tip deleted); alarming"
            );
            let category = AlarmCategory::TamperVerifyFailed;
            self.metrics.invariant_alarm(
                category.as_str(),
                crate::infra::events::alarm_catalog::severity(category).as_str(),
            );
            let alarm = LedgerInvariantAlarm {
                category: AlarmCategory::TamperVerifyFailed,
                severity: AlarmSeverity::Critical,
                tenant_id,
                scope: format!("tenant:{tenant_id}"),
                code: AlarmCategory::TamperVerifyFailed.as_str().to_owned(),
                detail: format!(
                    "tenant {tenant_id} has sealed journal_entry rows but no chain_state \
                     tip (tip missing/deleted) — chain unverifiable, manual investigation \
                     required"
                ),
                affected: Vec::new(),
            };
            self.publisher
                .emit_invariant_alarm(&SecurityContext::anonymous(), alarm)
                .await;
        }
    }

    /// Walk one tenant's chain from the tip to genesis. Returns
    /// `Ok((walked, None))` for a clean chain and `Ok((walked, Some(break)))` on
    /// the FIRST detected break, where `walked` is the number of entries the walk
    /// visited (the observed chain length, backing the §9 `chain_length` gauge);
    /// `Err` only on an infrastructure read failure.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear walk with inline fail-loud break-returns; flat is clearer than helpers"
    )]
    async fn verify_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<(i64, Option<ChainBreak>), VerifyError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| VerifyError::Db(format!("conn: {e}")))?;
        // Cross-tenant read scope — the job re-walks every tenant's chain (same
        // sanctioned system scope the tie-out enumeration uses).
        let scope = AccessScope::allow_all();

        // Start at the tip. No tip row ⇒ the tenant has no chain (nothing to
        // verify). The enumeration only yields tenants WITH a `chain_state`
        // row, but re-read defensively (the row could vanish between passes).
        let Some(tip) = chain_state::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(chain_state::Column::TenantId.eq(tenant_id)))
            .one(&conn)
            .await
            .map_err(|e| VerifyError::Db(format!("read chain_state tip: {e}")))?
        else {
            return Ok((0, None));
        };

        let genesis = genesis_prev_hash(tenant_id);

        // Walk back-pointers from the tip. `expected_child_link` carries the
        // `prev_hash` claimed by the child just processed: the CURRENT entry's
        // stored `row_hash` must equal it (the back-link check). `None` for the
        // newest entry — there is no child above the tip.
        let mut cursor: Option<(Uuid, String)> =
            Some((tip.last_entry_id, tip.last_period_id.clone()));
        let mut expected_child_link: Option<Vec<u8>> = None;
        // Cycle guard: a corrupt `prev_entry_id` could otherwise loop forever.
        let mut seen: HashSet<(Uuid, String)> = HashSet::new();

        // The walk evaluates to the FIRST detected break (or `None` for a clean
        // chain); `seen.len()` is then the observed chain length (§9
        // `chain_length`). A labeled block keeps the inline fail-loud returns
        // while still letting us read the visited count out of `seen`.
        let break_found: Option<ChainBreak> = 'walk: loop {
            let Some((entry_id, period_id)) = cursor else {
                break 'walk None;
            };
            if !seen.insert((entry_id, period_id.clone())) {
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::Cycle,
                });
            }

            // Load the header (carries the chain columns) + its lines.
            let Some(header) = journal_entry::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(journal_entry::Column::EntryId.eq(entry_id))
                        .add(journal_entry::Column::TenantId.eq(tenant_id))
                        .add(journal_entry::Column::PeriodId.eq(period_id.clone())),
                )
                .one(&conn)
                .await
                .map_err(|e| VerifyError::Db(format!("read journal_entry: {e}")))?
            else {
                // A back-pointer that names a missing entry is a broken chain.
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::CorruptRow,
                });
            };

            let line_rows = journal_line::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(journal_line::Column::EntryId.eq(entry_id))
                        .add(journal_line::Column::TenantId.eq(tenant_id))
                        .add(journal_line::Column::PeriodId.eq(period_id.clone())),
                )
                .all(&conn)
                .await
                .map_err(|e| VerifyError::Db(format!("read journal_line: {e}")))?;

            // A chained entry must be sealed (non-NULL `row_hash`/`prev_hash`,
            // each 32 bytes). A structurally corrupt seal is fail-loud.
            let (Some(stored_row_hash), Some(stored_prev_hash)) =
                (header.row_hash.clone(), header.prev_hash.clone())
            else {
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::CorruptRow,
                });
            };
            let Ok(prev_hash_arr) = <[u8; 32]>::try_from(stored_prev_hash.as_slice()) else {
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::CorruptRow,
                });
            };

            // (a) Recompute the row_hash over the canonical encoder (DRY: the
            // SAME `chain_row_hash` the in-posting seal uses) linked to the
            // entry's own stored `prev_hash`, and compare to the stored value.
            let Some((new_entry, new_lines)) = reconstruct(&header, &line_rows) else {
                // A stored enum literal no longer parses — a corrupt row.
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::CorruptRow,
                });
            };
            let recomputed = chain_row_hash(&new_entry, &new_lines, &prev_hash_arr);
            if recomputed.as_slice() != stored_row_hash.as_slice() {
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::RowHashMismatch,
                });
            }

            // (b) Back-link: this entry's stored `row_hash` must equal the
            // `prev_hash` the child below it claimed for its parent.
            if let Some(child_link) = &expected_child_link
                && child_link.as_slice() != stored_row_hash.as_slice()
            {
                break 'walk Some(ChainBreak {
                    entry_id,
                    period_id,
                    kind: BreakKind::BrokenBackLink,
                });
            }

            // Step to the parent. A non-genesis entry names its parent; a
            // genesis entry (`prev_entry_id IS NULL`) must seed
            // `genesis_prev_hash`. A half-NULL back-pointer pair is corrupt.
            match (header.prev_entry_id, header.prev_period_id.clone()) {
                (Some(parent_id), Some(parent_period)) => {
                    // The parent's stored `row_hash` must equal this entry's
                    // stored `prev_hash`.
                    expected_child_link = Some(stored_prev_hash);
                    cursor = Some((parent_id, parent_period));
                }
                (None, None) => {
                    // (c) Genesis: stored prev_hash must be the tenant seed.
                    if prev_hash_arr != genesis {
                        break 'walk Some(ChainBreak {
                            entry_id,
                            period_id,
                            kind: BreakKind::GenesisMismatch,
                        });
                    }
                    cursor = None;
                }
                _ => {
                    break 'walk Some(ChainBreak {
                        entry_id,
                        period_id,
                        kind: BreakKind::CorruptRow,
                    });
                }
            }
        };

        // `seen` holds every entry the walk visited (the chain length). Saturate
        // the `usize -> i64` narrowing rather than wrap (a chain longer than
        // `i64::MAX` is impossible in practice).
        let walked = i64::try_from(seen.len()).unwrap_or(i64::MAX);

        // Z3-1: orphan / tip-rollback reconciliation. The walk starts at the
        // `chain_state` tip — a MUTABLE cache with no append-only guard — and
        // only descends to genesis. A writer who redirects the tip to an OLDER
        // entry (rollback) or forks a branch hides every sealed row above/outside
        // the walked set: the tip-to-genesis walk still verifies clean. Reconcile
        // the visited count against the total sealed rows for the tenant; a
        // surplus means orphaned sealed rows the walk never reached, so freeze.
        // Only meaningful on an otherwise-clean walk — a mid-walk break already
        // freezes, and a broken walk's count is expected to be short. (Cheap
        // COUNT, sibling of the tip-deletion check. The whole-chain
        // re-sign attack is the separate, deferred D6 limitation.)
        if break_found.is_none() {
            let total_sealed = journal_entry::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(journal_entry::Column::TenantId.eq(tenant_id))
                        .add(journal_entry::Column::RowHash.is_not_null()),
                )
                .count(&conn)
                .await
                .map_err(|e| VerifyError::Db(format!("count sealed journal_entry: {e}")))?;
            let walked_count = u64::try_from(seen.len()).unwrap_or(u64::MAX);
            if total_sealed != walked_count {
                // The walk started from `tip`; `total_sealed` was counted by a
                // later statement on the same (READ COMMITTED) connection — no
                // single snapshot spans the two. A post committing in between
                // advances the tip and inflates the count past the walked set: a
                // false orphan that would tenant-wide-freeze a healthy tenant.
                // The tip's `last_seq` is monotonic, so re-read it — if it
                // advanced since the walk began, the surplus is a concurrent
                // post, not an orphan; defer to the next pass (which re-walks
                // from the new tip) instead of freezing.
                let current_tip = chain_state::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .filter(Condition::all().add(chain_state::Column::TenantId.eq(tenant_id)))
                    .one(&conn)
                    .await
                    .map_err(|e| VerifyError::Db(format!("re-read chain_state tip: {e}")))?;
                let tip_advanced = current_tip.as_ref().is_none_or(|t| {
                    t.last_seq != tip.last_seq || t.last_entry_id != tip.last_entry_id
                });
                if tip_advanced {
                    tracing::debug!(
                        tenant_id = %tenant_id,
                        walked = walked_count,
                        total_sealed,
                        "bss-ledger: chain-verify orphan check raced a concurrent post \
                         (tip advanced mid-walk); deferring to the next pass"
                    );
                } else {
                    tracing::error!(
                        tenant_id = %tenant_id,
                        walked = walked_count,
                        total_sealed,
                        "bss-ledger: chain-verify orphan check — sealed rows exist outside the \
                         tip-to-genesis walk (tip rollback or forked branch); freezing"
                    );
                    return Ok((
                        walked,
                        Some(ChainBreak {
                            entry_id: tip.last_entry_id,
                            period_id: tip.last_period_id.clone(),
                            kind: BreakKind::OrphanBranch,
                        }),
                    ));
                }
            }
        }

        Ok((walked, break_found))
    }

    /// Freeze the tenant tenant-wide on its own transaction, then emit the
    /// Critical `TAMPER_VERIFY_FAILED` alarm out-of-band. Fire-and-forget: a
    /// freeze failure is logged (the alarm still fires so an operator is paged).
    async fn freeze_and_alarm(&self, tenant_id: Uuid, brk: &ChainBreak) {
        let reason = format!(
            "chain verification failed ({}) at entry {} period {}",
            brk.kind.code(),
            brk.entry_id,
            brk.period_id,
        );
        tracing::error!(
            tenant_id = %tenant_id,
            entry_id = %brk.entry_id,
            period_id = %brk.period_id,
            kind = brk.kind.code(),
            "bss-ledger: chain verification failed; freezing tenant tenant-wide"
        );

        // Freeze on a fresh transaction (the walk above used read-only
        // connections), with a bounded retry: the freeze is the ACTUAL
        // write-path protection (`TamperFreezeGuard` rejects posts to a frozen
        // scope), so a transient DB fault must not silently leave the tenant
        // writable. The alarm below is only notification.
        let frozen = self.try_freeze_with_retry(tenant_id, &reason).await;

        // §9 metrics: make the tamper freeze observable. The alarm counter is the
        // shared `ledger_alarm_total{category,severity}` rollup; the
        // category token is the alarm's wire `as_str()` (same token the event
        // carries); the severity is `AlarmCategory::severity()` mapped to its
        // `&str` wire code.
        let category = AlarmCategory::TamperVerifyFailed;
        self.metrics.invariant_alarm(
            category.as_str(),
            crate::infra::events::alarm_catalog::severity(category).as_str(),
        );
        // The freeze gauge marks one active tenant-wide freeze — but ONLY when the
        // write actually committed. A failed freeze must NOT report the tenant as
        // frozen: the gauge would lie while posts keep flowing.
        if frozen {
            self.metrics.scope_freeze_active(tenant_id, 1);
        }

        // Out-of-band alarm on the publisher's own connection (mirrors the
        // tie-out job; `SecurityContext::anonymous()` is the same system ctx).
        // When the freeze write failed the tenant is NOT protected and still
        // accepting writes, so escalate loudly and flag it in the (id-only, no
        // PII) alarm detail — an operator must freeze it by hand.
        let detail = if frozen {
            reason
        } else {
            tracing::error!(
                tenant_id = %tenant_id,
                "bss-ledger: chain-verify freeze write FAILED after retries; tenant is \
                 NOT frozen and still accepting writes — manual freeze required"
            );
            format!(
                "{reason}; FREEZE WRITE FAILED — tenant NOT frozen, manual intervention required"
            )
        };
        let alarm = LedgerInvariantAlarm {
            category: AlarmCategory::TamperVerifyFailed,
            severity: AlarmSeverity::Critical,
            tenant_id,
            scope: format!("tenant:{tenant_id}"),
            code: AlarmCategory::TamperVerifyFailed.as_str().to_owned(),
            // Internal diagnostic — ids + break-kind only, no PII.
            detail,
            affected: Vec::new(),
        };
        self.publisher
            .emit_invariant_alarm(&SecurityContext::anonymous(), alarm)
            .await;
    }

    /// Write the tenant-wide `scope_freeze` row, retrying a transient DB fault a
    /// bounded number of times. Returns `true` once the freeze is committed and
    /// `false` if every attempt failed (the caller then escalates — the tenant is
    /// left writable). Re-setting an existing freeze is a no-op inside
    /// [`ScopeFreezeRepo::set`], so a retry after a partial failure is safe.
    async fn try_freeze_with_retry(&self, tenant_id: Uuid, reason: &str) -> bool {
        /// Total freeze-write attempts before giving up and escalating.
        const MAX_ATTEMPTS: u32 = 3;

        for attempt in 1..=MAX_ATTEMPTS {
            // A fresh owned scope + reason per attempt move into the `'static`
            // `move` closure (mirrors `period_open`).
            let freeze_scope = AccessScope::for_tenant(tenant_id);
            let reason_for_txn = reason.to_owned();
            // SERIALIZABLE: the §5.2 freeze-set-clear audit record is appended on
            // the tenant's audit chain in this same txn, and the audit-chain
            // append rests on SSI for its lockless tip read/advance (mirrors the
            // erasure / cross-tenant audit writers). A bare default-isolation txn
            // could fork the audit chain under a concurrent appender.
            let result = self
                .db
                .transaction_with_config(TxConfig::serializable(), move |txn| {
                    Box::pin(async move {
                        let newly_frozen = ScopeFreezeRepo::new()
                            .set(
                                txn,
                                &freeze_scope,
                                tenant_id,
                                SCOPE_KIND_TENANT,
                                PERIOD_ALL,
                                &reason_for_txn,
                                SET_BY_VERIFIER,
                            )
                            .await?;
                        // §5.2: a freeze writes a tamper-evident `freeze-set-clear`
                        // secured-audit record in the SAME txn — but only on a real
                        // transition, so the daily re-freeze of an already-frozen
                        // tenant (set = no-op) does not duplicate the record.
                        if newly_frozen {
                            let before_after = serde_json::json!({
                                "action": "set",
                                "scope": SCOPE_KIND_TENANT,
                                "period_id": PERIOD_ALL,
                                "reason": reason_for_txn,
                            });
                            SecuredAuditStore::new()
                                .append(
                                    txn,
                                    &freeze_scope,
                                    tenant_id,
                                    AuditEventType::FreezeSetClear,
                                    Some(SET_BY_VERIFIER),
                                    None,
                                    &before_after,
                                    None,
                                    None,
                                )
                                .await?;
                        }
                        Ok(())
                    })
                })
                .await;
            match result {
                Ok(()) => return true,
                Err(e) => tracing::error!(
                    tenant_id = %tenant_id,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    error = %e,
                    "bss-ledger: chain-verify freeze write attempt failed"
                ),
            }
        }
        false
    }
}

/// Rebuild the canonical [`NewEntry`] + `Vec<NewLine>` from a stored header +
/// its line rows, parsing the stored string enums back into their SDK types and
/// narrowing `currency_scale` from the persisted `i16` to the `u8` the encoder
/// expects. Returns `None` if any stored enum literal no longer parses (a
/// corrupt row — caught as a chain break by the caller).
///
/// `correlation_id` and `rounding_evidence` are excluded from the chain hash
/// (see [`crate::domain::chain`]), so any value reproduces the same `row_hash`;
/// they are carried through verbatim for completeness.
fn reconstruct(
    header: &journal_entry::Model,
    line_rows: &[journal_line::Model],
) -> Option<(NewEntry, Vec<NewLine>)> {
    let entry = NewEntry {
        entry_id: header.entry_id,
        tenant_id: header.tenant_id,
        legal_entity_id: header.legal_entity_id,
        period_id: header.period_id.clone(),
        entry_currency: header.entry_currency.clone(),
        source_doc_type: header.source_doc_type.parse::<SourceDocType>().ok()?,
        source_business_id: header.source_business_id.clone(),
        reverses_entry_id: header.reverses_entry_id,
        reverses_period_id: header.reverses_period_id.clone(),
        posted_at_utc: header.posted_at_utc,
        effective_at: header.effective_at,
        origin: header.origin.clone(),
        posted_by_actor_id: header.posted_by_actor_id,
        correlation_id: header.correlation_id,
        rounding_evidence: header.rounding_evidence.clone(),
        // Entry-level field (Slice 5) is not part of the chain hash and is not
        // re-inserted by the verifier (it rebuilds for hash recomputation only);
        // the per-line rate_snapshot_ref lives on the line rows.
        rate_snapshot_ref: None,
    };

    let mut lines = Vec::with_capacity(line_rows.len());
    for row in line_rows {
        lines.push(NewLine {
            line_id: row.line_id,
            payer_tenant_id: row.payer_tenant_id,
            seller_tenant_id: row.seller_tenant_id,
            resource_tenant_id: row.resource_tenant_id,
            account_id: row.account_id,
            account_class: row.account_class.parse::<AccountClass>().ok()?,
            gl_code: row.gl_code.clone(),
            side: row.side.parse::<Side>().ok()?,
            amount_minor: row.amount_minor,
            currency: row.currency.clone(),
            // `journal_line.currency_scale` is persisted as `i16`; the encoder
            // takes `u8`. A scale outside `0..=255` is a corrupt row.
            currency_scale: u8::try_from(row.currency_scale).ok()?,
            invoice_id: row.invoice_id.clone(),
            due_date: row.due_date,
            revenue_stream: row.revenue_stream.clone(),
            mapping_status: row.mapping_status.parse::<MappingStatus>().ok()?,
            functional_amount_minor: row.functional_amount_minor,
            functional_currency: row.functional_currency.clone(),
            tax_jurisdiction: row.tax_jurisdiction.clone(),
            tax_filing_period: row.tax_filing_period.clone(),
            tax_rate_ref: row.tax_rate_ref.clone(),
            legal_entity_id: row.legal_entity_id,
            invoice_item_ref: row.invoice_item_ref.clone(),
            sku_or_plan_ref: row.sku_or_plan_ref.clone(),
            price_id: row.price_id.clone(),
            pricing_snapshot_ref: row.pricing_snapshot_ref.clone(),
            po_allocation_group: row.po_allocation_group.clone(),
            credit_grant_event_type: row.credit_grant_event_type.clone(),
            ar_status: row.ar_status.clone(),
        });
    }

    Some((entry, lines))
}

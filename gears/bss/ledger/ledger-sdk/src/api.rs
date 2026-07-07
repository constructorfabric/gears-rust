//! The in-process data-access API handlers call through `ClientHub`. P3
//! exposes the post entry point and an account-balance read; reversal,
//! history, and counter-deltas arrive in later slices.

use uuid::Uuid;

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::close::CloseOutcome;
use crate::posting::{
    AllocateOutcome, AllocatePayment, AllocationView, ArInvoiceBalanceView, BalanceView,
    ChangeRecognitionSchedule, CreditApplication, CreditApplicationApplied, DisputeOutcome,
    EntryView, LineView, ODataQuery, Page, PostEntry, PostingRef, RecognitionRunOutcome,
    RecognitionScheduleList, RecognitionScheduleView, RecordDisputePhase, ReturnPayment,
    RevenueDisaggregation, RevenueDisaggregationQuery, ScheduleChangeRef, SettlePayment,
    TriggerRecognitionRun, UnallocatedView,
};
use crate::provisioning::{AccountInfo, ProvisionOutcome, ProvisionRequest};

#[async_trait::async_trait]
pub trait LedgerClientV1: Send + Sync {
    /// Post a balanced entry in one ACID transaction. Idempotent on the
    /// `(tenant, flow, business_id)` key.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` for unbalanced/empty/too-large,
    /// `FailedPrecondition` for period/account/payer-closed or no-negative,
    /// `Aborted` for an idempotency conflict, `PermissionDenied` when the PEP
    /// denies, `Internal` on an infrastructure fault.
    async fn post_balanced_entry(
        &self,
        ctx: &SecurityContext,
        entry: PostEntry,
    ) -> Result<PostingRef, CanonicalError>;

    /// Read the cached normal-side-positive balance for an account, or `None`
    /// if it has never been posted to. The account is single-currency (v1: one
    /// account per currency), so the currency is implied by `account_id`.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn read_account_balance(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        account_id: Uuid,
    ) -> Result<Option<i64>, CanonicalError>;

    /// List the chart of accounts for a tenant — every account's persistent
    /// `account_id` + coordinate, so a caller can resolve the ids it needs to
    /// post / read balances. Cursor-paginated via the canonical `query`
    /// (`$filter` over `account_class` / `currency` / `revenue_stream` /
    /// `lifecycle_state`, `$orderby`, `limit` / `cursor`). Tenant-scoped: the
    /// `$filter` ANDs the caller's authorized-subtree scope, so a target outside
    /// it returns an empty page (SQL-level BOLA, no leak — never replaced by the
    /// filter).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` on a malformed `$filter` /
    /// cursor, `PermissionDenied` when the PEP denies, or `Internal` on a
    /// storage failure.
    async fn list_accounts(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<AccountInfo>, CanonicalError>;

    /// Read a single posted entry with its lines, or `None` if absent. Tenant-
    /// scoped: an `entry_id` owned by a tenant outside the caller's authorized
    /// subtree returns `None` (SQL-level BOLA, no existence leak).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn get_entry(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        entry_id: Uuid,
    ) -> Result<Option<EntryView>, CanonicalError>;

    /// List journal lines for a tenant under the canonical `query`, cursor-
    /// paginated. `$filter` covers `payer_tenant_id` / `account_class` /
    /// `period_id` / `invoice_id`; `$orderby` / `limit` / `cursor` page the
    /// result. Tenant-scoped: the `$filter` ANDs the caller's authorized-subtree
    /// scope (SQL-level BOLA), so rows outside it are never returned and a
    /// foreign filter value can't widen the set.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` on a malformed `$filter` /
    /// cursor, `PermissionDenied` when the PEP denies, or `Internal` on a
    /// storage failure.
    async fn list_lines(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<LineView>, CanonicalError>;

    /// List the account-balance cache rows for a tenant under the canonical
    /// `query` (`$filter` over `account_class` / `currency`, `$orderby`,
    /// `limit` / `cursor`). Tenant-scoped: the `$filter` ANDs the caller's
    /// authorized-subtree scope (SQL-level BOLA), so a target outside it yields
    /// an empty page (no leak, never replaced by the filter).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` on a malformed `$filter` /
    /// cursor, `PermissionDenied` when the PEP denies, or `Internal` on a
    /// storage failure.
    async fn list_balances(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<BalanceView>, CanonicalError>;

    /// List the per-invoice AR-balance cache rows for a tenant, optionally
    /// narrowed to one `payer_tenant_id` (the AR-aging source). Tenant-scoped:
    /// a target outside the caller's authorized subtree yields an empty list
    /// (SQL-level BOLA, no leak).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn list_ar_invoice_balances(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payer_tenant_id: Option<Uuid>,
    ) -> Result<Vec<ArInvoiceBalanceView>, CanonicalError>;

    /// Provision a tenant's ledger: seed its chart of accounts, non-ISO
    /// currency scales, fiscal-calendar config, and initial OPEN period in
    /// one transaction. Idempotent and additive — a re-call leaves existing
    /// rows untouched and reports them as "existing".
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` on a malformed calendar / empty
    /// seed or a non-ISO scale beyond the supported headroom; `PermissionDenied`
    /// when the PEP denies; `Internal` on a storage failure.
    async fn provision(
        &self,
        ctx: &SecurityContext,
        req: ProvisionRequest,
    ) -> Result<ProvisionOutcome, CanonicalError>;

    /// Close a fiscal period (OPEN→CLOSED) after a clean pre-close tie-out.
    /// # Errors
    /// A [`CanonicalError`]: `FailedPrecondition` if the tenant's books don't
    /// tie out or the period is not OPEN; `NotFound` when the period is absent;
    /// `PermissionDenied` when the PEP denies; `Internal` on a storage failure.
    async fn close_period(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        period_id: String,
    ) -> Result<CloseOutcome, CanonicalError>;

    /// Settle a payment (the money-in side): record a received receipt into the
    /// payer's unallocated pool (`CR UNALLOCATED` gross, `DR CASH_CLEARING` net,
    /// `DR PSP_FEE_EXPENSE` fee). Idempotent on `(tenant, payment_id)` — a
    /// re-settle replays the prior post with no new ledger effect. A settlement
    /// records money already received and lands even for a closed payer.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` for an unrepresentable settlement
    /// (`gross < 0`, `fee < 0`, `fee > gross`); `FailedPrecondition` when a
    /// required account class is not provisioned or the period is closed;
    /// `PermissionDenied` when the PEP denies; `Internal` on a storage failure.
    async fn settle_payment(
        &self,
        ctx: &SecurityContext,
        req: SettlePayment,
    ) -> Result<PostingRef, CanonicalError>;

    /// Record a settlement return (the reversal of a money-in): claw a
    /// previously-settled receipt back out of the payer's unallocated pool
    /// (`DR UNALLOCATED` / `CR CASH_CLEARING`) and decrement the original
    /// payment's `settled_minor`. Idempotent on `(tenant, psp_return_id)` — a
    /// re-post replays the prior entry with no new ledger effect. Records money
    /// already moved and lands even for a closed payer.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` for a non-positive amount;
    /// `FailedPrecondition` when the return exceeds the still-returnable settled
    /// amount (`SETTLEMENT_RETURN_OVER_ALLOCATED`), a required account class is
    /// not provisioned, or the period is closed; `PermissionDenied` when the PEP
    /// denies; `Internal` on a storage failure.
    async fn return_payment(
        &self,
        ctx: &SecurityContext,
        req: ReturnPayment,
    ) -> Result<PostingRef, CanonicalError>;

    /// Record a chargeback dispute phase (the dispute state machine, §4.5). The
    /// LEDGER chooses the variant at `opened` from `funds_at_open` (card rails
    /// withheld the cash ⇒ cash-hold `DR DISPUTE_HOLD / CR CASH_CLEARING`;
    /// invoice/ACH did not move it ⇒ AR-reclass `ACTIVE → DISPUTED`,
    /// AR-class-neutral) and the `won`/`lost` outcomes branch on the recorded
    /// variant. Idempotent on `(tenant, CHARGEBACK, "dispute_id:cycle:phase")` —
    /// a re-post replays the prior entry. A dispute records a card-network / bank
    /// event and lands even for a closed payer.
    ///
    /// Returns [`DisputeOutcome`]: when the phase can post (it IS the `opened`,
    /// or its `opened` cycle exists) it yields `Recorded` (the posting handle).
    /// When a `won`/`lost` arrives BEFORE its `opened` (§4.7 out-of-order) the
    /// request is instead durably **queued** for a later drain and yields
    /// `Queued` (the queue key + `queued_at`) — the REST surface renders this as
    /// HTTP 202 `dispute-phase-queued`, NOT a rejection. The queued effect is
    /// applied once the `opened` lands (by the periodic drain); a replay during
    /// the queued window returns the same `Queued` handle.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` for a non-positive amount, an
    /// unknown `phase` / `funds_at_open`, or a missing AR-reclass `invoice_id`;
    /// `FailedPrecondition` when the phase is not a legal transition from the
    /// dispute's current state (`INVALID_DISPUTE_PHASE`), the clawback exceeds the
    /// settled amount (`CHARGEBACK_EXCEEDS_SETTLED`), a `lost` lands on an
    /// already-refunded payment (`CHARGEBACK_ON_REFUNDED`), a required account
    /// class is not provisioned, or the period is closed; `PermissionDenied`
    /// when the PEP denies; `Internal` on a storage failure. (A `won`/`lost`
    /// before its `opened` is no longer an error — it queues; see above.)
    async fn record_dispute_phase(
        &self,
        ctx: &SecurityContext,
        req: RecordDisputePhase,
    ) -> Result<DisputeOutcome, CanonicalError>;

    /// Allocate a payment's unallocated pool to the payer's open receivables (the
    /// money-out side): drains the pool (`DR UNALLOCATED`) into AR (`CR AR` per
    /// invoice). By default the split is decided by the tenant's precedence
    /// policy; a caller-computed [`AllocatePayment::splits`] (Mode B) instead
    /// supplies the per-invoice shares, which are validated against the open
    /// receivables. Idempotent on `(tenant, allocation_id)`.
    ///
    /// Returns [`AllocateOutcome`]: when the payment is already settled the
    /// allocation posts inline and yields `Applied` (the posting handle + the
    /// per-invoice splits). When the payment is NOT yet settled the request is
    /// instead durably **queued** for a later drain (§4.7
    /// allocation-before-settlement) and yields `Queued` (the queue key +
    /// `queued_at`) — the REST surface renders this as HTTP 202
    /// `allocation-queued`, NOT a rejection. The queued effect is applied once
    /// the settlement lands (the drain is deferred to a later slice); a replay
    /// during the queued window returns the same `Queued` handle.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` when there is no open AR to
    /// allocate, an over-cap allocation exceeds what was settled
    /// (`ALLOCATION_EXCEEDS_SETTLED`), the candidate set is too large
    /// (`ALLOCATION_TOO_LARGE`), the currency mismatches the settlement
    /// (`ALLOCATION_CURRENCY_MISMATCH`), or a caller-computed split is invalid
    /// (`ALLOCATION_SPLIT_INVALID`); `FailedPrecondition` on a closed period;
    /// `PermissionDenied` when the PEP denies; `Internal` on a storage failure.
    /// (An unsettled payment is no longer an error — it queues; see above.)
    async fn allocate_payment(
        &self,
        ctx: &SecurityContext,
        req: AllocatePayment,
    ) -> Result<AllocateOutcome, CanonicalError>;

    /// List a payment's recorded allocations (the per-invoice splits applied to
    /// it). Tenant-scoped: a `payment_id` owned by a tenant outside the caller's
    /// authorized subtree yields an empty list (SQL-level BOLA, no existence
    /// leak).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn list_payment_allocations(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payment_id: String,
    ) -> Result<Vec<AllocationView>, CanonicalError>;

    /// Read the payer's unallocated pool balance for a currency — the still-
    /// undrained portion of settled receipts. Tenant-scoped: a payer outside the
    /// caller's authorized subtree yields a zero balance (SQL-level BOLA).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn read_unallocated(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payer_tenant_id: Uuid,
        currency: String,
    ) -> Result<UnallocatedView, CanonicalError>;

    /// Operate a tenant's reusable-credit wallet (the wallet surface,
    /// architecture §5.2) — ONE entry point, two kinds. A `Grant` parks
    /// `amount_minor` of the payer's unallocated pool into the wallet sub-grain
    /// (`DR UNALLOCATED` / `CR REUSABLE_CREDIT`), capped at the live pool. An
    /// `Apply` spends the wallet against the named open receivables oldest-grant-
    /// first (`N×DR REUSABLE_CREDIT` / `M×CR AR`), capped on both the receivable
    /// side (open AR) and the wallet side (spendable sub-grains). Idempotent on
    /// `(tenant, CREDIT_APPLY, credit_application_id)`. Returns the posting handle
    /// plus — for an apply — the wallet draw-downs and the per-invoice shares.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` when a grant exceeds the payer's
    /// unallocated pool (`GRANT_EXCEEDS_UNALLOCATED`), an apply target names an
    /// unknown/closed invoice or over-applies the open AR (`CREDIT_EXCEEDS_OPEN_AR`),
    /// or the payer's spendable wallet cannot cover the apply total
    /// (`CREDIT_EXCEEDS_WALLET`); `FailedPrecondition` on a closed period or an
    /// unprovisioned account class; `PermissionDenied` when the PEP denies;
    /// `Internal` on a storage failure.
    async fn post_credit_application(
        &self,
        ctx: &SecurityContext,
        req: CreditApplication,
    ) -> Result<CreditApplicationApplied, CanonicalError>;

    /// Trigger an ASC 606 recognition run for one fiscal period (the S6
    /// release, design §4.3 / §5): release the period's due `PENDING`
    /// recognition segments, each as one balanced `DR CONTRACT_LIABILITY /
    /// CR REVENUE` entry. The trigger is idempotent on
    /// `(tenant, period_id, run_id)` — a replay of the same `run_id` returns the
    /// prior run reference without starting a second run — and a per-`(tenant,
    /// period_id)` single-active-run guard serializes overlapping runs; each
    /// released segment is independently at-most-once via the per-segment
    /// `RECOGNITION` gate, so overlapping different `run_id`s can never
    /// double-credit a segment.
    ///
    /// Returns [`RecognitionRunOutcome`]: when every due segment released in
    /// period order it yields `Ran` (the run reference + the release tally).
    /// When a due segment's lower-period predecessor was not yet `DONE` the
    /// segment is parked `QUEUED` (design §4.6 ordering) and the run yields
    /// `Queued` — the REST surface renders this as HTTP 202
    /// `recognition-period-queued`, NOT a rejection. A later run drains the
    /// `QUEUED` segments once their predecessors commit.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `Aborted` when a per-schedule over-recognition cap
    /// CHECK rejects a release (`OVER_RECOGNITION`); `FailedPrecondition` when a
    /// target period is closed and no current open period exists, or a required
    /// account class is not provisioned; `PermissionDenied` when the PEP denies;
    /// `Internal` on a storage failure.
    async fn trigger_recognition_run(
        &self,
        ctx: &SecurityContext,
        req: TriggerRecognitionRun,
    ) -> Result<RecognitionRunOutcome, CanonicalError>;

    /// Disaggregate recognized ASC 606 revenue by stream (design §3.5 / §4.5).
    /// The source is recognized revenue = the **DONE** recognition segments (each
    /// posted a `DR CONTRACT_LIABILITY / CR REVENUE` release), grouped by
    /// `(period_id, revenue_stream)` and summed, ordered by
    /// `(period_id, revenue_stream)`. `query.period_id` `None` ⇒ every period;
    /// `Some(_)` narrows to that period. Tenant-scoped on `query.tenant_id`: a
    /// target outside the caller's authorized subtree yields no entries (SQL-level
    /// BOLA, no leak).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or `Internal`
    /// on a storage failure.
    async fn list_revenue_disaggregation(
        &self,
        ctx: &SecurityContext,
        query: RevenueDisaggregationQuery,
    ) -> Result<RevenueDisaggregation, CanonicalError>;

    /// Change or cancel an ASC 606 recognition schedule (design §3.6 / §4.6, the
    /// modification path). Targets one ACTIVE `(tenant, schedule_id)` schedule.
    /// **First** the upstream modification-accounting `treatment` is gated:
    /// `"prospective"` / `"separate_contract"` proceed; `"catch_up"` or any
    /// unknown value is rejected `MODIFICATION_TREATMENT_REVIEW` with NO state
    /// change (the ledger never silently treats a modification as prospective —
    /// upstream owns the catch-up decision, §3.6). Then:
    ///
    /// - **`"cancel"`** marks the schedule `CANCELLED` (bumping `version`). The
    ///   unreleased deferred remainder stays as `CONTRACT_LIABILITY` (no
    ///   auto-reversal — out of v1 scope); already-recognized segments are
    ///   untouched. The schedule's still-`PENDING`/`QUEUED` segments will no
    ///   longer release (the runner releases ACTIVE schedules only).
    /// - **`"replace"`** marks the old schedule `REPLACED` and mints a NEW
    ///   `schedule_id` (`version = old + 1`) carrying the SAME business-key dims,
    ///   with `total_deferred = old.total_deferred − old.recognized` (the
    ///   REMAINING deferred) re-planned over `new_segments` (PENDING). Prospective:
    ///   already-recognized revenue is NOT unwound and no compensating journal
    ///   entry is posted (the `CONTRACT_LIABILITY` balance already equals the
    ///   remaining deferred; the new schedule re-plans its release).
    ///
    /// Idempotent on `change_id`: a replay returns the prior [`ScheduleChangeRef`]
    /// and mints no second schedule. Emits `billing.ledger.schedule.changed`
    /// in-txn on success.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `InvalidArgument` for an unknown `action`, an empty /
    /// over-long `change_id`, a `replace` whose `new_segments` are missing / sum
    /// to the wrong remaining-deferred total, a `catch_up`/unknown `treatment`
    /// (`MODIFICATION_TREATMENT_REVIEW`), or when no ACTIVE schedule exists for
    /// `(tenant, schedule_id)`; `PermissionDenied` when the PEP denies; `Internal`
    /// on a storage failure.
    async fn change_recognition_schedule(
        &self,
        ctx: &SecurityContext,
        cmd: ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, CanonicalError>;

    /// Read one ASC 606 recognition schedule's lifecycle view (design §3.7 / §4,
    /// the `GET /recognition-schedules/{schedule_id}` surface): the schedule
    /// header (status, version, revenue stream, currency, total-deferred /
    /// recognized-to-date, the originating invoice + item-link anchor, the PO /
    /// subscription / policy refs) plus its segments, ordered by `segment_no`
    /// (1:1 with `period_id`, so also period order). The schedule PK is
    /// `(tenant_id, schedule_id)`. `Ok(None)` ⇒ no such schedule exists for the
    /// `(tenant_id, schedule_id)` pair, OR it lies outside the caller's
    /// authorized subtree — tenant-scoped (SQL-level BOLA), so the two are
    /// indistinguishable (no existence leak; the REST handler renders a 404 in
    /// either case).
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn get_recognition_schedule(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schedule_id: String,
    ) -> Result<Option<RecognitionScheduleView>, CanonicalError>;

    /// List ASC 606 recognition schedules for `tenant_id`, optionally narrowed to
    /// one originating `invoice_id` (`source_invoice_id`) and/or one
    /// `revenue_stream` — the discovery surface for the server-minted
    /// `schedule_id` (`GET /recognition-schedules`, and the post-commit lookup
    /// that echoes a freshly-minted id on invoice-post). Header views only (the
    /// by-id surface carries the segments). Tenant-scoped (SQL-level BOLA):
    /// schedules outside the caller's subtree are silently excluded.
    ///
    /// # Errors
    /// A [`CanonicalError`]: `PermissionDenied` when the PEP denies, or
    /// `Internal` on a storage failure.
    async fn list_recognition_schedules(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        invoice_id: Option<String>,
        revenue_stream: Option<String>,
    ) -> Result<RecognitionScheduleList, CanonicalError>;
}

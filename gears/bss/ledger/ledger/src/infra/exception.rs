//! `ExceptionRouter` — the additive seam that routes a per-slice stub condition to
//! a durable, close-blocking `exception_queue` row (Slice 7 Phase 2, design §4.6).
//!
//! **Additive (decision 5).** Each stub site already emits its alarm / log (and
//! sometimes rejects with a 422); routing is a SECOND effect beside it — an OPEN
//! row the close gate reads — never a replacement. So routing is **fire-and-forget**:
//! a failure to open the row is logged, never propagated, and never fails the money
//! path the stub sits on (the rejection / alarm already happened). The only new
//! behaviour is "this condition now blocks close until resolved / approved".
//!
//! The row is **period-bound to the tenant's current OPEN period** so the close
//! gate's `list_open_in_txn(period)` sees it. A stub rejects the operation (no post
//! commits), so binding to the period that is open *now* — the one a close would
//! certify next — is the correct close-blocking grain.

use std::sync::Arc;

use serde_json::Value as JsonValue;
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::exception::ExceptionType;
use crate::infra::storage::repo::{ExceptionQueueRepo, RecognitionRepo};

/// Routes a stub condition into the durable exception queue, fire-and-forget. Cheap
/// to clone (`DBProvider` is a handle); shared as an `Arc` by the stub-bearing
/// services.
#[derive(Clone)]
pub struct ExceptionRouter {
    db: DBProvider<DbError>,
    /// Reused only for `current_open_period` (a `fiscal_period` read).
    periods: RecognitionRepo,
}

impl ExceptionRouter {
    /// Build the router over one database provider.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        let periods = RecognitionRepo::new(db.clone());
        Self { db, periods }
    }

    /// Wrap the router as a shared handle (the form the services hold).
    #[must_use]
    pub fn shared(db: DBProvider<DbError>) -> Arc<Self> {
        Arc::new(Self::new(db))
    }

    /// Open one OPEN `exception_queue` row for `(tenant, ty, business_ref)`, bound to
    /// the tenant's CURRENT OPEN period (so it blocks the next close). For a stub-site
    /// condition that rejected a present-time operation, the period open *now* is the
    /// correct close-blocking grain. A recon-origin caller that knows the reconciled
    /// period MUST use [`Self::route_for_period`] instead — binding to the current open
    /// period would leave a non-current reconciled period's close gate blind.
    /// Fire-and-forget: any failure is logged, never propagated.
    pub async fn route(
        &self,
        tenant: Uuid,
        ty: ExceptionType,
        business_ref: &str,
        detail: Option<JsonValue>,
    ) {
        self.route_inner(tenant, ty, business_ref, None, detail)
            .await;
    }

    /// Open one OPEN `exception_queue` row bound to an EXPLICIT `period` — the
    /// reconciliation framework's grain, so the close gate for *that* period sees it.
    /// A recon may run for a period that is not the tenant's current open one; binding
    /// to `current_open_period` would leave the reconciled period's close gate blind.
    pub async fn route_for_period(
        &self,
        tenant: Uuid,
        ty: ExceptionType,
        business_ref: &str,
        period: &str,
        detail: Option<JsonValue>,
    ) {
        self.route_inner(tenant, ty, business_ref, Some(period.to_owned()), detail)
            .await;
    }

    /// Shared body: resolve the close-blocking period (an explicit `period_override`
    /// for recon origin, else the tenant's current OPEN period), dedup on
    /// `(tenant, type, business_ref)`, and insert one OPEN row. Fire-and-forget — any
    /// failure is logged and swallowed (the stub's alarm / rejection is the primary
    /// effect; the row must never fail the caller's path).
    async fn route_inner(
        &self,
        tenant: Uuid,
        ty: ExceptionType,
        business_ref: &str,
        period_override: Option<String>,
        detail: Option<JsonValue>,
    ) {
        let scope = AccessScope::for_tenant(tenant);

        // Period-bind: an explicit recon period when given, else the current OPEN period
        // (the stub-rejection grain). A transient resolve failure must not silently
        // downgrade a close-blocking row to a period-less (dashboard-only) one, so retry
        // a few times; only a genuine no-open-period (`Ok(None)`) or exhausted retries
        // leaves the row period-less rather than dropping it.
        let period = if let Some(p) = period_override {
            Some(p)
        } else {
            let mut resolved = None;
            for attempt in 1..=3u8 {
                match self.periods.current_open_period(&scope, tenant).await {
                    Ok(p) => {
                        resolved = p;
                        break;
                    }
                    Err(e) => tracing::warn!(
                        target: "bss-ledger",
                        error = %e,
                        %tenant,
                        attempt,
                        exception_type = ty.as_str(),
                        "exception-route: current-open-period resolve failed (retrying)"
                    ),
                }
            }
            resolved
        };

        let exception_id = Uuid::now_v7();
        let type_token = ty.as_str();
        let business_ref = business_ref.to_owned();
        let result = self
            .db
            .transaction(move |txn| {
                let scope = scope.clone();
                let period = period.clone();
                let business_ref = business_ref.clone();
                let detail = detail.clone();
                Box::pin(async move {
                    // Dedup: skip if an OPEN row already exists for this business key
                    // (a periodic stub re-detects each scan; an inline re-try repeats
                    // the key). The check + open share this txn's snapshot.
                    if ExceptionQueueRepo::exists_open_for_ref(
                        txn,
                        &scope,
                        tenant,
                        type_token,
                        &business_ref,
                    )
                    .await
                    .map_err(|e| DbError::Other(anyhow::anyhow!("dedup exception_queue: {e}")))?
                    {
                        return Ok(());
                    }
                    ExceptionQueueRepo::open(
                        txn,
                        &scope,
                        tenant,
                        exception_id,
                        type_token,
                        &business_ref,
                        period.as_deref(),
                        detail,
                    )
                    .await
                    .map_err(|e| DbError::Other(anyhow::anyhow!("open exception_queue: {e}")))
                })
            })
            .await;

        if let Err(e) = result {
            tracing::warn!(
                target: "bss-ledger",
                error = %e,
                %tenant,
                exception_type = type_token,
                "exception-route: failed to open exception_queue row (alarm already emitted)"
            );
        }
    }
}

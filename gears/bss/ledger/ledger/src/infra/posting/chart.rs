//! `ChartIndex` — a tenant's chart of accounts indexed by
//! `(account_class, currency, revenue_stream)` for fast `account_id`
//! resolution during posting. Shared by the invoice-post orchestrator and the
//! payment orchestrators (settlement / allocation), which all bind freshly
//! built lines (the pure builders emit a nil placeholder `account_id`) to the
//! provisioned chart before posting.
//!
//! Resolution is intentionally key-based (`resolve(class, currency, stream)`)
//! so non-`PostLine` callers (the payment builders) share the same lookup; a
//! thin `PostLine`-based adapter lives at the call site in `invoice_post.rs`.

use std::collections::HashMap;

use bss_ledger_sdk::AccountClass;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::storage::repo::ReferenceRepo;

/// `(account_class, currency, revenue_stream)` → `account_id` index over a
/// tenant's chart, the key the direct-split lines resolve on.
pub struct ChartIndex {
    by_key: HashMap<AccountKey, Uuid>,
}

impl ChartIndex {
    /// Build the index from `(class, currency, stream)` → `account_id` pairs.
    /// `class`/`currency` are the stored chart-row strings; `stream` is `None`
    /// for the stream-less system classes.
    #[must_use]
    pub fn from_rows(
        rows: impl IntoIterator<Item = (String, String, Option<String>, Uuid)>,
    ) -> Self {
        let mut by_key: HashMap<AccountKey, Uuid> = HashMap::new();
        for (account_class, currency, revenue_stream, account_id) in rows {
            by_key.insert(
                AccountKey {
                    account_class,
                    currency,
                    revenue_stream,
                },
                account_id,
            );
        }
        Self { by_key }
    }

    /// Resolve a chart `account_id` from a class + currency + optional stream.
    /// Per-stream classes (`REVENUE` / `CONTRACT_LIABILITY`) key on `stream`;
    /// the system parking / clearing classes (AR, TAX, SUSPENSE, CASH,
    /// UNALLOCATED, …) resolve on `stream = None` regardless of any stream the
    /// originating item carried — an unmapped item routed to SUSPENSE still
    /// bears its source `revenue_stream`, but the SUSPENSE account is a single
    /// stream-less parking row.
    #[must_use]
    pub fn resolve(
        &self,
        class: AccountClass,
        currency: &str,
        stream: Option<&str>,
    ) -> Option<Uuid> {
        let revenue_stream = if class.is_per_stream() {
            stream.map(ToOwned::to_owned)
        } else {
            None
        };
        let key = AccountKey {
            account_class: class.as_str().to_owned(),
            currency: currency.to_owned(),
            revenue_stream,
        };
        self.by_key.get(&key).copied()
    }
}

/// Chart lookup key. `account_class`/`currency` are stored strings (the chart
/// row form); `revenue_stream` mirrors the column (`None` for non-revenue rows).
#[derive(Clone, PartialEq, Eq, Hash)]
struct AccountKey {
    account_class: String,
    currency: String,
    revenue_stream: Option<String>,
}

/// Load the tenant's chart of accounts into a `(class, currency, stream)`
/// lookup. SecureORM-scoped (BOLA): a foreign tenant yields an empty index.
///
/// # Errors
/// [`DomainError::Internal`] on a storage / connection failure.
pub async fn load_chart(
    reference: &ReferenceRepo,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<ChartIndex, DomainError> {
    let rows = reference
        .all_accounts(scope, tenant)
        .await
        .map_err(|e| DomainError::Internal(format!("load chart: {e}")))?;
    Ok(ChartIndex::from_rows(rows.into_iter().map(|r| {
        (r.account_class, r.currency, r.revenue_stream, r.account_id)
    })))
}

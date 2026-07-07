//! PII minimization, erasure, and re-identification (Slice 6 Phase 3 Group 3A,
//! architecture §4.5 / AC #22).
//!
//! Three pieces:
//!
//! - [`PiiMinimizer`] — a PURE scanner that walks a JSON payload and flags the
//!   PRD-prohibited PII categories whose keys appear anywhere in it (customer
//!   name, email, phone, payment-instrument detail, street address). The ledger
//!   stores ledger truth, never raw customer PII; this guard lets a caller prove
//!   a payload carries only internal ids before it is persisted.
//!
//! - [`PayerPiiMap`] — a stateless repo over `bss.payer_pii_map`: one row per
//!   `(tenant, payer_tenant_id)` holding the opaque `pii_ref` (a pointer into
//!   the external PII store) and an `erased` tombstone.
//!
//! - [`ErasureService`] — the GDPR right-to-erasure + forensic
//!   re-identification path. `erase` flips the tombstone and records ONE
//!   `erasure` secured-audit record in one `SERIALIZABLE` transaction (idempotent
//!   — re-erasing an already-tombstoned map still records the audit event but
//!   touches no financial table; erasing a payer with no map row is a 404).
//!   `reidentify` is forensic-gated exactly like the cross-tenant audit read
//!   (`reason` + `reason_code` required, else
//!   [`DomainError::MissingInvestigationReason`]): it records ONE
//!   `re-identification` record BEFORE returning the `pii_ref` (even of a
//!   tombstoned payer — the documented investigator path).
//!
//!   Both forensic records are written onto the **home (actor) tenant's** audit
//!   chain — the same rule [`crate::infra::authz::cross_tenant::CrossTenantGateway`]
//!   follows for `cross-tenant-access`. The map write (tombstone / read) is
//!   scoped to the **data (target) tenant**. For a routine same-tenant call the
//!   two coincide; for a cross-tenant erase/re-identify the actor's own chain
//!   carries the trail of what they did to the other tenant.
//!
//! Neither path touches `journal_entry` / `journal_line`: the financial truth
//! (and its tamper-evidence chain) stays byte-identical across an erasure.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{
    AccessScope, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt, TxConfig,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::domain::ports::metrics::{LedgerMetricsPort, NoopLedgerMetrics};
use crate::infra::audit::event_type::AuditEventType;
use crate::infra::audit::store::SecuredAuditStore;
use crate::infra::posting::service::{business, repo_to_db};
use crate::infra::storage::entity::payer_pii_map;

// ---------------------------------------------------------------------------
// PiiMinimizer — pure prohibited-field scanner.
// ---------------------------------------------------------------------------

/// One PRD-prohibited PII category. The `&'static str` is the stable category
/// label the scanner returns (NOT the matched key — many keys map to one
/// category, e.g. `pan` and `card_number` both map to `payment_instrument`).
mod category {
    /// Customer name (personal / full / customer name).
    pub const NAME: &str = "name";
    /// Email address.
    pub const EMAIL: &str = "email";
    /// Phone / mobile number.
    pub const PHONE: &str = "phone";
    /// Payment-instrument detail (card / PAN / IBAN / bank account number).
    pub const PAYMENT_INSTRUMENT: &str = "payment_instrument";
    /// Street / postal address.
    pub const STREET_ADDRESS: &str = "street_address";
}

/// Map ONE object key (already lower-cased) to its prohibited category, or
/// `None` when the key is benign (an internal id is benign). Matching is by the
/// common spellings the PRD enumerates; an unknown key is treated as benign (the
/// scanner flags only KNOWN PII keys, never guesses).
fn category_for_key(key_lower: &str) -> Option<&'static str> {
    match key_lower {
        // Customer name spellings.
        "name" | "full_name" | "fullname" | "customer_name" | "first_name" | "last_name" => {
            Some(category::NAME)
        }
        // Email spellings.
        "email" | "email_address" | "e_mail" => Some(category::EMAIL),
        // Phone spellings.
        "phone" | "phone_number" | "mobile" | "mobile_number" | "msisdn" | "telephone" => {
            Some(category::PHONE)
        }
        // Payment-instrument spellings (card / PAN / IBAN / bank account).
        "card_number"
        | "cardnumber"
        | "pan"
        | "iban"
        | "account_number"
        | "bank_account_number" => Some(category::PAYMENT_INSTRUMENT),
        // Street / postal address spellings.
        "street" | "address" | "address_line1" | "address_line_1" | "addressline1"
        | "postal_address" | "street_address" => Some(category::STREET_ADDRESS),
        _ => None,
    }
}

/// `true` if `text` contains something shaped like an email address: a
/// whitespace-delimited token (trimmed of surrounding punctuation) with a
/// non-empty local part, an `@`, and a domain whose last dot-segment is a TLD
/// (2+ ASCII letters). Deliberately strict to keep false positives low on
/// legitimate ledger notes.
fn text_has_email(text: &str) -> bool {
    text.split_whitespace().any(|raw| {
        let tok = raw.trim_matches(|c: char| {
            matches!(
                c,
                '.' | ',' | ';' | ':' | '(' | ')' | '<' | '>' | '"' | '\''
            )
        });
        let Some(at) = tok.find('@') else {
            return false;
        };
        let local = &tok[..at];
        let domain = &tok[at + 1..];
        if local.is_empty() {
            return false;
        }
        match domain.rsplit_once('.') {
            Some((host, tld)) => {
                !host.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
            }
            None => false,
        }
    })
}

/// `true` if `text` contains a run of 10+ digits once intra-number separators
/// (space, dash, parentheses, a `+`) are ignored — the shape of a phone
/// (national 10-digit `415-555-2671` or `+CC` international) or a
/// payment-instrument (card / account) number. The 10-digit floor still skips
/// dates (`2026-06-25` — 8 digits) and short order numbers (≤ 9 digits),
/// keeping false positives low.
fn text_has_long_number(text: &str) -> bool {
    /// Digit floor that distinguishes a phone / card / account number from a
    /// date or short reference. A national phone number is exactly 10 digits.
    const MIN_DIGITS: usize = 10;
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= MIN_DIGITS {
                return true;
            }
        } else if !matches!(c, ' ' | '-' | '(' | ')' | '+') {
            // A non-separator breaks the run; separators neither count nor reset.
            run = 0;
        }
    }
    false
}

/// A pure scanner for PRD-prohibited PII (architecture §4.5). Holds no state.
pub struct PiiMinimizer;

impl PiiMinimizer {
    /// Scan `value` (recursively) for object KEYS matching a prohibited PII
    /// category, returning the DISTINCT categories found in stable category
    /// order. A clean payload (internal ids only) returns an empty `Vec`.
    ///
    /// Recursion: an object's keys are tested then its values are descended;
    /// an array's elements are each descended; a scalar (string / number / bool
    /// / null) contributes nothing (only KEYS are matched, never values — a key
    /// is the PII signal, a value could be any opaque string). Both an
    /// `{"customer": {"email": …}}` nesting and a `[{"email": …}]` array element
    /// are flagged.
    #[must_use]
    pub fn prohibited_fields(value: &serde_json::Value) -> Vec<&'static str> {
        // Collect into a fixed-order de-dup set: walk, then return the canonical
        // category order so the result is deterministic regardless of key order.
        const ORDER: &[&str] = &[
            category::NAME,
            category::EMAIL,
            category::PHONE,
            category::PAYMENT_INSTRUMENT,
            category::STREET_ADDRESS,
        ];
        let mut found: Vec<&'static str> = Vec::new();
        Self::walk(value, &mut found);
        ORDER
            .iter()
            .copied()
            .filter(|c| found.contains(c))
            .collect()
    }

    /// Depth-first walk: push the category of every matching object key, then
    /// descend into nested objects / arrays.
    fn walk(value: &serde_json::Value, found: &mut Vec<&'static str>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    if let Some(cat) = category_for_key(&key.to_lowercase())
                        && !found.contains(&cat)
                    {
                        found.push(cat);
                    }
                    Self::walk(child, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    Self::walk(item, found);
                }
            }
            // Scalars carry no key, so they contribute no category.
            _ => {}
        }
    }

    /// Scan `value` recursively for PII embedded in STRING VALUES (not object
    /// keys), returning the DISTINCT categories found in stable order. This
    /// complements [`Self::prohibited_fields`] (which sees only keys): a
    /// free-text field such as a metadata `description` carries its PII in the
    /// value, where the key-based scan is blind.
    ///
    /// Detects the two categories that are reliable to spot in free text — an
    /// email address ([`category::EMAIL`]) and a long digit run, a phone or
    /// payment-instrument number ([`category::PAYMENT_INSTRUMENT`]). Customer
    /// name and street address are not machine-detectable in free text and are
    /// left to the key-based scan.
    #[must_use]
    pub fn prohibited_in_values(value: &serde_json::Value) -> Vec<&'static str> {
        const ORDER: &[&str] = &[category::EMAIL, category::PAYMENT_INSTRUMENT];
        let mut found: Vec<&'static str> = Vec::new();
        Self::walk_values(value, &mut found);
        ORDER
            .iter()
            .copied()
            .filter(|c| found.contains(c))
            .collect()
    }

    /// Depth-first walk that inspects STRING scalars (descending objects /
    /// arrays); object KEYS are ignored here — that is [`Self::walk`]'s job.
    fn walk_values(value: &serde_json::Value, found: &mut Vec<&'static str>) {
        match value {
            serde_json::Value::Object(map) => {
                for child in map.values() {
                    Self::walk_values(child, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    Self::walk_values(item, found);
                }
            }
            serde_json::Value::String(text) => {
                if text_has_email(text) && !found.contains(&category::EMAIL) {
                    found.push(category::EMAIL);
                }
                if text_has_long_number(text) && !found.contains(&category::PAYMENT_INSTRUMENT) {
                    found.push(category::PAYMENT_INSTRUMENT);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// PayerPiiMap — stateless repo over bss.payer_pii_map.
// ---------------------------------------------------------------------------

/// Stateless repo over `bss.payer_pii_map`. Every method runs inside the
/// caller's transaction (mirrors [`SecuredAuditStore`]).
#[derive(Clone, Default)]
pub struct PayerPiiMap;

impl PayerPiiMap {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Upsert the `pii_ref` for `(tenant, payer_tenant_id)`:
    /// `INSERT … ON CONFLICT (tenant_id, payer_tenant_id) DO UPDATE SET pii_ref`.
    /// A fresh row is `erased = false` (the column default); a conflict updates
    /// only `pii_ref` (an existing tombstone is NOT resurrected by an upsert).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage / scope failure.
    pub async fn upsert(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payer_tenant_id: Uuid,
        pii_ref: &str,
    ) -> Result<(), RepoError> {
        let am = payer_pii_map::ActiveModel {
            tenant_id: Set(tenant),
            payer_tenant_id: Set(payer_tenant_id),
            pii_ref: Set(pii_ref.to_owned()),
            erased: Set(false),
        };
        let on_conflict = OnConflict::columns([
            payer_pii_map::Column::TenantId,
            payer_pii_map::Column::PayerTenantId,
        ])
        .update_columns([payer_pii_map::Column::PiiRef])
        .to_owned();

        payer_pii_map::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| RepoError::Db(format!("payer_pii_map upsert scope: {e}")))?
            .on_conflict_raw(on_conflict)
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("payer_pii_map upsert: {e}")))?;
        Ok(())
    }

    /// Read `(pii_ref, erased)` for `(tenant, payer_tenant_id)` under `scope`,
    /// or `None` when no row exists. Takes no row lock.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage / scope failure.
    pub async fn get(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payer_tenant_id: Uuid,
    ) -> Result<Option<(String, bool)>, RepoError> {
        let row = payer_pii_map::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(payer_pii_map::Column::TenantId.eq(tenant))
                    .add(payer_pii_map::Column::PayerTenantId.eq(payer_tenant_id)),
            )
            .one(txn)
            .await
            .map_err(|e| RepoError::Db(format!("payer_pii_map get: {e}")))?;
        Ok(row.map(|r| (r.pii_ref, r.erased)))
    }

    /// Tombstone `(tenant, payer_tenant_id)`: `UPDATE … SET erased = true` WHERE
    /// the PK matches. Returns whether a row was updated (`rows_affected > 0`) —
    /// `false` when no map row exists. Already-tombstoned rows still match (the
    /// UPDATE is a harmless no-op write), so re-erasing returns `true`.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage / scope failure.
    pub async fn tombstone(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payer_tenant_id: Uuid,
    ) -> Result<bool, RepoError> {
        let res = payer_pii_map::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(payer_pii_map::Column::Erased, Expr::value(true))
            .filter(
                Condition::all()
                    .add(payer_pii_map::Column::TenantId.eq(tenant))
                    .add(payer_pii_map::Column::PayerTenantId.eq(payer_tenant_id)),
            )
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("payer_pii_map tombstone: {e}")))?;
        Ok(res.rows_affected > 0)
    }
}

// ---------------------------------------------------------------------------
// ErasureService — erase + forensic re-identify.
// ---------------------------------------------------------------------------

/// The PII erasure + re-identification service. Holds the append-only
/// [`SecuredAuditStore`] + the stateless [`PayerPiiMap`] repo; every write runs
/// in its own `SERIALIZABLE` transaction (mirrors
/// [`crate::infra::annotation::AnnotationService`]).
#[derive(Clone)]
pub struct ErasureService {
    audit: SecuredAuditStore,
    map: PayerPiiMap,
    metrics: Arc<dyn LedgerMetricsPort>,
}

impl Default for ErasureService {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasureService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            audit: SecuredAuditStore::new(),
            map: PayerPiiMap::new(),
            metrics: Arc::new(NoopLedgerMetrics),
        }
    }

    /// Bind the §9 metrics sink (`ledger_erasure_applied_total` /
    /// `ledger_reidentification_total`). Defaults to no-op until wired.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn LedgerMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Erase (GDPR right-to-erasure) the PII map for
    /// `(data_tenant, payer_tenant_id)`: one `SERIALIZABLE` transaction that
    /// tombstones the map (`erased = true`) under the DATA (target) scope and
    /// appends ONE `erasure` secured-audit record onto the HOME (actor) tenant's
    /// audit chain. NO journal table is touched — the financial truth and its
    /// chain stay byte-identical.
    ///
    /// `home_scope`/`home_tenant` own the forensic record + its chain (mirrors
    /// [`crate::infra::authz::cross_tenant::CrossTenantGateway`]);
    /// `data_scope`/`data_tenant` scope the map write. For a routine same-tenant
    /// erasure the two coincide.
    ///
    /// Idempotent: re-erasing an already-tombstoned map — AND erasing a payer
    /// that has no map row at all — is a no-op on the tombstone yet STILL records
    /// an `erasure` audit event and succeeds (204). Every erase request is
    /// recorded, even a repeat or a never-mapped payer; GDPR right-to-erasure must
    /// not leak whether a payer was ever mapped, so "erase what isn't there" is a
    /// successful no-op, not a 404 (unlike [`Self::reidentify`], which returns a
    /// `pii_ref` and so legitimately 404s on an absent row).
    ///
    /// # Errors
    /// [`DomainError::MissingInvestigationReason`] (empty reason, pre-write),
    /// [`DomainError::Internal`] on a storage / scope / audit failure (rolls the
    /// transaction back).
    #[allow(
        clippy::too_many_arguments,
        reason = "one erasure's home (forensic chain) + data (map) scope/tenant + payer + actor/reason"
    )]
    pub async fn erase(
        &self,
        db: &DBProvider<DbError>,
        ctx: &SecurityContext,
        home_scope: &AccessScope,
        home_tenant: Uuid,
        data_scope: &AccessScope,
        data_tenant: Uuid,
        payer_tenant_id: Uuid,
        actor_ref: String,
        reason: String,
        correlation_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        // Forensic gate: an erasure MUST carry a non-empty reason —
        // the same bar `reidentify` enforces. Checked here (not just at the REST
        // seam) so every caller, own-tenant or cross-tenant, is covered before any
        // write. The trimmed value is what the `erasure` audit record records.
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(DomainError::MissingInvestigationReason(format!(
                "erasing payer {payer_tenant_id} requires a non-empty reason"
            )));
        }

        // `ctx` authorized the call at the REST seam (the PEP gate ran there); the
        // in-txn writes are tenant-scoped (data scope for the map, home scope for
        // the forensic record) and the audit append generates its own ids, so the
        // txn body does not re-thread `ctx`.
        let _ = ctx;
        let audit = self.audit.clone();
        let map = self.map.clone();
        let home_scope = home_scope.clone();
        let data_scope = data_scope.clone();

        let result: Result<(), DbError> = db
            .db()
            .transaction_with_retry(TxConfig::serializable(), as_db_err, move |txn| {
                let audit = audit.clone();
                let map = map.clone();
                let home_scope = home_scope.clone();
                let data_scope = data_scope.clone();
                let actor_ref = actor_ref.clone();
                let reason = reason.clone();
                Box::pin(async move {
                    // 1. Tombstone the target map (idempotent). A missing or
                    //    already-tombstoned row is a no-op — erasing a never-mapped
                    //    payer still succeeds and STILL records the forensic
                    //    `erasure` event below (every erase request is recorded;
                    //    GDPR erasure must not reveal whether a payer was mapped).
                    map.tombstone(txn, &data_scope, data_tenant, payer_tenant_id)
                        .await
                        .map_err(repo_to_db)?;

                    // 2. Record ONE `erasure` record on the HOME chain in the SAME
                    //    txn — the actor's tenant owns the trail of what it erased
                    //    (in `data_tenant`).
                    let before_after = serde_json::json!({
                        "tenant_id": data_tenant,
                        "payer_tenant_id": payer_tenant_id,
                        "erased": true,
                    });
                    audit
                        .append(
                            txn,
                            &home_scope,
                            home_tenant,
                            AuditEventType::Erasure,
                            Some(actor_ref.as_str()),
                            Some(reason.as_str()),
                            &before_after,
                            correlation_id,
                            None,
                        )
                        // Propagate the append's `DbError` as-is so a retryable
                        // SSI serialization failure is not buried in a
                        // non-retryable `DbErr::Custom`.
                        .await?;
                    Ok(())
                })
            })
            .await;

        // §9: count a successful erasure (`ledger_erasure_applied_total`); only a
        // committed erase increments. `decode_business_error` maps any business
        // sentinel back to its `DomainError` rather than mislabelling it 500 (the
        // erase txn body raises no business rejection today — the reason gate is
        // pre-txn — but keep the decode symmetric with `reidentify`).
        let mapped = result.map_err(|e| crate::infra::posting::service::decode_business_error(&e));
        if mapped.is_ok() {
            self.metrics.erasure_applied();
        }
        mapped
    }

    /// Re-identify (forensic-gated) the PII map for
    /// `(data_tenant, payer_tenant_id)`: returns the `pii_ref` AFTER recording
    /// ONE `re-identification` secured-audit record, in one `SERIALIZABLE`
    /// transaction.
    ///
    /// `home_scope`/`home_tenant` own the forensic record + its chain (mirrors
    /// [`crate::infra::authz::cross_tenant::CrossTenantGateway`]);
    /// `data_scope`/`data_tenant` scope the map read. For a routine same-tenant
    /// re-identify the two coincide.
    ///
    /// Forensic gate (mirrors [`crate::infra::authz::cross_tenant`]): both a
    /// free-text `reason` AND a machine `reason_code` are required — a missing /
    /// empty one fails with [`DomainError::MissingInvestigationReason`] BEFORE
    /// any read or write (the audit record is never half-written). A
    /// re-identify of a TOMBSTONED payer is the documented investigator path, so
    /// the `pii_ref` is returned regardless of `erased`. Absent map row →
    /// [`DomainError::PayerPiiNotFound`] (404).
    ///
    /// # Errors
    /// [`DomainError::MissingInvestigationReason`] (gate, pre-write),
    /// [`DomainError::PayerPiiNotFound`] (no map row),
    /// [`DomainError::Internal`] on a storage / scope / audit failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "the full re-identify contract: home (forensic chain) + data (map) scope/tenant + payer + actor/reason/reason_code"
    )]
    pub async fn reidentify(
        &self,
        db: &DBProvider<DbError>,
        ctx: &SecurityContext,
        home_scope: &AccessScope,
        home_tenant: Uuid,
        data_scope: &AccessScope,
        data_tenant: Uuid,
        payer_tenant_id: Uuid,
        actor_ref: String,
        reason: String,
        reason_code: String,
        correlation_id: Option<Uuid>,
    ) -> Result<String, DomainError> {
        let _ = ctx;
        let audit = self.audit.clone();
        let map = self.map.clone();
        let home_scope = home_scope.clone();
        let data_scope = data_scope.clone();

        let result: Result<String, DbError> = db
            .db()
            .transaction_with_retry(TxConfig::serializable(), as_db_err, move |txn| {
                let audit = audit.clone();
                let map = map.clone();
                let home_scope = home_scope.clone();
                let data_scope = data_scope.clone();
                let actor_ref = actor_ref.clone();
                let reason = reason.clone();
                let reason_code = reason_code.clone();
                Box::pin(async move {
                    // 1. Forensic gate: BOTH a non-empty reason AND reason_code are
                    //    required (fail BEFORE any read or write — no half-written
                    //    record). Mirrors `CrossTenantGateway::resolve_read_scope`.
                    let reason = reason.trim();
                    let reason_code = reason_code.trim();
                    if reason.is_empty() || reason_code.is_empty() {
                        return Err(business(DomainError::MissingInvestigationReason(format!(
                            "re-identifying payer {payer_tenant_id} requires both a reason \
                             and a reason_code"
                        ))));
                    }

                    // 2. Read the map row under the DATA scope; 404 if absent
                    //    (re-identify of a tombstoned payer is allowed — the
                    //    `erased` flag is ignored here, the investigator path
                    //    returns the ref regardless).
                    let (pii_ref, erased) = map
                        .get(txn, &data_scope, data_tenant, payer_tenant_id)
                        .await
                        .map_err(repo_to_db)?
                        .ok_or_else(|| {
                            business(DomainError::PayerPiiNotFound(format!(
                                "no payer_pii_map for payer {payer_tenant_id}"
                            )))
                        })?;

                    // 3. Record ONE `re-identification` record on the HOME chain
                    //    BEFORE returning the pii_ref, in the SAME txn (the
                    //    forensic trail and the read commit or roll back together).
                    let before_after = serde_json::json!({
                        "tenant_id": data_tenant,
                        "payer_tenant_id": payer_tenant_id,
                        "erased": erased,
                        "reason": reason,
                    });
                    audit
                        .append(
                            txn,
                            &home_scope,
                            home_tenant,
                            AuditEventType::ReIdentification,
                            Some(actor_ref.as_str()),
                            Some(reason_code),
                            &before_after,
                            correlation_id,
                            None,
                        )
                        // Propagate the append's `DbError` as-is so a retryable
                        // SSI serialization failure is not buried in a
                        // non-retryable `DbErr::Custom`.
                        .await?;
                    Ok(pii_ref)
                })
            })
            .await;

        // §9: count a successful re-identification (`ledger_reidentification_total`).
        // A business rejection (missing reason / not found) is decoded below; only
        // a committed Ok increments.
        let mapped = result.map_err(|e| crate::infra::posting::service::decode_business_error(&e));
        if mapped.is_ok() {
            self.metrics.reidentification();
        }
        mapped
    }
}

/// Retry-extractor for the `SERIALIZABLE` PII writes: a wrapped `DbErr` so a
/// serialization failure (statement or COMMIT) is retryable contention; the
/// business-rejection sentinel is a non-retryable `DbErr::Custom` (mirrors
/// `infra::annotation::as_db_err` / `infra::posting::service::as_db_err`).
fn as_db_err(e: &DbError) -> Option<&sea_orm::DbErr> {
    match e {
        DbError::Sea(db_err) => Some(db_err),
        _ => None,
    }
}

#[cfg(test)]
#[path = "pii_tests.rs"]
mod tests;

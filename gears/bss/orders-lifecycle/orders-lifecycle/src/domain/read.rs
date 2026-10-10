//! Read-surface rules (08 §2.2, §3.6 common read wrapper, §4.4; D-139, D-141): page bounds,
//! the cursor token codec with its request binding, scope confinement for the logging decision
//! and the served/refused access-log decision table. Everything here is pure: no storage, no PDP
//! call, no actor-class evaluation.
//!
//! Input validation (page size, filters, cursor) precedes the access decision and appends no
//! access-log row; a served log is required before any cross-tenant or proof-bearing
//! disclosure; a refused log is required on every access refusal.
use crate::authz::ProofDenial;
use crate::gts::permissions::properties;
use aws_lc_rs::digest::{SHA256, digest};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bss_orders_lifecycle_sdk::catalog::Reason;
use bss_orders_lifecycle_sdk::reads::{Cursor, OrderFilters, PageSize};
use serde::{Deserialize, Serialize};
use toolkit_security::access_scope::{AccessScope, ScopeFilter, ScopeValue};
use uuid::Uuid;

/// The current token version; a token of any other version is `cursor-invalid`.
const CURSOR_VERSION: u8 = 1;

/// The paged collections delivered by the early S6-01 subset, each with its immutable sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collection {
    /// The order list: `(created_at, order_id)` ascending over the aggregate.
    Orders,
    /// The per-order line read: `(created_at, line_id)` ascending over the line identity,
    /// joined to the current (draft working) membership.
    Lines,
}
impl Collection {
    /// The S1-02 catalog operation identifier, stored in `orders_read_access_log.operation`.
    #[must_use]
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Orders => "list",
            Self::Lines => "list_lines",
        }
    }
    /// The declared sort, part of the token binding (clients cannot select another).
    #[must_use]
    pub const fn sort(self) -> &'static str {
        match self {
            Self::Orders => "created_at,order_id",
            Self::Lines => "created_at,line_id",
        }
    }
}

/// The catalog operation identifier of the point read.
pub const GET_OPERATION: &str = "get";

/// The keyset position of the last returned row: its creation instant in microseconds since
/// the Unix epoch (the stored `timestamptz` precision, preserved exactly) and its identity
/// tiebreaker (binary UUID order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub at_micros: i64,
    pub id: Uuid,
}
impl Position {
    /// The position of a returned row.
    ///
    /// # Errors
    /// An instant outside the microsecond `i64` range cannot be a stored `timestamptz`.
    pub fn of(created_at: time::OffsetDateTime, id: Uuid) -> Result<Self, ReadRuleError> {
        let micros = created_at.unix_timestamp_nanos().div_euclid(1_000);
        Ok(Self {
            at_micros: i64::try_from(micros).map_err(|_| ReadRuleError::Precision)?,
            id,
        })
    }
    /// The exact instant the position names.
    ///
    /// # Errors
    /// A microsecond count outside the representable range.
    pub fn instant(self) -> Result<time::OffsetDateTime, ReadRuleError> {
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.at_micros) * 1_000)
            .map_err(|_| ReadRuleError::Precision)
    }
}

/// A pure-rule failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadRuleError {
    /// The token fails §2.2's structure, version, precision or request binding (D-139).
    #[error("cursor-invalid")]
    CursorInvalid,
    /// An instant outside the microsecond range.
    #[error("instant outside the stored precision")]
    Precision,
}

/// What a token binds to (08 §2.2): the endpoint, the parent order where the collection has one,
/// the authenticated principal and tenant, the normalized filters and the sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorBinding<'a> {
    pub collection: Collection,
    pub parent: Option<Uuid>,
    pub subject_id: Uuid,
    pub subject_tenant_id: Uuid,
    pub filters_hash: &'a str,
}

/// The stored token. Short keys keep the opaque token small; `deny_unknown_fields` makes an
/// extra field a structural failure.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    v: u8,
    c: Collection,
    p: Option<Uuid>,
    s: Uuid,
    t: Uuid,
    f: String,
    o: String,
    k: Position,
}

/// The normalized-filter hash the order-list token binds to: SHA-256 over the canonical JSON
/// of the typed filters (fixed member order, RFC 3339 instants in UTC, absent members omitted).
#[must_use]
pub fn filters_hash(filters: &OrderFilters) -> String {
    let utc =
        |instant: Option<time::OffsetDateTime>| instant.map(|t| t.to_offset(time::UtcOffset::UTC));
    let normalized = OrderFilters {
        state: filters.state,
        created_from: utc(filters.created_from),
        created_to: utc(filters.created_to),
        state_entered_before: utc(filters.state_entered_before),
        contract_id: filters.contract_id,
    };
    let canonical = serde_json::to_vec(&normalized).unwrap_or_default();
    hex(digest(&SHA256, &canonical).as_ref())
}

/// The empty-filter hash of the per-order collections, which take no filters.
#[must_use]
pub fn no_filters_hash() -> String {
    filters_hash(&OrderFilters::default())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Encode the continuation token issued from the last returned row.
///
/// # Errors
/// A token that cannot be encoded (never for well-formed inputs).
pub fn encode_cursor(
    binding: CursorBinding<'_>,
    position: Position,
) -> Result<Cursor, ReadRuleError> {
    let token = Token {
        v: CURSOR_VERSION,
        c: binding.collection,
        p: binding.parent,
        s: binding.subject_id,
        t: binding.subject_tenant_id,
        f: binding.filters_hash.to_owned(),
        o: binding.collection.sort().to_owned(),
        k: position,
    };
    let raw = serde_json::to_vec(&token).map_err(|_| ReadRuleError::CursorInvalid)?;
    Cursor::try_from(URL_SAFE_NO_PAD.encode(raw)).map_err(|_| ReadRuleError::CursorInvalid)
}

/// Validate a supplied token's structure, version, precision and binding to this request, and
/// return the position to continue strictly after (08 §2.2; D-139). Every failure is
/// `cursor-invalid`; no distinction is disclosed.
///
/// # Errors
/// `CursorInvalid`.
pub fn decode_cursor(
    cursor: &Cursor,
    binding: CursorBinding<'_>,
) -> Result<Position, ReadRuleError> {
    let invalid = ReadRuleError::CursorInvalid;
    let raw = URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|_| invalid)?;
    let token: Token = serde_json::from_slice(&raw).map_err(|_| invalid)?;
    if token.v != CURSOR_VERSION
        || token.c != binding.collection
        || token.p != binding.parent
        || token.s != binding.subject_id
        || token.t != binding.subject_tenant_id
        || token.f != binding.filters_hash
        || token.o != binding.collection.sort()
    {
        return Err(invalid);
    }
    // Precision: the instant must be representable exactly.
    token.k.instant()?;
    Ok(token.k)
}

/// The scoped query fetches at most `page_size + 1` rows (08 §2.2).
#[must_use]
pub fn fetch_limit(page_size: PageSize) -> u64 {
    u64::from(page_size) + 1
}

/// Keep at most `page_size` rows and report whether an extra authorized row followed, in which
/// case the caller issues a cursor from the last returned row.
#[must_use]
pub fn split_page<T>(mut rows: Vec<T>, page_size: PageSize) -> (Vec<T>, bool) {
    let limit = usize::from(page_size.get());
    let more = rows.len() > limit;
    rows.truncate(limit);
    (rows, more)
}

/// Whether a PDP-compiled collection scope is provably confined to the subject's own resource
/// tenant (08 §4.4): every OR path carries an in-memory-representable `resource_tenant_id`
/// filter whose values are all the subject tenant. Anything else — an unconstrained scope, a
/// seller/payer/ID-only path, a subquery filter, or a foreign resource tenant — is *not*
/// provably confined, so the served read logs conservatively. This classifies logging only; it
/// never widens or narrows the scope the query applies.
#[must_use]
pub fn confined_to_subject(scope: &AccessScope, subject_tenant_id: Uuid) -> bool {
    if scope.is_unconstrained() || scope.constraints().is_empty() {
        return false;
    }
    let own = ScopeValue::Uuid(subject_tenant_id);
    scope.constraints().iter().all(|path| {
        path.filters().iter().any(|filter| {
            matches!(filter, ScopeFilter::Eq(_) | ScopeFilter::In(_))
                && filter.property() == properties::RESOURCE_TENANT_ID
                && {
                    let values = filter.values();
                    let mut iter = values.iter();
                    let mut any = false;
                    let all_own = iter.all(|value| {
                        any = true;
                        value == &own
                            || value
                                .as_uuid()
                                .is_some_and(|uuid| uuid == subject_tenant_id)
                    });
                    any && all_own
                }
        })
    })
}

/// The served-log decision of 08 §4.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedLog {
    /// Append a served row before disclosure; its failure is `read-store-unavailable`.
    Required,
    /// An own-resource-tenant read with no supplied proof: nothing is appended.
    NotRequired,
}

/// Point/child read: log when a proof reference was supplied on the allowed request or the
/// current resource tenant differs from the authenticated subject tenant (direct seller/payer
/// access included).
#[must_use]
pub fn point_read_log(
    proof_supplied: bool,
    subject_tenant_id: Uuid,
    resource_tenant_id: Uuid,
) -> ServedLog {
    if proof_supplied || subject_tenant_id != resource_tenant_id {
        ServedLog::Required
    } else {
        ServedLog::NotRequired
    }
}

/// Collection: one row per request when a proof was supplied or the effective scope is not
/// provably confined to the subject's resource tenant, including mixed and empty pages.
#[must_use]
pub fn collection_log(proof_supplied: bool, confined: bool) -> ServedLog {
    if proof_supplied || !confined {
        ServedLog::Required
    } else {
        ServedLog::NotRequired
    }
}

/// The outcome column of an access-log row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessOutcome {
    Served,
    Refused,
}
impl AccessOutcome {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::Refused => "refused",
        }
    }
}

/// A refused row's reason columns: the public reason and, for a targeted delegation-proof
/// denial hidden behind `order-not-found`, the operational-only classified detail (D-141).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub reason: Reason,
    pub proof: Option<ProofDenial>,
}
impl Refusal {
    /// The stored `internal_refusal_detail`: only a proof denial of a targeted request keeps
    /// one, and only under the public `order-not-found` (the table CHECK enforces the same).
    #[must_use]
    pub fn internal_detail(self) -> Option<&'static str> {
        match (self.reason, self.proof) {
            (Reason::OrderNotFound, Some(ProofDenial::Required)) => {
                Some("delegation-proof-required")
            }
            (Reason::OrderNotFound, Some(ProofDenial::Invalid)) => Some("delegation-proof-invalid"),
            _ => None,
        }
    }
}

/// The requested and resolved target columns of an order-scoped row (08 §3.7): the requested
/// identifier is always kept; the foreign-key column is set only where the aggregate exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub requested_order_ref: Uuid,
    pub exists: bool,
}
impl Target {
    #[must_use]
    pub fn order_id(self) -> Option<Uuid> {
        self.exists.then_some(self.requested_order_ref)
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;

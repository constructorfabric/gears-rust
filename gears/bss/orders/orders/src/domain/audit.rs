//! Transition-audit integrity: canonical byte encodings, sealing, verification and the exact
//! row shape of every writer path.
//!
//! Sources: DESIGN §4.4 *Audit* (D-99 v1, D-143 v2, D-201 v3 encodings, genesis, chain checks,
//! D-100 roll-up encoding), Foundation §3.7 `orders_transition_audit` (target-shape invariants,
//! committed reason tokens, actor class D-115, refusal ownership D-104), DESIGN §4.3 immutable
//! identity and minimization (D-96/D-103, keyed by D-204). Pure rules only; the locked transactional writer is
//! `infra::storage::repo::audit`. New writers seal v3; v1/v2 stay verifiable and every other
//! version is refused, never decoded as a fallback.
use bss_orders_sdk::catalog::{OrderState, Reason, Trigger};
use bss_orders_sdk::models::{DelegationProofRef, IdempotencyKey};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use toolkit_security::crypto::{CryptoError, hash::Sha256, mac::HmacSha256};
use uuid::Uuid;

use super::failure::FailureReason;
use super::idempotency::REPLACE_FULFILLMENT_GRANT;

/// SHA-256 digest length stored in `entry_hash`, `prev_hash` and checkpoint digests.
pub const DIGEST_LEN: usize = 32;
/// One stored digest.
pub type Digest = [u8; DIGEST_LEN];
/// `caller_reason` bound in Unicode scalar values (DB `length()`), D-143.
pub const MAX_CALLER_REASON_CHARS: usize = 4096;
/// `external_reference` bound (administrative content DDL).
pub const MAX_EXTERNAL_REFERENCE_CHARS: usize = 1024;
/// Committed v1 checkpoint roll-up format (D-100).
pub const CHECKPOINT_FORMAT_VERSION: i16 = 1;

const ROW_TAG_V1: &str = "VHP-BSS-ORDERS-AUDIT-ROW-v1";
const ROW_TAG_V2: &str = "VHP-BSS-ORDERS-AUDIT-ROW-v2";
const ROW_TAG_V3: &str = "VHP-BSS-ORDERS-AUDIT-ROW-v3";
const GENESIS_TAG: &str = "VHP-BSS-ORDERS-AUDIT-GENESIS-v1";
const ROLLUP_GENESIS_TAG: &str = "VHP-BSS-ORDERS-AUDIT-ROLLUP-GENESIS-v1";
const ROLLUP_TAG: &str = "VHP-BSS-ORDERS-AUDIT-ROLLUP-v1";
const COMMITTED: &str = "committed";
const REFUSED: &str = "refused";
const FORCE_FAIL_TRIGGER: &str = "force-fail-unreconciled";

/// Audit row encoding version; not the order version.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashVersion {
    /// D-99, frozen: no `caller_reason`, no observation.
    V1,
    /// D-143, frozen: v1 plus `caller_reason`.
    V2,
    /// D-201: v2 plus the closed `force_request_observation`. Every new writer emits it.
    V3,
}
impl HashVersion {
    /// The only version a new writer seals.
    pub const CURRENT: Self = Self::V3;
    /// Explicitly defined versions only; anything else is an unsupported-version failure.
    ///
    /// # Errors
    /// `UnsupportedVersion` for every stored value other than 1, 2 or 3.
    pub fn from_stored(value: i16) -> Result<Self, AuditError> {
        match value {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            3 => Ok(Self::V3),
            other => Err(AuditError::UnsupportedVersion(other)),
        }
    }
    /// Stored `hash_version` column value.
    #[must_use]
    pub const fn stored(self) -> i16 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Self::V1 => ROW_TAG_V1,
            Self::V2 => ROW_TAG_V2,
            Self::V3 => ROW_TAG_V3,
        }
    }
}

/// Encoding, shape or minimization failure. A writer never stores a row that fails any of these.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// Stored hash version outside 1, 2 and 3.
    #[error("unsupported audit hash version {0}")]
    UnsupportedVersion(i16),
    /// The selected encoding cannot cover a populated field.
    #[error("audit encoding version cannot cover {0}")]
    Uncovered(&'static str),
    /// A field is longer than the `u32` frame length allows.
    #[error("audit field {0} exceeds the u32 frame length")]
    FrameTooLong(&'static str),
    /// A field that must be positive is zero or negative.
    #[error("audit field {0} must be positive")]
    NonPositive(&'static str),
    /// A digest field is not exactly 32 bytes.
    #[error("audit field {0} must be a 32-byte digest")]
    DigestLength(&'static str),
    /// An instant is not microsecond-normalized or out of range.
    #[error("audit instant is not microsecond-normalized or out of range")]
    Instant,
    /// The row violates the named shape invariant.
    #[error("audit row shape violates {0}")]
    Shape(&'static str),
    /// The named field is not minimized.
    #[error("audit field {0} is not minimized")]
    NotMinimized(&'static str),
    /// A reference carries credential material.
    #[error("reference carries credential material")]
    CredentialInReference,
    /// The actor is not a stable authenticated identity.
    #[error("audit actor is not a stable authenticated identity")]
    UnstableActor,
    /// The configured audit identities are invalid for the named reason.
    #[error("audit identity configuration is invalid: {0}")]
    Identities(&'static str),
    /// The committed audit sequence cannot be incremented further.
    #[error("audit sequence exhausted")]
    SequenceExhausted,
}

// ---------------------------------------------------------------------------------------------
// Framing

/// `F(x)`: `0x00` for NULL, otherwise `0x01 || u32_be(len) || bytes` (D-99).
#[derive(Default)]
struct Frames(Vec<u8>);
impl Frames {
    fn tagged(tag: &str) -> Self {
        let mut out = tag.as_bytes().to_vec();
        out.push(0x1f);
        Self(out)
    }
    fn raw(&mut self, field: &'static str, bytes: Option<&[u8]>) -> Result<(), AuditError> {
        match bytes {
            None => self.0.push(0),
            Some(bytes) => {
                let len =
                    u32::try_from(bytes.len()).map_err(|_| AuditError::FrameTooLong(field))?;
                self.0.push(1);
                self.0.extend_from_slice(&len.to_be_bytes());
                self.0.extend_from_slice(bytes);
            }
        }
        Ok(())
    }
    /// A present UUID frame is always 16 bytes and cannot fail.
    fn fixed_uuid(&mut self, value: Uuid) {
        self.0.push(1);
        self.0.extend_from_slice(&16u32.to_be_bytes());
        self.0.extend_from_slice(value.as_bytes());
    }
    fn uuid(&mut self, field: &'static str, value: Option<Uuid>) -> Result<(), AuditError> {
        self.raw(field, value.as_ref().map(|v| v.as_bytes().as_slice()))
    }
    fn text(&mut self, field: &'static str, value: Option<&str>) -> Result<(), AuditError> {
        self.raw(field, value.map(str::as_bytes))
    }
    fn u16(&mut self, field: &'static str, value: i16) -> Result<(), AuditError> {
        let value = u16::try_from(value).map_err(|_| AuditError::NonPositive(field))?;
        self.raw(field, Some(&value.to_be_bytes()))
    }
    /// Unsigned 64-bit big-endian of a positive signed database value.
    fn positive(&mut self, field: &'static str, value: Option<i64>) -> Result<(), AuditError> {
        match value {
            None => self.raw(field, None),
            Some(v) if v > 0 => self.raw(field, Some(&v.unsigned_abs().to_be_bytes())),
            Some(_) => Err(AuditError::NonPositive(field)),
        }
    }
    /// Non-negative counts (checkpoint `member_count`) use the same unsigned framing.
    fn count(&mut self, field: &'static str, value: i64) -> Result<(), AuditError> {
        let value = u64::try_from(value).map_err(|_| AuditError::NonPositive(field))?;
        self.raw(field, Some(&value.to_be_bytes()))
    }
    fn instant(&mut self, field: &'static str, value: OffsetDateTime) -> Result<(), AuditError> {
        self.raw(field, Some(&micros(value)?.to_be_bytes()))
    }
    fn digest(&mut self, field: &'static str, value: Option<&[u8]>) -> Result<(), AuditError> {
        if value.is_some_and(|v| v.len() != DIGEST_LEN) {
            return Err(AuditError::DigestLength(field));
        }
        self.raw(field, value)
    }
    fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// Signed microseconds since the Unix epoch, UTC. A value with sub-microsecond residue is refused
/// so the hashed instant can never differ from what PostgreSQL stores.
///
/// # Errors
/// `Instant` for a non-normalized or out-of-range value.
pub fn micros(value: OffsetDateTime) -> Result<i64, AuditError> {
    let nanos = value.unix_timestamp_nanos();
    if nanos.rem_euclid(1_000) != 0 {
        return Err(AuditError::Instant);
    }
    i64::try_from(nanos.div_euclid(1_000)).map_err(|_| AuditError::Instant)
}

/// Normalize once, before both hashing and storage, to microsecond precision (floor).
///
/// # Errors
/// `Instant` when the value is outside the signed 64-bit microsecond range.
pub fn normalize_instant(value: OffsetDateTime) -> Result<OffsetDateTime, AuditError> {
    let nanos = value.unix_timestamp_nanos();
    let floored = nanos - nanos.rem_euclid(1_000);
    i64::try_from(floored.div_euclid(1_000)).map_err(|_| AuditError::Instant)?;
    OffsetDateTime::from_unix_timestamp_nanos(floored)
        .map(|t| t.to_offset(time::UtcOffset::UTC))
        .map_err(|_| AuditError::Instant)
}

fn sha256(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes)
}

// ---------------------------------------------------------------------------------------------
// Stored row and its encodings

/// D-201 closed object `{audit_sequence, state, version}` observed under the aggregate lock.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceRequestObservation {
    /// Committed audit sequence observed on the order.
    pub audit_sequence: i64,
    /// Observed order state token.
    pub state: String,
    /// Observed order version.
    pub version: i32,
}
impl ForceRequestObservation {
    /// The persisted JSON object: exactly three members with JSON numbers (DB CHECK).
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "audit_sequence": self.audit_sequence,
            "state": self.state,
            "version": self.version,
        })
    }
    /// Strict decode of a stored observation; unknown or missing members refuse.
    ///
    /// # Errors
    /// `Shape` for any other JSON shape.
    pub fn from_json(value: &Value) -> Result<Self, AuditError> {
        const BAD: AuditError = AuditError::Shape("force_request_observation members");
        let object = value.as_object().ok_or(BAD)?;
        if object.len() != 3 {
            return Err(BAD);
        }
        let audit_sequence = object
            .get("audit_sequence")
            .and_then(Value::as_i64)
            .ok_or(BAD)?;
        let state = object.get("state").and_then(Value::as_str).ok_or(BAD)?;
        let version = object
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or(BAD)?;
        Ok(Self {
            audit_sequence,
            state: state.to_owned(),
            version,
        })
    }
}

/// Every persisted column of one audit row except `entry_hash`, exactly as stored.
///
/// The encoder destructures this record exhaustively, so a new evidence field cannot silently
/// escape integrity coverage (DESIGN §4.4, Pricing precedent).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// Encoding version (`HashVersion::stored`) that sealed the row.
    pub hash_version: i16,
    /// Row identifier.
    pub audit_id: Uuid,
    /// Immutable chain namespace of the order; NULL when the order is unresolved.
    pub audit_tenant_id: Option<Uuid>,
    /// Tenant of the authenticated subject.
    pub subject_tenant_id: Uuid,
    /// Resource-tenant snapshot of the order; NULL when the order is unresolved.
    pub resource_tenant_id: Option<Uuid>,
    /// Resolved order; NULL when the attempt did not resolve one.
    pub order_id: Option<Uuid>,
    /// Order reference the caller targeted; NULL only for `create`.
    pub requested_order_ref: Option<Uuid>,
    /// Committed chain position; NULL for refusals.
    pub sequence: Option<i64>,
    /// State token before the transition; NULL for create and unresolved refusals.
    pub from_state: Option<String>,
    /// State token after the transition; equals `from_state` on a resolved refusal.
    pub to_state: Option<String>,
    /// Trigger token.
    pub trigger: String,
    /// `committed` or `refused`.
    pub outcome: String,
    /// Immutable subject UUID of the actor (D-103).
    pub actor: String,
    /// Actor class token: `system`, `service` or `user` (D-115).
    pub actor_class: String,
    /// Validated delegation proof reference, if any.
    pub delegation_proof_ref: Option<String>,
    /// Reason token: the trigger for committed rows, a registered refusal reason otherwise.
    pub reason: String,
    /// Administrative field token; only on committed administrative edits.
    pub changed_field: Option<String>,
    /// Minimized prior value of `changed_field`.
    pub prior_value: Option<String>,
    /// Minimized new value of `changed_field`.
    pub new_value: Option<String>,
    /// Idempotency key of the attempt.
    pub idempotency_key: String,
    /// Caller correlation identifier, if any.
    pub correlation_id: Option<Uuid>,
    /// Order version after a commit, or the observed version on a resolved refusal.
    pub version: Option<i32>,
    /// Microsecond-normalized UTC write instant.
    pub created_at: OffsetDateTime,
    /// Predecessor digest (genesis for sequence 1); NULL for refusals.
    pub prev_hash: Option<Vec<u8>>,
    /// Caller-supplied reason text (v2+), 1..=4096 characters.
    pub caller_reason: Option<String>,
    /// Observation recorded on a v3 force-fail request refusal (D-201).
    pub force_request_observation: Option<ForceRequestObservation>,
}

/// Canonical preimage selected by the row's stored `hash_version`.
///
/// # Errors
/// Unsupported version, evidence the version cannot cover, malformed numbers, digests or
/// instants. Lengths beyond `u32::MAX` refuse; nothing is truncated or substituted.
pub fn preimage(row: &AuditRow) -> Result<Vec<u8>, AuditError> {
    let AuditRow {
        hash_version,
        audit_id,
        audit_tenant_id,
        subject_tenant_id,
        resource_tenant_id,
        order_id,
        requested_order_ref,
        sequence,
        from_state,
        to_state,
        trigger,
        outcome,
        actor,
        actor_class,
        delegation_proof_ref,
        reason,
        changed_field,
        prior_value,
        new_value,
        idempotency_key,
        correlation_id,
        version,
        created_at,
        prev_hash,
        caller_reason,
        force_request_observation,
    } = row;
    let encoding = HashVersion::from_stored(*hash_version)?;
    if encoding == HashVersion::V1 && caller_reason.is_some() {
        return Err(AuditError::Uncovered("caller_reason"));
    }
    if encoding != HashVersion::V3 && force_request_observation.is_some() {
        return Err(AuditError::Uncovered("force_request_observation"));
    }
    let mut f = Frames::tagged(encoding.tag());
    f.u16("hash_version", *hash_version)?;
    f.uuid("audit_id", Some(*audit_id))?;
    f.uuid("audit_tenant_id", *audit_tenant_id)?;
    f.uuid("subject_tenant_id", Some(*subject_tenant_id))?;
    f.uuid("resource_tenant_id", *resource_tenant_id)?;
    f.uuid("order_id", *order_id)?;
    f.uuid("requested_order_ref", *requested_order_ref)?;
    f.positive("sequence", *sequence)?;
    f.text("from_state", from_state.as_deref())?;
    f.text("to_state", to_state.as_deref())?;
    f.text("trigger", Some(trigger))?;
    f.text("outcome", Some(outcome))?;
    f.text("actor", Some(actor))?;
    f.text("actor_class", Some(actor_class))?;
    f.text("delegation_proof_ref", delegation_proof_ref.as_deref())?;
    f.text("reason", Some(reason))?;
    f.text("changed_field", changed_field.as_deref())?;
    f.text("prior_value", prior_value.as_deref())?;
    f.text("new_value", new_value.as_deref())?;
    f.text("idempotency_key", Some(idempotency_key))?;
    f.uuid("correlation_id", *correlation_id)?;
    f.positive("version", version.map(i64::from))?;
    f.instant("created_at", *created_at)?;
    f.digest("prev_hash", prev_hash.as_deref())?;
    if encoding != HashVersion::V1 {
        f.text("caller_reason", caller_reason.as_deref())?;
    }
    if encoding == HashVersion::V3 {
        match force_request_observation {
            None => f.raw("force_request_observation", None)?,
            Some(ForceRequestObservation {
                audit_sequence,
                state,
                version,
            }) => {
                let mut inner = Frames::default();
                inner.positive(
                    "force_request_observation.audit_sequence",
                    Some(*audit_sequence),
                )?;
                inner.text("force_request_observation.state", Some(state))?;
                inner.positive(
                    "force_request_observation.version",
                    Some(i64::from(*version)),
                )?;
                f.raw("force_request_observation", Some(&inner.finish()))?;
            }
        }
    }
    Ok(f.finish())
}

/// `SHA256(GENESIS_TAG || F(audit_tenant_id) || F(order_id))`: `prev_hash` of sequence 1.
#[must_use]
pub fn genesis(audit_tenant_id: Uuid, order_id: Uuid) -> Digest {
    sha256(&genesis_preimage(audit_tenant_id, order_id))
}
/// Genesis preimage: `GENESIS_TAG || F(audit_tenant_id) || F(order_id)`.
#[must_use]
pub fn genesis_preimage(audit_tenant_id: Uuid, order_id: Uuid) -> Vec<u8> {
    let mut f = Frames::tagged(GENESIS_TAG);
    f.fixed_uuid(audit_tenant_id);
    f.fixed_uuid(order_id);
    f.finish()
}

/// Shape-checked digest of a row (the stored `entry_hash` of a correct row).
///
/// # Errors
/// Any shape or encoding failure.
pub fn entry_hash(row: &AuditRow) -> Result<Digest, AuditError> {
    check_shape(row)?;
    Ok(sha256(&preimage(row)?))
}

fn registered_state(token: &str) -> bool {
    OrderState::from_token(token).is_some()
}
fn registered_trigger(token: &str) -> bool {
    token == REPLACE_FULFILLMENT_GRANT || Trigger::from_token(token).is_some()
}
fn registered_reason(token: &str) -> bool {
    Reason::from_token(token).is_some()
}
/// Parse a stored state token (the generated [`OrderState::token`] spelling).
///
/// # Errors
/// `Shape` for an unregistered token.
pub fn parse_state(token: &str) -> Result<OrderState, AuditError> {
    OrderState::from_token(token).ok_or(AuditError::Shape("registered state"))
}

fn canonical_uuid_text(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

/// Target-shape invariants (Foundation §3.7 D-98/D-99/D-104/D-143/D-148/D-201), mirrored from
/// the database CHECKs so a writer refuses before insert and a verifier reports a malformed row.
///
/// # Errors
/// `Shape`, `UnsupportedVersion` or `DigestLength` for the first violated rule.
pub fn check_shape(row: &AuditRow) -> Result<(), AuditError> {
    use AuditError::Shape;
    let encoding = HashVersion::from_stored(row.hash_version)?;
    if !matches!(row.outcome.as_str(), COMMITTED | REFUSED) {
        return Err(Shape("outcome"));
    }
    if !matches!(row.actor_class.as_str(), "system" | "service" | "user") {
        return Err(Shape("actor_class"));
    }
    if !canonical_uuid_text(&row.actor) || row.actor == Uuid::nil().to_string() {
        return Err(Shape("actor is a lowercase hyphenated subject UUID"));
    }
    if !registered_trigger(&row.trigger) {
        return Err(Shape("registered trigger"));
    }
    for state in [&row.from_state, &row.to_state].into_iter().flatten() {
        if !registered_state(state) {
            return Err(Shape("registered state"));
        }
    }
    if row.subject_tenant_id.is_nil() || row.audit_id.is_nil() || row.idempotency_key.is_empty() {
        return Err(Shape("required identity"));
    }
    if row
        .prev_hash
        .as_ref()
        .is_some_and(|h| h.len() != DIGEST_LEN)
    {
        return Err(AuditError::DigestLength("prev_hash"));
    }
    let committed = row.outcome == COMMITTED;
    if committed {
        let (Some(_), Some(_), Some(_), Some(sequence), Some(_), Some(to), Some(version)) = (
            row.audit_tenant_id,
            row.resource_tenant_id,
            row.order_id,
            row.sequence,
            row.prev_hash.as_ref(),
            row.to_state.as_deref(),
            row.version,
        ) else {
            return Err(Shape("committed entries carry the resolved aggregate"));
        };
        if version <= 0 {
            return Err(Shape("positive version"));
        }
        if row.trigger == "create" {
            if sequence != 1 || version != 1 || row.from_state.is_some() || to != "draft" {
                return Err(Shape("committed create is sequence 1 into draft"));
            }
        } else if row.from_state.is_none() || sequence <= 1 {
            return Err(Shape("later committed entries have a prior state"));
        }
        if row.reason != row.trigger {
            return Err(Shape("committed reason is the trigger token (D-148)"));
        }
        if row.trigger == REPLACE_FULFILLMENT_GRANT
            && (row.from_state.as_deref() != Some("in_fulfillment") || to != "in_fulfillment")
        {
            return Err(Shape("internal rebuild stays in_fulfillment"));
        }
    } else {
        if row.sequence.is_some() || row.prev_hash.is_some() || row.caller_reason.is_some() {
            return Err(Shape("refusals take no sequence, chain or caller text"));
        }
        if !registered_reason(&row.reason) {
            return Err(Shape("registered refusal reason"));
        }
    }
    match row.order_id {
        None => {
            if row.audit_tenant_id.is_some()
                || row.resource_tenant_id.is_some()
                || row.from_state.is_some()
                || row.to_state.is_some()
                || row.version.is_some()
            {
                return Err(Shape("unresolved facts stay NULL"));
            }
        }
        Some(order) => {
            if row.requested_order_ref.is_some_and(|r| r != order) {
                return Err(Shape("order and requested reference agree"));
            }
            if !committed
                && (row.audit_tenant_id.is_none()
                    || row.resource_tenant_id.is_none()
                    || row.from_state.is_none()
                    || row.from_state != row.to_state
                    || row.version.is_none_or(|v| v <= 0))
            {
                return Err(Shape("resolved refusal keeps observed state/version"));
            }
        }
    }
    if row.trigger != "create" && row.requested_order_ref.is_none() {
        return Err(Shape(
            "order-targeted attempts keep the requested reference",
        ));
    }
    if encoding == HashVersion::V1 && row.caller_reason.is_some() {
        return Err(Shape("v1 has no caller_reason"));
    }
    if let Some(text) = &row.caller_reason {
        let chars = text.chars().count();
        if chars == 0 || chars > MAX_CALLER_REASON_CHARS {
            return Err(Shape("caller_reason is 1..=4096 characters"));
        }
    }
    let admin = committed && row.trigger == "administrative-edit";
    if admin {
        if row.changed_field.is_none() || row.prior_value == row.new_value {
            return Err(Shape("administrative edit names one changed field"));
        }
    } else if row.changed_field.is_some() || row.prior_value.is_some() || row.new_value.is_some() {
        return Err(Shape(
            "only committed administrative edits carry field changes",
        ));
    }
    let force_request = !committed
        && row.trigger == FORCE_FAIL_TRIGGER
        && row.reason == Reason::SecondApproverRequired.mapping().reason;
    match &row.force_request_observation {
        Some(observation) => {
            if encoding != HashVersion::V3 || !force_request || row.order_id.is_none() {
                return Err(Shape("observation belongs to a resolved v3 force request"));
            }
            if observation.audit_sequence <= 0
                || observation.version <= 0
                || !registered_state(&observation.state)
                || row.from_state.as_deref() != Some(observation.state.as_str())
                || row.version != Some(observation.version)
            {
                return Err(Shape("observation equals the observed state/version"));
            }
        }
        None if encoding == HashVersion::V3 && force_request => {
            return Err(Shape("new force requests record the observation"));
        }
        None => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Verification

/// Explicit verification finding; never repaired or rehashed.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The row's stored hash version is not supported.
    #[error("audit {audit_id}: unsupported hash version {version}")]
    UnsupportedVersion {
        /// Affected row.
        audit_id: Uuid,
        /// Stored hash version.
        version: i16,
    },
    /// The row fails encoding or shape checks.
    #[error("audit {audit_id}: malformed row: {source}")]
    Malformed {
        /// Affected row.
        audit_id: Uuid,
        /// Underlying encoding or shape failure.
        source: AuditError,
    },
    /// The stored entry hash is not 32 bytes.
    #[error("audit {audit_id}: stored entry hash has the wrong length")]
    HashLength {
        /// Affected row.
        audit_id: Uuid,
    },
    /// The recomputed digest differs from the stored entry hash.
    #[error("audit {audit_id}: recomputed digest differs from stored entry hash")]
    DigestMismatch {
        /// Affected row.
        audit_id: Uuid,
    },
    /// A refused entry appeared in the committed chain.
    #[error("audit {audit_id}: refused entry inside the committed chain")]
    NotCommitted {
        /// Affected row.
        audit_id: Uuid,
    },
    /// The entry is bound to another order or audit namespace.
    #[error("audit {audit_id}: entry bound to another order or audit namespace")]
    Binding {
        /// Affected row.
        audit_id: Uuid,
    },
    /// The entry's sequence does not continue the chain.
    #[error("chain gap: expected sequence {expected}, found {found:?}")]
    Sequence {
        /// Next sequence the chain requires.
        expected: i64,
        /// Sequence the row carries.
        found: Option<i64>,
    },
    /// The predecessor hash or genesis does not match.
    #[error("audit {audit_id}: predecessor hash or genesis mismatch")]
    Predecessor {
        /// Affected row.
        audit_id: Uuid,
    },
    /// The verified chain head differs from the aggregate counter.
    #[error("chain head {head} differs from aggregate counter {counter}")]
    Counter {
        /// Verified head sequence.
        head: i64,
        /// `orders_order.audit_sequence` value.
        counter: i64,
    },
}

/// Recompute one entry: supported encoding, row shape, digest length and digest equality.
///
/// # Errors
/// The first finding for this entry.
pub fn verify_entry(row: &AuditRow, stored_entry_hash: &[u8]) -> Result<Digest, VerifyError> {
    let audit_id = row.audit_id;
    if let Err(AuditError::UnsupportedVersion(version)) = HashVersion::from_stored(row.hash_version)
    {
        return Err(VerifyError::UnsupportedVersion { audit_id, version });
    }
    if stored_entry_hash.len() != DIGEST_LEN {
        return Err(VerifyError::HashLength { audit_id });
    }
    let digest = entry_hash(row).map_err(|source| VerifyError::Malformed { audit_id, source })?;
    if digest.as_slice() != stored_entry_hash {
        return Err(VerifyError::DigestMismatch { audit_id });
    }
    Ok(digest)
}

/// Verified committed chain position.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainHead {
    /// Last verified committed sequence.
    pub sequence: i64,
    /// Digest of the entry at `sequence`.
    pub entry_hash: Digest,
}

/// Streaming committed-chain verifier for one order (bounded memory: one predecessor).
///
/// The namespace comes from the immutable aggregate field, never today's resource tenant.
/// Refusals are outside the committed chain and are not accepted here.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ChainVerifier {
    audit_tenant_id: Uuid,
    order_id: Uuid,
    head: Option<ChainHead>,
}
impl ChainVerifier {
    /// Verifier for the order's chain under its immutable audit namespace.
    #[must_use]
    pub fn new(audit_tenant_id: Uuid, order_id: Uuid) -> Self {
        Self {
            audit_tenant_id,
            order_id,
            head: None,
        }
    }
    /// Accept the next committed entry in ascending sequence order.
    ///
    /// # Errors
    /// Shape/encoding/digest, binding, continuity or predecessor finding.
    pub fn push(&mut self, row: &AuditRow, stored_entry_hash: &[u8]) -> Result<(), VerifyError> {
        let audit_id = row.audit_id;
        if row.outcome != COMMITTED {
            return Err(VerifyError::NotCommitted { audit_id });
        }
        let digest = verify_entry(row, stored_entry_hash)?;
        if row.order_id != Some(self.order_id) || row.audit_tenant_id != Some(self.audit_tenant_id)
        {
            return Err(VerifyError::Binding { audit_id });
        }
        let expected = self.head.map_or(Some(1), |h| h.sequence.checked_add(1));
        if row.sequence != expected {
            return Err(VerifyError::Sequence {
                expected: expected.unwrap_or(i64::MAX),
                found: row.sequence,
            });
        }
        let predecessor = self.head.map_or_else(
            || genesis(self.audit_tenant_id, self.order_id),
            |h| h.entry_hash,
        );
        if row.prev_hash.as_deref() != Some(predecessor.as_slice()) {
            return Err(VerifyError::Predecessor { audit_id });
        }
        self.head = Some(ChainHead {
            sequence: row.sequence.unwrap_or_default(),
            entry_hash: digest,
        });
        Ok(())
    }
    /// Last verified committed position, if any.
    #[must_use]
    pub fn head(&self) -> Option<ChainHead> {
        self.head
    }
    /// Reconcile with `orders_order.audit_sequence`: an intact counter detects a shortened or
    /// empty trail (D-100 item 5).
    ///
    /// # Errors
    /// `Counter` when the verified head differs from the counter.
    pub fn finish(self, counter: i64) -> Result<Option<ChainHead>, VerifyError> {
        let head = self.head.map_or(0, |h| h.sequence);
        if head != counter {
            return Err(VerifyError::Counter { head, counter });
        }
        Ok(self.head)
    }
}

// ---------------------------------------------------------------------------------------------
// Checkpoint roll-up encoding (D-100). The checkpoint worker and its reconciliation are S2-11.

/// One checkpoint header as stored (`checkpoint_hash` excluded).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointHeader {
    /// Roll-up format version; must equal `CHECKPOINT_FORMAT_VERSION`.
    pub format_version: i16,
    /// Audit namespace the checkpoint covers.
    pub audit_tenant_id: Uuid,
    /// Checkpoint position in the namespace's roll-up chain.
    pub checkpoint_sequence: i64,
    /// Capture instant.
    pub captured_at: OffsetDateTime,
    /// Number of members; must equal the member list length.
    pub member_count: i64,
    /// Digest of the previous checkpoint (roll-up genesis for the first).
    pub prev_checkpoint_hash: Vec<u8>,
}
/// One expected order-chain head.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointMember {
    /// Order whose chain head is captured.
    pub order_id: Uuid,
    /// Expected committed chain head sequence.
    pub audit_sequence: i64,
    /// Expected entry hash at `audit_sequence`.
    pub entry_hash: Vec<u8>,
}

/// `SHA256(ROLLUP_GENESIS_TAG || F(audit_tenant_id))`.
#[must_use]
pub fn checkpoint_genesis(audit_tenant_id: Uuid) -> Digest {
    let mut f = Frames::tagged(ROLLUP_GENESIS_TAG);
    f.fixed_uuid(audit_tenant_id);
    sha256(&f.finish())
}

/// Roll-up preimage: header fields then sorted members.
///
/// # Errors
/// Unsupported format, duplicate/unsorted/miscounted members, malformed numbers or digests.
pub fn checkpoint_preimage(
    header: &CheckpointHeader,
    members: &[CheckpointMember],
) -> Result<Vec<u8>, AuditError> {
    let CheckpointHeader {
        format_version,
        audit_tenant_id,
        checkpoint_sequence,
        captured_at,
        member_count,
        prev_checkpoint_hash,
    } = header;
    if *format_version != CHECKPOINT_FORMAT_VERSION {
        return Err(AuditError::UnsupportedVersion(*format_version));
    }
    if usize::try_from(*member_count).ok() != Some(members.len()) {
        return Err(AuditError::Shape("checkpoint member count"));
    }
    if members
        .windows(2)
        .any(|pair| pair[0].order_id.as_bytes() >= pair[1].order_id.as_bytes())
    {
        return Err(AuditError::Shape("checkpoint members sorted and unique"));
    }
    let mut f = Frames::tagged(ROLLUP_TAG);
    f.u16("format_version", *format_version)?;
    f.uuid("audit_tenant_id", Some(*audit_tenant_id))?;
    f.positive("checkpoint_sequence", Some(*checkpoint_sequence))?;
    f.instant("captured_at", *captured_at)?;
    f.count("member_count", *member_count)?;
    f.digest("prev_checkpoint_hash", Some(prev_checkpoint_hash))?;
    for CheckpointMember {
        order_id,
        audit_sequence,
        entry_hash,
    } in members
    {
        f.uuid("member.order_id", Some(*order_id))?;
        f.positive("member.audit_sequence", Some(*audit_sequence))?;
        f.digest("member.entry_hash", Some(entry_hash))?;
    }
    Ok(f.finish())
}

/// Checkpoint digest.
///
/// # Errors
/// As [`checkpoint_preimage`].
pub fn checkpoint_hash(
    header: &CheckpointHeader,
    members: &[CheckpointMember],
) -> Result<Digest, AuditError> {
    Ok(sha256(&checkpoint_preimage(header, members)?))
}

/// Streaming roll-up digest for the checkpoint worker (S2-11): the header is framed first, then
/// each member as it is streamed in ascending binary `order_id` order, so a namespace of any size
/// is hashed in bounded memory. The bytes are exactly [`checkpoint_preimage`]'s; `finish`
/// refuses a member count that differs from the header, exactly as the slice form does.
#[domain_model]
pub struct CheckpointDigest {
    context: Sha256,
    expected: i64,
    pushed: i64,
    last: Option<Uuid>,
}
impl std::fmt::Debug for CheckpointDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointDigest")
            .field("expected", &self.expected)
            .field("pushed", &self.pushed)
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}
impl CheckpointDigest {
    /// Frame the header. The member count it declares must be reached exactly by `push`.
    ///
    /// # Errors
    /// Unsupported format, a non-positive sequence, a malformed instant or a wrong-length
    /// predecessor digest.
    pub fn new(header: &CheckpointHeader) -> Result<Self, AuditError> {
        let CheckpointHeader {
            format_version,
            audit_tenant_id,
            checkpoint_sequence,
            captured_at,
            member_count,
            prev_checkpoint_hash,
        } = header;
        if *format_version != CHECKPOINT_FORMAT_VERSION {
            return Err(AuditError::UnsupportedVersion(*format_version));
        }
        if *member_count < 0 {
            return Err(AuditError::Shape("checkpoint member count"));
        }
        let mut f = Frames::tagged(ROLLUP_TAG);
        f.u16("format_version", *format_version)?;
        f.uuid("audit_tenant_id", Some(*audit_tenant_id))?;
        f.positive("checkpoint_sequence", Some(*checkpoint_sequence))?;
        f.instant("captured_at", *captured_at)?;
        f.count("member_count", *member_count)?;
        f.digest("prev_checkpoint_hash", Some(prev_checkpoint_hash))?;
        let mut context = Sha256::new();
        context.update(f.finish());
        Ok(Self {
            context,
            expected: *member_count,
            pushed: 0,
            last: None,
        })
    }
    /// Frame the next member; members must arrive strictly ascending by binary `order_id`.
    ///
    /// # Errors
    /// Unsorted or duplicate member, a non-positive sequence, a wrong-length digest, or more
    /// members than the header declares.
    pub fn push(&mut self, member: &CheckpointMember) -> Result<(), AuditError> {
        if self
            .last
            .is_some_and(|last| last.as_bytes() >= member.order_id.as_bytes())
        {
            return Err(AuditError::Shape("checkpoint members sorted and unique"));
        }
        if self.pushed >= self.expected {
            return Err(AuditError::Shape("checkpoint member count"));
        }
        let CheckpointMember {
            order_id,
            audit_sequence,
            entry_hash,
        } = member;
        let mut f = Frames::default();
        f.uuid("member.order_id", Some(*order_id))?;
        f.positive("member.audit_sequence", Some(*audit_sequence))?;
        f.digest("member.entry_hash", Some(entry_hash))?;
        self.context.update(f.finish());
        self.pushed += 1;
        self.last = Some(*order_id);
        Ok(())
    }
    /// The number of members framed so far.
    #[must_use]
    pub fn pushed(&self) -> i64 {
        self.pushed
    }
    /// The checkpoint digest once exactly the declared members were framed.
    ///
    /// # Errors
    /// Fewer members than the header declares.
    pub fn finish(self) -> Result<Digest, AuditError> {
        if self.pushed != self.expected {
            return Err(AuditError::Shape("checkpoint member count"));
        }
        let mut out = [0u8; DIGEST_LEN];
        out.copy_from_slice(self.context.finalize().as_ref());
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// Actor identity (D-96/D-103/D-115)

/// The closed actor class; grants nothing.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditActorClass {
    /// The configured Orders worker actor.
    System,
    /// A configured Workflow, Subscriptions or Billing service principal.
    Service,
    /// Every other authenticated subject.
    User,
}
impl AuditActorClass {
    /// Stored `actor_class` token.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Service => "service",
            Self::User => "user",
        }
    }
}

/// A configured service role (D-115).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRole {
    /// The Workflow service principal.
    Workflow,
    /// The Subscriptions service principal.
    Subscriptions,
    /// The Billing service principal.
    Billing,
}

/// An authenticated principal tuple; matched on both subject and subject tenant.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Principal {
    /// Authenticated subject identifier.
    pub subject_id: Uuid,
    /// Tenant of the subject.
    pub subject_tenant_id: Uuid,
}

/// Configured identities the actor class is derived from; never from the PDP path, the
/// permission column or caller input.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ActorIdentities {
    system: Option<Principal>,
    services: Vec<(ServiceRole, Principal)>,
}
impl ActorIdentities {
    /// # Errors
    /// Nil identities, no Workflow principal, duplicates, or a service principal equal to the
    /// system actor.
    pub fn new(
        system: Option<Principal>,
        services: impl IntoIterator<Item = (ServiceRole, Principal)>,
    ) -> Result<Self, AuditError> {
        let services: Vec<_> = services.into_iter().collect();
        let nil = |p: &Principal| p.subject_id.is_nil() || p.subject_tenant_id.is_nil();
        if system.as_ref().is_some_and(nil) || services.iter().any(|(_, p)| nil(p)) {
            return Err(AuditError::Identities("nil principal"));
        }
        if !services.iter().any(|(r, _)| *r == ServiceRole::Workflow) {
            return Err(AuditError::Identities("at least one workflow principal"));
        }
        let mut seen = std::collections::HashSet::new();
        if !services.iter().all(|(_, p)| seen.insert(*p)) {
            return Err(AuditError::Identities("duplicate service principal"));
        }
        if system.is_some_and(|s| seen.contains(&s)) {
            return Err(AuditError::Identities(
                "service principals are disjoint from the system actor",
            ));
        }
        Ok(Self { system, services })
    }
    /// Classify the authenticated subject.
    ///
    /// # Errors
    /// `UnstableActor` for an anonymous/nil identity.
    pub fn classify(&self, ctx: &SecurityContext) -> Result<AuditActor, AuditError> {
        let principal = Principal {
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
        };
        if principal.subject_id.is_nil() || principal.subject_tenant_id.is_nil() {
            return Err(AuditError::UnstableActor);
        }
        Ok(AuditActor {
            principal,
            class: self.class_of(principal),
        })
    }
    /// The configured class of the authenticated subject, without the stable-identity check
    /// [`Self::classify`] adds for the audit row: a nil principal never matches a configured
    /// (non-nil) identity and so reads as `User`. The one place the configured identities
    /// decide a class (D-115, D-226).
    #[must_use]
    pub fn configured_class(&self, ctx: &SecurityContext) -> AuditActorClass {
        self.class_of(Principal {
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
        })
    }
    fn class_of(&self, principal: Principal) -> AuditActorClass {
        if self.system == Some(principal) {
            AuditActorClass::System
        } else if self.services.iter().any(|(_, p)| *p == principal) {
            AuditActorClass::Service
        } else {
            AuditActorClass::User
        }
    }
    /// The configured role of a service subject (for guards such as 06's Workflow check).
    #[must_use]
    pub fn service_role(&self, ctx: &SecurityContext) -> Option<ServiceRole> {
        let principal = Principal {
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
        };
        self.services
            .iter()
            .find(|(_, p)| *p == principal)
            .map(|(r, _)| *r)
    }
}

/// The trusted actor of one attempt: opaque subject UUID, home tenant and class.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditActor {
    principal: Principal,
    class: AuditActorClass,
}
impl AuditActor {
    /// Tenant of the authenticated subject.
    #[must_use]
    pub fn subject_tenant_id(&self) -> Uuid {
        self.principal.subject_tenant_id
    }
    /// Derived actor class.
    #[must_use]
    pub fn class(&self) -> AuditActorClass {
        self.class
    }
    /// Immutable lowercase hyphenated subject UUID (D-103); never a name or label.
    #[must_use]
    pub fn reference(&self) -> String {
        self.principal.subject_id.hyphenated().to_string()
    }
}

// ---------------------------------------------------------------------------------------------
// Minimization (D-96)

/// Secret-named parameters that mark a credential-bearing reference.
const SECRET_NAMES: [&str; 11] = [
    "token=",
    "secret=",
    "password=",
    "passwd=",
    "pwd=",
    "api_key=",
    "apikey=",
    "sig=",
    "signature=",
    "credential=",
    "authorization=",
];

/// Well-known bearer-token and API-key prefixes (D-221): GitHub (`ghp_`, `gho_`, `ghs_`,
/// `github_pat_`), GitLab (`glpat-`), Stripe (`sk_live_`, `sk_test_`, `rk_live_`) and Slack
/// (`xoxa-`, `xoxb-`, `xoxp-`, `xoxr-`), each followed by at least [`MIN_TOKEN_BODY`] token
/// characters. Case-sensitive, as the issuers define them.
const TOKEN_PREFIXES: [&str; 12] = [
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "xoxr-",
];
/// The shortest token body after a [`TOKEN_PREFIXES`] entry that counts as a secret.
const MIN_TOKEN_BODY: usize = 16;

/// Whether `value` contains a well-known opaque secret token (D-221, amending D-202): a
/// [`TOKEN_PREFIXES`] entry with a token body, an AWS access key id (`AKIA`/`ASIA` plus 16
/// upper-case letters or digits) or a Google API key (`AIza` plus 35 token characters). A token
/// counts only where it starts the value or follows a non-alphanumeric separator.
fn carries_known_token(value: &str) -> bool {
    let token_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let bytes = value.as_bytes();
    let mut starts = (0..bytes.len()).filter(|&i| i == 0 || !bytes[i - 1].is_ascii_alphanumeric());
    starts.any(|i| {
        let rest = &bytes[i..];
        let body = |prefix: &str| {
            rest.strip_prefix(prefix.as_bytes()).map_or(0, |tail| {
                tail.iter().take_while(|b| token_char(**b)).count()
            })
        };
        let aws = ["AKIA", "ASIA"].iter().any(|prefix| {
            rest.strip_prefix(prefix.as_bytes()).is_some_and(|tail| {
                tail.len() >= 16
                    && tail[..16]
                        .iter()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                    && tail.get(16).is_none_or(|b| !b.is_ascii_alphanumeric())
            })
        });
        TOKEN_PREFIXES
            .iter()
            .any(|prefix| body(prefix) >= MIN_TOKEN_BODY)
            || aws
            || body("AIza") >= 35
    })
}

/// Whether an opaque reference embeds credential material: compact JWS/JWE/JWT, PEM armor,
/// URL user-info, a secret-named parameter or a well-known opaque token (D-202, D-221).
/// References name proofs; they never carry them. A denylist: the allowlisted reference grammar
/// belongs to the PDP policy owners (D-221).
#[must_use]
pub fn carries_credential(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let b64url = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
    };
    let parts: Vec<&str> = value.split('.').collect();
    let compact_jose = matches!(parts.len(), 3 | 5)
        && parts[0].starts_with("eyJ")
        && parts.iter().take(2).all(|p| b64url(p));
    let pem = lower.contains("-----begin");
    let userinfo = lower.split_once("://").is_some_and(|(_, rest)| {
        rest.split('/')
            .next()
            .is_some_and(|auth| auth.contains('@'))
    });
    compact_jose
        || pem
        || userinfo
        || SECRET_NAMES.iter().any(|n| lower.contains(n))
        || carries_known_token(value)
}

/// Validate a supplied delegation proof reference for evidence storage; the boundary maps a
/// refusal to `request-invalid` before authorization (S2-12).
///
/// # Errors
/// `CredentialInReference`.
pub fn validate_proof_reference(proof: &DelegationProofRef) -> Result<String, AuditError> {
    if carries_credential(proof.as_str()) {
        return Err(AuditError::CredentialInReference);
    }
    Ok(proof.as_str().to_owned())
}

/// Validate caller text before storage: 1..=4096 characters and no control characters other
/// than tab/line breaks. Stored as received; never trimmed, folded or normalized.
///
/// # Errors
/// `NotMinimized("caller_reason")`.
pub fn validate_caller_reason(text: &str) -> Result<(), AuditError> {
    let chars = text.chars().count();
    if chars == 0
        || chars > MAX_CALLER_REASON_CHARS
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(AuditError::NotMinimized("caller_reason"));
    }
    Ok(())
}

/// The closed administrative-field allowlist (`orders_order_admin`/`orders_order_line_admin`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAttribute {
    /// `external_reference`, stored verbatim after validation.
    ExternalReference,
    /// `display_label`, stored as a keyed digest.
    DisplayLabel,
    /// `internal_notes`, stored as a keyed digest.
    InternalNotes,
}
impl AdminAttribute {
    /// Every allowlisted attribute.
    pub const ALL: [Self; 3] = [
        Self::ExternalReference,
        Self::DisplayLabel,
        Self::InternalNotes,
    ];
    /// Column token of the attribute.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::ExternalReference => "external_reference",
            Self::DisplayLabel => "display_label",
            Self::InternalNotes => "internal_notes",
        }
    }
    /// # Errors
    /// `NotMinimized("changed_field")` for a field outside the allowlist.
    pub fn from_token(token: &str) -> Result<Self, AuditError> {
        Self::ALL
            .into_iter()
            .find(|a| a.token() == token)
            .ok_or(AuditError::NotMinimized("changed_field"))
    }
    /// The audit representation of one value. NULL stays NULL.
    fn minimize(
        self,
        key: &AdminTextKey,
        value: Option<&str>,
    ) -> Result<Option<String>, AuditError> {
        let Some(value) = value else {
            return Ok(None);
        };
        match self {
            Self::ExternalReference => {
                if value.is_empty()
                    || value.chars().count() > MAX_EXTERNAL_REFERENCE_CHARS
                    || value.chars().any(char::is_control)
                    || carries_credential(value)
                {
                    return Err(AuditError::NotMinimized("external_reference"));
                }
                Ok(Some(value.to_owned()))
            }
            Self::DisplayLabel | Self::InternalNotes => Ok(Some(minimized_text_digest(key, value))),
        }
    }
}

/// Minimum `audit_minimization.key` length in bytes: the HMAC-SHA256 output size (RFC 2104 §3).
pub const MIN_ADMIN_TEXT_KEY_BYTES: usize = 32;
/// Upper bound of the configured key, which is never truncated or pre-hashed silently.
pub const MAX_ADMIN_TEXT_KEY_BYTES: usize = 1024;
/// Upper bound of the configured key identifier.
pub const MAX_ADMIN_TEXT_KEY_ID_CHARS: usize = 64;

/// Why a configured administrative-text key is refused at startup. Never carries key material.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdminTextKeyError {
    /// The key identifier is empty, too long or has disallowed characters.
    #[error("key_id must be 1..=64 characters of ASCII letters, digits, '.', '_' or '-'")]
    KeyId,
    /// The key is shorter than 32 or longer than 1024 bytes.
    #[error("key must be 32..=1024 bytes")]
    KeyLength,
    /// The configured crypto backend could not initialize the HMAC key.
    #[error("audit crypto backend failed: {0}")]
    Crypto(CryptoError),
}

/// The keyed minimization secret for `display_label`/`internal_notes` (D-204, amending D-96's
/// interim). Built once at startup from required configuration; `Debug` prints only the key ID.
#[domain_model]
pub struct AdminTextKey {
    key_id: String,
    key: HmacSha256,
}
impl AdminTextKey {
    /// # Errors
    /// A malformed key ID, or a key shorter than [`MIN_ADMIN_TEXT_KEY_BYTES`] or longer than
    /// [`MAX_ADMIN_TEXT_KEY_BYTES`], or an unavailable crypto backend.
    pub fn new(key_id: &str, key: &[u8]) -> Result<Self, AdminTextKeyError> {
        if key_id.is_empty()
            || key_id.len() > MAX_ADMIN_TEXT_KEY_ID_CHARS
            || !key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(AdminTextKeyError::KeyId);
        }
        if !(MIN_ADMIN_TEXT_KEY_BYTES..=MAX_ADMIN_TEXT_KEY_BYTES).contains(&key.len()) {
            return Err(AdminTextKeyError::KeyLength);
        }
        Ok(Self {
            key_id: key_id.to_owned(),
            key: HmacSha256::new(key).map_err(AdminTextKeyError::Crypto)?,
        })
    }
    /// The non-secret identifier recorded in every value this key produces.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}
impl std::fmt::Debug for AdminTextKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminTextKey")
            .field("key_id", &self.key_id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// The single minimized representation of personal-minimal free text in audit (D-204):
/// `hmac-sha256:v1:<key_id>:<hex>`, an HMAC-SHA256 tag of the exact UTF-8 bytes under the
/// configured key. Without the key a short label cannot be recovered by dictionary search;
/// it is still pseudonymization, not anonymization (Privacy/Legal sign-off remains under
/// `upreq-audit-identity-lifecycle`). The key ID names the key so rotation never makes two
/// keys' tags look comparable. Audit verification hashes the stored text and never needs the
/// key. A further replacement changes only this function, not the row shape.
///
/// # Panics
/// Panics if the crypto backend is unavailable or text exceeds the portable MAC limit.
#[must_use]
pub fn minimized_text_digest(key: &AdminTextKey, value: &str) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    const PREFIX: &str = "hmac-sha256:v1:";
    let tag = key
        .key
        .sign(value.as_bytes())
        .unwrap_or_else(|error| panic!("administrative text MAC failed: {error}"));
    let mut out = String::with_capacity(PREFIX.len() + key.key_id.len() + 1 + 2 * DIGEST_LEN);
    out.push_str(PREFIX);
    out.push_str(&key.key_id);
    out.push(':');
    for byte in &tag {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}

/// One administrative field target.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminField {
    /// A field of the order itself.
    Order(AdminAttribute),
    /// Named `lines/<line_id>/<field>` (D-117).
    Line(Uuid, AdminAttribute),
}
impl AdminField {
    /// Stored `changed_field` token.
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Order(a) => a.token().to_owned(),
            Self::Line(line, a) => format!("lines/{}/{}", line.hyphenated(), a.token()),
        }
    }
    fn attribute(self) -> AdminAttribute {
        match self {
            Self::Order(a) | Self::Line(_, a) => a,
        }
    }
}

/// One changed administrative value, before minimization.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminChange {
    /// Changed field.
    pub field: AdminField,
    /// Value before the change.
    pub prior: Option<String>,
    /// Value after the change.
    pub new: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Writer row shapes

/// The trigger recorded on an audit row: a public trigger or the D-201 internal writer token.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditTrigger {
    /// A public trigger.
    Public(Trigger),
    /// The internal fulfillment-grant rebuild writer (D-201).
    ReplaceFulfillmentGrant,
}
impl AuditTrigger {
    /// Stored trigger token.
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Public(t) => t.token().to_owned(),
            Self::ReplaceFulfillmentGrant => REPLACE_FULFILLMENT_GRANT.to_owned(),
        }
    }
}

/// Whether a committed trigger records caller text (D-143).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerReasonRule {
    /// The trigger records no caller text.
    Forbidden,
    /// Caller text is recorded when given.
    Optional,
    /// Caller text is required.
    Required,
}

/// The caller-text rule of a committed trigger (D-143).
fn caller_reason_rule(trigger: AuditTrigger) -> CallerReasonRule {
    match trigger {
        AuditTrigger::Public(
            Trigger::Cancel
            | Trigger::CancelWorkflowMediated
            | Trigger::Amendment
            | Trigger::AcknowledgeFailed
            | Trigger::ForceFailUnreconciled,
        ) => CallerReasonRule::Required,
        AuditTrigger::Public(Trigger::Hold) => CallerReasonRule::Optional,
        _ => CallerReasonRule::Forbidden,
    }
}

/// Facts of the resolved aggregate, read from the locked row only.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderFacts {
    /// Order identifier.
    pub order_id: Uuid,
    /// Immutable chain namespace frozen at create.
    pub audit_tenant_id: Uuid,
    /// Current resource-tenant snapshot.
    pub resource_tenant_id: Uuid,
    /// Current order state.
    pub state: OrderState,
    /// Current order version.
    pub version: i32,
    /// `orders_order.audit_sequence`: the last allocated committed sequence.
    pub audit_sequence: i64,
}

/// Trusted per-attempt evidence common to every row the attempt writes.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AttemptEvidence {
    actor: AuditActor,
    delegation_proof_ref: Option<String>,
    idempotency_key: String,
    correlation_id: Option<Uuid>,
    created_at: OffsetDateTime,
}

/// A committed entry awaiting its sequence and predecessor under the aggregate lock.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommitted {
    row: AuditRow,
}
impl PendingCommitted {
    /// Resolved order the entry is bound to.
    #[must_use]
    pub fn order_id(&self) -> Option<Uuid> {
        self.row.order_id
    }
    /// Immutable audit namespace of the entry.
    #[must_use]
    pub fn audit_tenant_id(&self) -> Option<Uuid> {
        self.row.audit_tenant_id
    }
    /// Seal with the allocated sequence and the predecessor digest (genesis for sequence 1).
    ///
    /// # Errors
    /// Shape or encoding failure.
    pub fn seal(mut self, sequence: i64, prev_hash: Digest) -> Result<SealedAudit, AuditError> {
        self.row.sequence = Some(sequence);
        self.row.prev_hash = Some(prev_hash.to_vec());
        SealedAudit::seal(self.row)
    }
}

/// A complete row and the digest of exactly those values; only constructible by sealing.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedAudit {
    row: AuditRow,
    entry_hash: Digest,
}
impl SealedAudit {
    fn seal(row: AuditRow) -> Result<Self, AuditError> {
        if row.hash_version != HashVersion::CURRENT.stored() {
            return Err(AuditError::UnsupportedVersion(row.hash_version));
        }
        let entry_hash = entry_hash(&row)?;
        Ok(Self { row, entry_hash })
    }
    /// The sealed row.
    #[must_use]
    pub fn row(&self) -> &AuditRow {
        &self.row
    }
    /// Digest of the sealed row.
    #[must_use]
    pub fn entry_hash(&self) -> Digest {
        self.entry_hash
    }
    /// The sealed row and its digest.
    #[must_use]
    pub fn into_parts(self) -> (AuditRow, Digest) {
        (self.row, self.entry_hash)
    }
}

impl AttemptEvidence {
    /// # Errors
    /// Credential-bearing proof reference or an out-of-range instant.
    pub fn new(
        actor: AuditActor,
        delegation_proof_ref: Option<&DelegationProofRef>,
        idempotency_key: &IdempotencyKey,
        correlation_id: Option<Uuid>,
        created_at: OffsetDateTime,
    ) -> Result<Self, AuditError> {
        Ok(Self {
            actor,
            delegation_proof_ref: delegation_proof_ref
                .map(validate_proof_reference)
                .transpose()?,
            idempotency_key: String::from(idempotency_key.clone()),
            correlation_id,
            created_at: normalize_instant(created_at)?,
        })
    }
    /// Normalized write instant of the attempt.
    #[must_use]
    pub fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }
    /// Authenticated actor of the attempt.
    #[must_use]
    pub fn actor(&self) -> AuditActor {
        self.actor
    }

    fn base(
        &self,
        audit_id: Uuid,
        trigger: AuditTrigger,
        outcome: &str,
        reason: String,
    ) -> AuditRow {
        AuditRow {
            hash_version: HashVersion::CURRENT.stored(),
            audit_id,
            audit_tenant_id: None,
            subject_tenant_id: self.actor.subject_tenant_id(),
            resource_tenant_id: None,
            order_id: None,
            requested_order_ref: None,
            sequence: None,
            from_state: None,
            to_state: None,
            trigger: trigger.token(),
            outcome: outcome.to_owned(),
            actor: self.actor.reference(),
            actor_class: self.actor.class().token().to_owned(),
            delegation_proof_ref: self.delegation_proof_ref.clone(),
            reason,
            changed_field: None,
            prior_value: None,
            new_value: None,
            idempotency_key: self.idempotency_key.clone(),
            correlation_id: self.correlation_id,
            version: None,
            created_at: self.created_at,
            prev_hash: None,
            caller_reason: None,
            force_request_observation: None,
        }
    }

    fn resolved(row: &mut AuditRow, from: &OrderFacts, to: &OrderFacts) {
        row.audit_tenant_id = Some(from.audit_tenant_id);
        row.resource_tenant_id = Some(to.resource_tenant_id);
        row.order_id = Some(from.order_id);
        row.from_state = Some(from.state.token().to_owned());
        row.to_state = Some(to.state.token().to_owned());
        row.version = Some(to.version);
    }

    /// Committed create: sequence 1 into `draft` at version 1, linked to the inserted aggregate,
    /// whose audit namespace is frozen to the authorized resource tenant.
    ///
    /// # Errors
    /// `Shape` when the inserted aggregate is not a fresh draft with a frozen namespace.
    pub fn committed_create(
        &self,
        audit_id: Uuid,
        created: &OrderFacts,
    ) -> Result<PendingCommitted, AuditError> {
        if created.state != OrderState::Draft
            || created.version != 1
            || created.audit_sequence != 0
            || created.audit_tenant_id != created.resource_tenant_id
        {
            return Err(AuditError::Shape(
                "create freezes the namespace on a fresh version-1 draft",
            ));
        }
        let trigger = AuditTrigger::Public(Trigger::Create);
        let mut row = self.base(audit_id, trigger, COMMITTED, trigger.token());
        Self::resolved(&mut row, created, created);
        row.from_state = None;
        Ok(PendingCommitted { row })
    }

    /// Ordinary committed transition (any trigger except create and administrative edit),
    /// observed before and after the mutation on the same locked aggregate.
    ///
    /// # Errors
    /// Wrong trigger, aggregate/namespace mismatch, or caller text the trigger does not carry.
    pub fn committed_transition(
        &self,
        audit_id: Uuid,
        trigger: AuditTrigger,
        before: &OrderFacts,
        after: &OrderFacts,
        caller_reason: Option<&str>,
    ) -> Result<PendingCommitted, AuditError> {
        if matches!(
            trigger,
            AuditTrigger::Public(Trigger::Create | Trigger::AdministrativeEdit)
        ) {
            return Err(AuditError::Shape(
                "create/administrative edit use their own shapes",
            ));
        }
        if before.order_id != after.order_id || before.audit_tenant_id != after.audit_tenant_id {
            return Err(AuditError::Shape("one aggregate and frozen namespace"));
        }
        match (caller_reason_rule(trigger), caller_reason) {
            (CallerReasonRule::Forbidden, Some(_)) => {
                return Err(AuditError::NotMinimized("caller_reason"));
            }
            (CallerReasonRule::Required, None) => {
                return Err(AuditError::Shape("caller_reason required"));
            }
            (_, Some(text)) => validate_caller_reason(text)?,
            (_, None) => {}
        }
        // 06 §4.4 (D-136): the failed acknowledgement records the closed failure reason.
        if trigger == AuditTrigger::Public(Trigger::AcknowledgeFailed)
            && !caller_reason
                .and_then(FailureReason::from_token)
                .is_some_and(FailureReason::acknowledgeable)
        {
            return Err(AuditError::NotMinimized("caller_reason"));
        }
        let mut row = self.base(audit_id, trigger, COMMITTED, trigger.token());
        Self::resolved(&mut row, before, after);
        row.requested_order_ref = Some(before.order_id);
        row.caller_reason = caller_reason.map(str::to_owned);
        Ok(PendingCommitted { row })
    }

    /// One committed entry per changed administrative field, in request order (D-117); unchanged
    /// fields beside changed ones produce no entry. Values are minimized with the configured
    /// key (D-204) before hashing.
    ///
    /// # Errors
    /// No change, an unchanged value, a field outside the allowlist or an unminimizable value.
    pub fn administrative_edits(
        &self,
        key: &AdminTextKey,
        order: &OrderFacts,
        changes: &[AdminChange],
        mut audit_id: impl FnMut() -> Uuid,
    ) -> Result<Vec<PendingCommitted>, AuditError> {
        let trigger = AuditTrigger::Public(Trigger::AdministrativeEdit);
        let mut rows = Vec::with_capacity(changes.len());
        for change in changes {
            if change.prior == change.new {
                continue;
            }
            let attribute = change.field.attribute();
            let prior = attribute.minimize(key, change.prior.as_deref())?;
            let new = attribute.minimize(key, change.new.as_deref())?;
            if prior == new {
                return Err(AuditError::Shape(
                    "administrative change survives minimization",
                ));
            }
            let mut row = self.base(audit_id(), trigger, COMMITTED, trigger.token());
            Self::resolved(&mut row, order, order);
            row.requested_order_ref = Some(order.order_id);
            row.changed_field = Some(change.field.token());
            row.prior_value = prior;
            row.new_value = new;
            rows.push(PendingCommitted { row });
        }
        if rows.is_empty() {
            return Err(AuditError::Shape(
                "administrative edit changes at least one field",
            ));
        }
        Ok(rows)
    }

    /// Resolved business refusal: known order, observed state/version, no sequence or chain. A
    /// `second-approver-required` force request records the D-201 observation of the lock.
    ///
    /// # Errors
    /// Shape failure (for example an aggregate without a committed create).
    pub fn resolved_refusal(
        &self,
        audit_id: Uuid,
        trigger: AuditTrigger,
        order: &OrderFacts,
        reason: Reason,
    ) -> Result<SealedAudit, AuditError> {
        let mut row = self.base(
            audit_id,
            trigger,
            REFUSED,
            reason.mapping().reason.to_owned(),
        );
        Self::resolved(&mut row, order, order);
        row.requested_order_ref = Some(order.order_id);
        if trigger == AuditTrigger::Public(Trigger::ForceFailUnreconciled)
            && reason == Reason::SecondApproverRequired
        {
            row.force_request_observation = Some(ForceRequestObservation {
                audit_sequence: order.audit_sequence,
                state: order.state.token().to_owned(),
                version: order.version,
            });
        }
        SealedAudit::seal(row)
    }

    /// Unresolved refusal (early denial, unknown target, refused create): only the validated
    /// requested identifier, no target lookup/enrichment; every resolved fact stays NULL.
    ///
    /// # Errors
    /// Shape failure (an order-targeted attempt without its requested identifier).
    pub fn unresolved_refusal(
        &self,
        audit_id: Uuid,
        trigger: AuditTrigger,
        requested_order_ref: Option<Uuid>,
        reason: Reason,
    ) -> Result<SealedAudit, AuditError> {
        let mut row = self.base(
            audit_id,
            trigger,
            REFUSED,
            reason.mapping().reason.to_owned(),
        );
        row.requested_order_ref = requested_order_ref;
        SealedAudit::seal(row)
    }
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;

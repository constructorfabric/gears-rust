//! Idempotency registry rules: key scope, request fingerprint, retention class, gate
//! classification, execution fencing and the immutable settled-response snapshot.
//!
//! Sources: Foundation §4.2 (key scope, fingerprint, retention, lease recovery), §3.6 common
//! transactional gate and claim/reclaim mechanics, D-173 (workflow-class retention), D-188
//! (commercial attempts) and D-198 (staged receiver controls). Pure rules only; the locked
//! transactional gate is `infra::storage::repo::idempotency`.
use std::collections::BTreeMap;

use bss_orders_lifecycle_sdk::catalog::{Reason, Trigger};
use bss_orders_lifecycle_sdk::models::IdempotencyKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Duration, OffsetDateTime};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Ordinary response-retention window (Foundation §4.2).
pub const ORDINARY_RETENTION: Duration = Duration::hours(24);
/// D-173: workflow-class triggers retain receipts for at least 30 days.
pub const WORKFLOW_RETENTION: Duration = Duration::days(30);

/// The exact workflow-trigger class (DESIGN §4.1, D-110/D-173): the triggers of the five
/// Workflow seam operations. `force-fail-unreconciled` is deliberately absent (D-182). The
/// D-201 internal `replace-fulfillment-grant` operation is not a trigger but shares the class
/// (D-203, [`RegistryOperation::retention`]).
pub const WORKFLOW_CLASS: [Trigger; 9] = [
    Trigger::ReflectApprovalRequired,
    Trigger::ReflectApprovalNotRequired,
    Trigger::ReflectApprovalGranted,
    Trigger::ReflectApprovalDenied,
    Trigger::BeginFulfillment,
    Trigger::ReportSpawnSignal,
    Trigger::AcknowledgeCompleted,
    Trigger::AcknowledgeFailed,
    Trigger::CancelWorkflowMediated,
];

/// D-201 internal engine continuation token; not a public state-machine trigger.
pub const REPLACE_FULFILLMENT_GRANT: &str = "replace-fulfillment-grant";

/// Registry operation: the engine-entering transition the key scopes. The stored token is the
/// trigger token, which is also the operation recorded by D-188 attempts and D-198 controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryOperation {
    Trigger(Trigger),
    /// D-201 `replace_fulfillment_grant` internal `IdempotentWrite`.
    ReplaceFulfillmentGrant,
}
impl RegistryOperation {
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Trigger(trigger) => trigger_token(trigger),
            Self::ReplaceFulfillmentGrant => REPLACE_FULFILLMENT_GRANT.to_owned(),
        }
    }
    /// Operation-specific retention: workflow-class 30 days, everything else 24 hours.
    /// The D-201 internal rebuild is Workflow-issued and retried under the same key, so it
    /// joins the workflow class (D-203).
    #[must_use]
    pub fn retention(self) -> RetentionClass {
        match self {
            Self::Trigger(trigger) if WORKFLOW_CLASS.contains(&trigger) => RetentionClass::Workflow,
            Self::ReplaceFulfillmentGrant => RetentionClass::Workflow,
            Self::Trigger(_) => RetentionClass::Ordinary,
        }
    }
    #[must_use]
    pub fn is_create(self) -> bool {
        self == Self::Trigger(Trigger::Create)
    }
}

/// The registered trigger token (the SDK's serde name).
#[must_use]
pub fn trigger_token(trigger: Trigger) -> String {
    match serde_json::to_value(trigger) {
        Ok(Value::String(token)) => token,
        // The SDK enum serializes every variant as its registered string token.
        _ => unreachable!("trigger tokens are strings"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionClass {
    Ordinary,
    Workflow,
}
impl RetentionClass {
    /// The absolute window persisted at claim time; replay/reclaim/settlement never extend it.
    #[must_use]
    pub fn window(self) -> Duration {
        match self {
            Self::Ordinary => ORDINARY_RETENTION,
            Self::Workflow => WORKFLOW_RETENTION,
        }
    }
}

/// Positive finite lease (`idempotency_lease_duration`); no implicit default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseDuration(Duration);
impl LeaseDuration {
    /// # Errors
    /// Zero or more than one day (the configuration bound) is rejected.
    pub fn from_seconds(seconds: u32) -> Result<Self, RegistryRuleError> {
        if seconds == 0 || seconds > 86_400 {
            return Err(RegistryRuleError::InvalidLease);
        }
        Ok(Self(Duration::seconds(i64::from(seconds))))
    }
    #[must_use]
    pub fn duration(self) -> Duration {
        self.0
    }
}

/// Stable authorized principal: the platform-asserted subject tenant and subject identifier.
/// Never session/token/`jti`/proof/replica/transport identity (Foundation §4.2).
#[derive(Clone, PartialEq, Eq)]
pub struct PrincipalScope(String);
impl PrincipalScope {
    /// # Errors
    /// An anonymous or nil identity cannot scope a key; substituting a per-connection value
    /// would turn every retry into a new execution.
    pub fn from_context(ctx: &SecurityContext) -> Result<Self, RegistryRuleError> {
        if ctx.is_anonymous() || ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
            return Err(RegistryRuleError::UnstablePrincipal);
        }
        Ok(Self(format!(
            "{}/{}",
            ctx.subject_tenant_id(),
            ctx.subject_id()
        )))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for PrincipalScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrincipalScope([redacted])")
    }
}

/// `(operation, principal_scope, idempotency_key)`: the complete registry identity.
#[derive(Clone, Debug)]
pub struct RegistryKey {
    operation: RegistryOperation,
    principal: PrincipalScope,
    key: IdempotencyKey,
}
impl RegistryKey {
    #[must_use]
    pub fn new(
        operation: RegistryOperation,
        principal: PrincipalScope,
        key: IdempotencyKey,
    ) -> Self {
        Self {
            operation,
            principal,
            key,
        }
    }
    #[must_use]
    pub fn operation(&self) -> RegistryOperation {
        self.operation
    }
    #[must_use]
    pub fn principal(&self) -> &PrincipalScope {
        &self.principal
    }
    /// Exact, unnormalized key text.
    #[must_use]
    pub fn key_text(&self) -> String {
        String::from(self.key.clone())
    }
}

/// Request target: the create sentinel or a resolved order with its expected version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintTarget {
    Create,
    Order {
        order_id: Uuid,
        expected_version: i32,
    },
}
/// `expected_draft_revision` for draft writes/submit, otherwise the not-applicable sentinel
/// (including a `draft-mutate` request that omits it, D-147).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftRevisionInput {
    Expected(i64),
    NotApplicable,
}
/// The three tenant axes in force for the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "Named exactly after the three registered tenant-axis properties"
)]
pub struct FingerprintAxes {
    pub seller_tenant_id: Uuid,
    pub resource_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
}

/// Exactly the Foundation §4.2 input set. Correlation, request instant, transport headers,
/// principal/key and server-assigned values have no field here and cannot enter the hash.
#[derive(Debug, Clone)]
pub struct FingerprintInput<'a> {
    /// Public catalog operation identifier.
    pub operation: &'a str,
    pub trigger: Trigger,
    pub target: FingerprintTarget,
    pub axes: FingerprintAxes,
    pub draft_revision: DraftRevisionInput,
    /// Canonicalised typed document contribution; numbers must already be exact strings.
    pub contribution: &'a Value,
}

/// Request fingerprint profile v1 domain tag (adopted byte-for-byte from the S1-06 frozen
/// request profile; see the conformance vectors in `tests/fixtures`).
const REQUEST_TAG: &str = "VHP-BSS-ORDERS-REQUEST-FIXTURE-v1";
/// Stored fingerprint prefix; a future profile must use a different one.
const FINGERPRINT_PREFIX: &str = "rf1:";

impl FingerprintInput<'_> {
    /// Tagged canonical preimage bytes.
    ///
    /// # Errors
    /// An operation that is not a registered idempotent write, a create carrying a
    /// target/version, a non-create without them, or a contribution containing JSON numbers
    /// (the profile requires exact numeric strings).
    pub fn preimage(&self) -> Result<Vec<u8>, RegistryRuleError> {
        if !is_idempotent_write(self.operation) {
            return Err(RegistryRuleError::UnknownOperation);
        }
        let is_create = self.trigger == Trigger::Create;
        let (order_id, expected_version) = match (self.target, is_create) {
            (FingerprintTarget::Create, true) => (Value::Null, Value::Null),
            (
                FingerprintTarget::Order {
                    order_id,
                    expected_version,
                },
                false,
            ) if expected_version > 0 => (
                Value::String(order_id.to_string()),
                Value::String(expected_version.to_string()),
            ),
            _ => return Err(RegistryRuleError::InvalidTarget),
        };
        let draft = match self.draft_revision {
            DraftRevisionInput::Expected(n) if n >= 0 => Value::String(n.to_string()),
            DraftRevisionInput::Expected(_) => return Err(RegistryRuleError::InvalidTarget),
            DraftRevisionInput::NotApplicable => Value::Null,
        };
        let document = serde_json::json!({
            "operation": self.operation,
            "trigger": trigger_token(self.trigger),
            "order_id": order_id,
            "seller_tenant_id": self.axes.seller_tenant_id.to_string(),
            "resource_tenant_id": self.axes.resource_tenant_id.to_string(),
            "payer_tenant_id": self.axes.payer_tenant_id.to_string(),
            "expected_version": expected_version,
            "expected_draft_revision": draft,
            "contribution": self.contribution,
        });
        let mut out = REQUEST_TAG.as_bytes().to_vec();
        out.push(0x1f);
        out.extend(canonical(&document)?.as_bytes());
        Ok(out)
    }
    /// # Errors
    /// See [`Self::preimage`].
    pub fn fingerprint(&self) -> Result<Fingerprint, RegistryRuleError> {
        use std::fmt::Write;
        // FIPS-validated provider (DE0708), as the Types Registry admission fingerprint.
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &self.preimage()?);
        let mut hex = String::with_capacity(FINGERPRINT_PREFIX.len() + 64);
        hex.push_str(FINGERPRINT_PREFIX);
        for byte in digest.as_ref() {
            write!(hex, "{byte:02x}").map_err(|_| RegistryRuleError::NonCanonical)?;
        }
        Ok(Fingerprint(hex))
    }
}

/// D-201 internal SDK `IdempotentWrite` operation identifier (not a REST catalog route).
pub const REPLACE_FULFILLMENT_GRANT_OPERATION: &str = "replace_fulfillment_grant";

/// Internal worker operations (07 §3.1, 08 §3.5): the private expiry and draft auto-void
/// entries. They are not REST catalog routes; the worker derives their generation-bound keys.
pub const EXPIRE_OPERATION: &str = "expire";
pub const AUTO_VOID_OPERATION: &str = "auto_void";

/// A registered state-changing catalog operation (or an internal engine write: the D-201
/// rebuild and the two expiry workers). Reads and Preview never enter the transition registry,
/// and free text cannot reach the hash.
fn is_idempotent_write(operation: &str) -> bool {
    [
        REPLACE_FULFILLMENT_GRANT_OPERATION,
        EXPIRE_OPERATION,
        AUTO_VOID_OPERATION,
    ]
    .contains(&operation)
        || bss_orders_lifecycle_sdk::catalog::OPERATIONS
            .iter()
            .any(|o| o.id == operation && o.method != "GET" && o.id != "preview")
}

/// Restricted canonical JSON: keys sorted by UTF-16 code units, no insignificant whitespace,
/// exact strings for every number.
fn canonical(value: &Value) -> Result<String, RegistryRuleError> {
    let text = |v: &Value| serde_json::to_string(v).map_err(|_| RegistryRuleError::NonCanonical);
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => text(value),
        Value::Number(_) => Err(RegistryRuleError::NonCanonical),
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            let entries = entries
                .into_iter()
                .map(|(k, v)| {
                    Ok(format!(
                        "{}:{}",
                        text(&Value::String(k.clone()))?,
                        canonical(v)?
                    ))
                })
                .collect::<Result<Vec<_>, RegistryRuleError>>()?;
            Ok(format!("{{{}}}", entries.join(",")))
        }
    }
}

/// Stored as a hash, never as the payload: the registry holds no commercial content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(String);
impl Fingerprint {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Persisted execution identity plus live ownership fence (D-188/D-198).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOwner {
    pub execution_id: Uuid,
    pub owner_token: Uuid,
    pub fencing_generation: i64,
}

/// Checked monotonic fence increment; overflow refuses instead of wrapping.
///
/// # Errors
/// `FenceExhausted` at `i64::MAX`.
pub fn next_fence(current: i64) -> Result<i64, RegistryRuleError> {
    if current < 0 {
        return Err(RegistryRuleError::InvalidRecord);
    }
    current
        .checked_add(1)
        .ok_or(RegistryRuleError::FenceExhausted)
}

/// The locked record facts the gate classifies.
#[derive(Debug, Clone, Copy)]
pub struct RecordView<'a> {
    pub fingerprint: &'a str,
    pub settled: bool,
    pub lease_expires_at: Option<OffsetDateTime>,
    pub expires_at: OffsetDateTime,
    /// A linked D-188 attempt / D-198 control that is still unresolved.
    pub unresolved_execution: bool,
    pub owner: Option<ExecutionOwner>,
}

/// Common transactional gate decision (Foundation §3.6 table), evaluated at fresh DB time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Retention elapsed (and, for an in-flight record, its lease): the expired record may be
    /// deleted and replaced by a fresh execution in this transaction.
    ReplaceExpired,
    Mismatch,
    Replay,
    /// The presenting executor's own durable marker (D-188/D-198): not still-processing.
    CurrentOwner,
    StillProcessing,
    Reclaim,
}

/// Retention expiry first, then fingerprint before settlement/lease interpretation.
///
/// # Errors
/// An in-flight record without a lease is invalid (the schema CHECK forbids it).
pub fn classify(
    record: RecordView<'_>,
    fingerprint: &Fingerprint,
    now: OffsetDateTime,
    presented: Option<&ExecutionOwner>,
) -> Result<GateDecision, RegistryRuleError> {
    let lease = match (record.settled, record.lease_expires_at) {
        (true, _) => None,
        (false, Some(lease)) => Some(lease),
        (false, None) => return Err(RegistryRuleError::InvalidRecord),
    };
    let retention_over = record.expires_at <= now;
    let lease_over = lease.is_none_or(|lease| lease <= now);
    if retention_over && lease_over && !record.unresolved_execution {
        return Ok(GateDecision::ReplaceExpired);
    }
    if record.fingerprint != fingerprint.as_str() {
        return Ok(GateDecision::Mismatch);
    }
    if record.settled {
        return Ok(GateDecision::Replay);
    }
    if presented.is_some() && presented == record.owner.as_ref() {
        return Ok(GateDecision::CurrentOwner);
    }
    if lease_over {
        Ok(GateDecision::Reclaim)
    } else {
        Ok(GateDecision::StillProcessing)
    }
}

/// Settled response snapshot `formatVersion = 1`: status, public body, semantic headers and
/// the assessment result where gate evaluation was reached. Never transport credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredResponse {
    format_version: u8,
    pub status: u16,
    pub body: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Value>,
}
/// Semantic response headers a snapshot may carry (`ETag` = returned version, `Location` = created
/// order, `Content-Type`). Everything else, including credentials, cookies, proof references and
/// request-specific transport headers, is refused: the list is closed, not a denylist.
const SEMANTIC_HEADERS: [&str; 3] = ["content-type", "etag", "location"];
impl StoredResponse {
    pub const FORMAT_VERSION: u8 = 1;
    /// # Errors
    /// Non-HTTP status, a header outside the closed semantic set, a case-duplicated header or a
    /// value with control/non-ASCII characters.
    pub fn new(
        status: u16,
        body: Value,
        headers: BTreeMap<String, String>,
        assessment: Option<Value>,
    ) -> Result<Self, RegistryRuleError> {
        let response = Self {
            format_version: Self::FORMAT_VERSION,
            status,
            body,
            headers,
            assessment,
        };
        response.validate()?;
        Ok(response)
    }
    fn validate(&self) -> Result<(), RegistryRuleError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(RegistryRuleError::UnsupportedResponseFormat);
        }
        if !(200..=599).contains(&self.status) {
            return Err(RegistryRuleError::InvalidResponse);
        }
        let mut names = std::collections::BTreeSet::new();
        for (name, value) in &self.headers {
            let name = name.to_ascii_lowercase();
            if !SEMANTIC_HEADERS.contains(&name.as_str())
                || !names.insert(name)
                || value.is_empty()
                || value.chars().any(|c| c.is_ascii_control() || !c.is_ascii())
            {
                return Err(RegistryRuleError::InvalidResponse);
            }
        }
        Ok(())
    }
    /// # Errors
    /// Serialization failure.
    pub fn encode(&self) -> Result<Value, RegistryRuleError> {
        serde_json::to_value(self).map_err(|_| RegistryRuleError::InvalidResponse)
    }
    /// Readers keep decoders for every supported version and reject unknown ones.
    ///
    /// # Errors
    /// Unknown version or malformed snapshot.
    pub fn decode(value: &Value) -> Result<Self, RegistryRuleError> {
        if value.get("formatVersion") != Some(&Value::from(Self::FORMAT_VERSION)) {
            return Err(RegistryRuleError::UnsupportedResponseFormat);
        }
        let response: Self = serde_json::from_value(value.clone())
            .map_err(|_| RegistryRuleError::InvalidResponse)?;
        response.validate()?;
        Ok(response)
    }
}

/// Settlement of an owned record. Only the owner may settle; the snapshot is immutable after.
#[derive(Debug, Clone)]
pub enum Settlement {
    /// `order_id` is required for create and must equal the bound target otherwise.
    Success {
        order_id: Option<Uuid>,
        audit_id: Uuid,
        response: StoredResponse,
    },
    Refused {
        reason: Reason,
        audit_id: Uuid,
        response: StoredResponse,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegistryRuleError {
    #[error("idempotency lease must be a positive finite duration")]
    InvalidLease,
    #[error("no stable authenticated principal")]
    UnstablePrincipal,
    #[error("fingerprint target does not match the operation")]
    InvalidTarget,
    #[error("not a registered idempotent write operation")]
    UnknownOperation,
    #[error("contribution is not canonical")]
    NonCanonical,
    #[error("fencing generation exhausted")]
    FenceExhausted,
    #[error("invalid registry record")]
    InvalidRecord,
    #[error("unsupported settled response format")]
    UnsupportedResponseFormat,
    #[error("invalid settled response")]
    InvalidResponse,
}

#[cfg(test)]
#[path = "idempotency_tests.rs"]
mod tests;

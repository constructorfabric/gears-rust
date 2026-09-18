// Created: 2026-09-07 by Constructor Tech
//! Wire shapes of the write surface.

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::api::rest::setting_dto::{EffectiveValueDto, mask};
use crate::domain::error::DomainError;
use crate::domain::secrets::PendingSecret;
use crate::domain::writes::Committed;
use crate::domain::writes::service::{ImpactReport, ValidationReport};

/// `PUT /settings/{key}/value`: the value to store.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct SetValueRequest {
    /// The new value, validated against the declaration's value type.
    pub value: Value,
}

/// `POST /settings/{key}/validate`: the candidate to check.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct ValidateRequest {
    /// The candidate value.
    pub value: Value,
    /// Page size of the impact report, for a cascading setting.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /settings/{key}/impact`: the candidate whose reach is asked about.
/// A body rather than a query string: a value may run to 64 KiB, which no
/// URL carries.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct ImpactRequest {
    /// The candidate value.
    pub value: Value,
    /// Page size, one to five hundred; outside that band it is clamped.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /settings/{key}/secret-stage`: the plaintext to stage ahead of the
/// step-up redirect.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct StageSecretRequest {
    /// The secret value, validated against the declaration's type exactly as
    /// a set validates it.
    pub value: Value,
}

/// The answer to staging a secret: a token that stands in for the value, and
/// when it stops standing in for it. Neither the plaintext nor the store
/// reference.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PendingSecretDto {
    /// The opaque, single-use token to send as the batch change's `value`,
    /// shaped `{ "pending_id": "…" }`.
    pub pending_id: String,
    /// When the staged secret is swept, RFC 3339. After this the token is
    /// refused and the credential-store entry is released.
    pub expires_at: String,
}

/// Render a stage: the token and its expiry, nothing else.
#[must_use]
pub fn render_pending(pending: &PendingSecret) -> PendingSecretDto {
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-6
    PendingSecretDto {
        pending_id: pending.id.to_string(),
        expires_at: pending
            .expires_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| pending.expires_at.to_string()),
    }
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-6
}

/// `POST /settings/{key}/value/clone`: where to copy from.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CloneRequest {
    /// The source tenant; absent, the caller's own tenant.
    #[serde(default)]
    pub from: Option<Uuid>,
}

/// One change of `POST /settings/batch`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BatchChangeRequest {
    /// The setting key.
    pub key: String,
    /// The target tenant; absent, the caller's own.
    #[serde(default)]
    pub tenant: Option<Uuid>,
    /// `set` or `revert`; absent is `set`, so a client written before the
    /// field existed sends exactly what it sent before. An unrecognised word
    /// refuses this change alone with `invalid`.
    #[serde(default)]
    pub op: Option<String>,
    /// The new value, carried by a `set` and absent from a `revert`; a
    /// mismatch either way refuses this change alone with `invalid`. For a
    /// `secret`-trait setting this may instead be `{ "pending_id": "…" }`,
    /// naming a secret staged earlier through `/secret-stage`: the staged
    /// entry is adopted and nothing is stored a second time. Any other shape
    /// is a value and validates as one.
    #[serde(default)]
    pub value: Option<Value>,
    /// The value state tag the caller last read for this scope — for a revert,
    /// the tag of the row being cleared — or the literal `absent` for a first
    /// write. Required in effect: a change that omits it is rejected on its
    /// own with `if_match_required` while the rest of the batch proceeds. It
    /// is optional in the schema only so that one missing tag refuses one
    /// change instead of the whole request.
    #[serde(default)]
    pub if_match: Option<String>,
}

/// `POST /settings/batch`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BatchRequest {
    /// At most five hundred changes, evaluated in order.
    pub changes: Vec<BatchChangeRequest>,
}

/// A committed change.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct SetResultDto {
    /// The setting key.
    pub key: String,
    /// The target scope path.
    pub scope: String,
    /// The target as a tenant id.
    pub tenant_id: Uuid,
    /// The value before, masked by classification; absent when no row existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<Value>,
    /// The value after, masked by classification; absent after a removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<Value>,
    /// Whether the values carry the mask token.
    pub masked: bool,
    /// The scope's new value state tag; the next write presents it.
    pub etag: String,
    /// The change set the write belongs to.
    pub change_set_id: Uuid,
    /// `create`, `change`, `revert` or `remove`.
    pub operation: String,
}

/// Render a committed change, masking by classification.
#[must_use]
pub fn render_committed(committed: &Committed, may_read_pii: bool) -> SetResultDto {
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-9
    // A secret's images are the reference on the way in and the mask token on
    // the way out: the response never echoes what was sent.
    let render = |v: &Value| mask(v, &committed.data_classification, may_read_pii);
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-set:p1:inst-sv-set-9
    let old = committed.old_value.as_ref().map(render);
    let new = committed.new_value.as_ref().map(render);
    let masked = old.as_ref().is_some_and(|(_, m)| *m) || new.as_ref().is_some_and(|(_, m)| *m);
    SetResultDto {
        key: committed.key.clone(),
        scope: committed.scope.clone(),
        tenant_id: committed.tenant_id,
        old_value: old.map(|(v, _)| v),
        new_value: new.map(|(v, _)| v),
        masked,
        etag: committed.etag.clone(),
        change_set_id: committed.change_set_id,
        operation: committed.operation.as_str().to_owned(),
    }
}

/// A revert or remove: the row is gone and the scope falls back.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct FallbackResultDto {
    /// The change as recorded.
    pub change: SetResultDto,
    /// What the scope resolves to now.
    pub effective: EffectiveValueDto,
}

/// One entry of a batch answer.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct BatchItemDto {
    /// The setting key as requested.
    pub key: String,
    /// `committed` or `rejected`.
    pub outcome: String,
    /// The change, when committed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<SetResultDto>,
    /// Why, when rejected — one of a fixed vocabulary: `invalid`,
    /// `if_match_required`, `stale`, `conflict`, `forbidden`, `retired`,
    /// `not_found`, `unavailable` or `error`.
    /// `retired` also covers a declaration retired after the client assembled
    /// the change: the check runs again as each change commits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The rejection in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A batch answer: one entry per change.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct BatchResultDto {
    /// The change set every committed change carries.
    pub change_set_id: Uuid,
    /// In request order.
    pub results: Vec<BatchItemDto>,
}

/// A short stable code for a rejection. The vocabulary is closed and is
/// documented on [`BatchItemDto::error`]; a new arm here is a new word there.
#[must_use]
pub fn rejection_code(err: &DomainError) -> &'static str {
    match err {
        DomainError::Validation { .. } => "invalid",
        DomainError::PreconditionRequired { .. } => "if_match_required",
        DomainError::PreconditionFailed { .. } => "stale",
        DomainError::Conflict { .. } => "conflict",
        DomainError::Unauthorized { .. } => "forbidden",
        DomainError::Retired { .. } => "retired",
        DomainError::NotFound { .. } => "not_found",
        DomainError::Unavailable { .. } => "unavailable",
        // `StepUpRequired` never reaches an entry: a batch verifies step-up
        // once, before any change is evaluated, and refuses the whole request
        // with the 401 challenge. It falls through here on purpose.
        _ => "error",
    }
}

/// Render one batch entry.
#[must_use]
pub fn render_batch_item(
    key: &str,
    outcome: &Result<Committed, DomainError>,
    may_read_pii: bool,
) -> BatchItemDto {
    match outcome {
        Ok(committed) => BatchItemDto {
            key: key.to_owned(),
            outcome: "committed".to_owned(),
            change: Some(render_committed(committed, may_read_pii)),
            error: None,
            detail: None,
        },
        Err(err) => BatchItemDto {
            key: key.to_owned(),
            outcome: "rejected".to_owned(),
            change: None,
            error: Some(rejection_code(err).to_owned()),
            detail: Some(err.to_string()),
        },
    }
}

/// One field-level violation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ViolationDto {
    /// The offending field, as a JSON pointer under `value`.
    pub field: String,
    /// A stable code.
    pub code: String,
    /// In words.
    pub message: String,
}

/// One descendant the change would affect.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct ImpactEntryDto {
    /// The descendant.
    pub tenant_id: Uuid,
    /// Its scope path.
    pub scope: String,
    /// Its effective value today, masked by classification.
    pub current: Value,
}

/// The bounded impact report.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct ImpactReportDto {
    /// The first `limit` affected descendants in traversal order.
    pub changed: Vec<ImpactEntryDto>,
    /// How many would change, up to the node budget.
    pub total_changed: usize,
    /// How many descendants were examined.
    pub scanned: usize,
    /// Whether the budget or `limit` cut the report short: it then reads as
    /// "at least this many".
    pub truncated: bool,
}

/// Render an impact report.
#[must_use]
pub fn render_impact(
    report: &ImpactReport,
    classification: &str,
    may_read_pii: bool,
) -> ImpactReportDto {
    ImpactReportDto {
        changed: report
            .changed
            .iter()
            .map(|e| ImpactEntryDto {
                tenant_id: e.tenant_id,
                scope: e.scope.clone(),
                current: mask(&e.current, classification, may_read_pii).0,
            })
            .collect(),
        total_changed: report.total_changed,
        scanned: report.scanned,
        truncated: report.truncated,
    }
}

/// The read-only report of `validate`.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct ValidationReportDto {
    /// Whether the candidate would be accepted.
    pub valid: bool,
    /// Field-level detail; empty when valid.
    pub violations: Vec<ViolationDto>,
    /// The current effective value at the target.
    pub effective: EffectiveValueDto,
    /// The descendants the change would affect, for a cascading setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<ImpactReportDto>,
}

/// Render the validation report.
#[must_use]
pub fn render_validation(report: &ValidationReport, may_read_pii: bool) -> ValidationReportDto {
    let effective = crate::api::rest::setting_dto::render(&report.effective, may_read_pii);
    let impact = report
        .impact
        .as_ref()
        .map(|i| render_impact(i, &report.effective.data_classification, may_read_pii));
    ValidationReportDto {
        valid: report.violations.is_empty(),
        violations: report
            .violations
            .iter()
            .map(|v| ViolationDto {
                field: v.field.clone(),
                code: v.code.to_owned(),
                message: v.message.clone(),
            })
            .collect(),
        effective,
        impact,
    }
}

#[cfg(test)]
#[path = "value_dto_tests.rs"]
mod value_dto_tests;

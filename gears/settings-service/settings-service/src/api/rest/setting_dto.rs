// Created: 2026-09-07 by Constructor Tech
//! Wire shapes of the administrative read surface over effective values.

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, MASK_TOKEN, scope_path};
use crate::domain::value::StoredValue;

/// The value state tag of a scope that holds no row yet — the write path's
/// absent-state tag, which the read returns for the same state.
pub const ABSENT_STATE_TAG: &str = crate::domain::writes::ABSENT_VALUE_TAG;

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_else(|_| at.to_string())
}

/// Mask a value by its classification.
///
/// `secret` is the mask token always; `pii` is the token unless the caller is
/// entitled to unmasked PII; `public` passes through. Returns the value and
/// whether it was masked.
#[must_use]
// @cpt-dod:cpt-cf-settings-service-dod-secret-values-no-reveal:p1
pub fn mask(value: &Value, data_classification: &str, may_read_pii: bool) -> (Value, bool) {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-10
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-1
    // @cpt-begin:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-2
    match data_classification {
        "secret" => (Value::String(MASK_TOKEN.to_owned()), true),
        "pii" if !may_read_pii => (Value::String(MASK_TOKEN.to_owned()), true),
        _ => (value.clone(), false),
    }
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-2
    // @cpt-end:cpt-cf-settings-service-flow-secret-values-admin-read:p1:inst-sv-aread-1
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-10
}

/// One scope on the inheritance trail, with the setter identity and
/// timestamp the administrative read is allowed to show.
// Three independent facts about one scope, each read on its own by a client
// rendering the trail; an enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct TrailEntryDto {
    /// `/` or `/tenants/{id}`.
    pub scope: String,
    /// The tenant inspected.
    pub tenant_id: Uuid,
    /// Whether an override row exists here.
    pub has_override: bool,
    /// Whether this scope supplied the effective value.
    pub provided_value: bool,
    /// Whether the row here is flagged for review and was skipped.
    pub needs_review: bool,
    /// Who set the row here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
    /// When the row here last changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_change_at: Option<String>,
}

/// The effective value at a scope, as `GET /settings-service/v1/settings/{key}`
/// returns it.
// `PartialEq` only -- JSON numbers have no total equality.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct EffectiveValueDto {
    /// The setting key.
    pub key: String,
    /// The requested scope.
    pub scope: String,
    /// The requested scope as a tenant id; the root tenant's id is platform
    /// scope.
    pub tenant_id: Uuid,
    /// The resolved value, masked by classification.
    pub value: Value,
    /// `own_override`, `inherited` or `schema_default`.
    pub source: String,
    /// The scope that supplied the value; absent for a Schema Default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_scope: Option<String>,
    /// The value type's resolved trait set.
    pub traits: Value,
    /// The scopes inspected, root to self, with setter identity.
    pub inheritance_trail: Vec<TrailEntryDto>,
    /// Recency of the effective value the caller sees: the later of the
    /// declaration's own change and the resolved row's.
    pub last_change_at: String,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// Whether `value` carries the mask token rather than the value.
    pub masked: bool,
    /// Present when the requested scope's own override is flagged for review;
    /// the value above is then the fallthrough the resolver served.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_review: Option<bool>,
    /// Why the own override is flagged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_review_detail: Option<String>,
    /// The value state tag of the requested scope's own row, or of the absent
    /// state — what a write at this scope presents in `If-Match`. Also sent as
    /// the `ETag` header.
    pub etag: String,
}

/// Render a resolved value for the administrative read.
#[must_use]
pub fn render(effective: &EffectiveValue, may_read_pii: bool) -> EffectiveValueDto {
    let (value, masked) = mask(
        &effective.value,
        &effective.data_classification,
        may_read_pii,
    );
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-8
    // The later of the two arms, each leak-safe on its own: the declaration's
    // definition change, and the resolved row's — a row within the caller's
    // own chain, never a maximum over sibling or descendant scopes.
    let last_change_at = effective
        .resolved_row_last_change_at
        .map_or(effective.declaration_last_change_at, |row| {
            row.max(effective.declaration_last_change_at)
        });
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-8
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-9
    let (needs_review, needs_review_detail) = match &effective.own_row {
        Some(own) if own.needs_review => (Some(true), own.needs_review_detail.clone()),
        _ => (None, None),
    };
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-9
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-11
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-6
    let inheritance_trail = effective
        .trail
        .iter()
        .map(|e| TrailEntryDto {
            scope: e.scope.clone(),
            tenant_id: e.tenant_id,
            has_override: e.has_override,
            provided_value: e.provided_value,
            needs_review: e.needs_review,
            set_by: e.set_by.clone(),
            last_change_at: e.last_change_at.map(rfc3339),
        })
        .collect();
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-6
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-11
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-12
    // The tag is the requested scope's own row state, or the absent state; it
    // is distinct from the recency above, which describes the effective value.
    let etag = effective.own_row.as_ref().map_or_else(
        || ABSENT_STATE_TAG.to_owned(),
        |own| own.last_change_at.unix_timestamp_nanos().to_string(),
    );
    EffectiveValueDto {
        key: effective.key.clone(),
        scope: effective.scope.clone(),
        tenant_id: effective.tenant_id,
        value,
        source: source_name(effective.source),
        source_scope: effective.source_scope.clone(),
        traits: effective.traits.clone(),
        inheritance_trail,
        last_change_at: rfc3339(last_change_at),
        data_classification: effective.data_classification.clone(),
        masked,
        needs_review,
        needs_review_detail,
        etag,
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-12
}

fn source_name(source: settings_service_sdk::EffectiveSource) -> String {
    match source {
        settings_service_sdk::EffectiveSource::OwnOverride => "own_override",
        settings_service_sdk::EffectiveSource::Inherited => "inherited",
        settings_service_sdk::EffectiveSource::SchemaDefault => "schema_default",
    }
    .to_owned()
}

/// An override flagged for review, as the needs-review listing reports it.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct FlaggedOverrideDto {
    /// The setting key.
    pub key: String,
    /// The tenant whose override is flagged.
    pub tenant_id: Uuid,
    /// Its scope path.
    pub scope: String,
    /// The stored value, masked by classification.
    pub value: Value,
    /// Whether `value` carries the mask token.
    pub masked: bool,
    /// Why the override no longer validates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_review_detail: Option<String>,
    /// When the override last changed.
    pub last_change_at: String,
    /// Who set it.
    pub set_by: String,
    /// The override's value state tag.
    pub etag: String,
}

/// Render a flagged row for the listing.
#[must_use]
pub fn render_flagged(
    key: &str,
    row: &StoredValue,
    root: Uuid,
    may_read_pii: bool,
) -> FlaggedOverrideDto {
    let stored = row
        .value
        .clone()
        .unwrap_or_else(|| Value::String(MASK_TOKEN.to_owned()));
    let (value, masked) = mask(&stored, &row.data_classification, may_read_pii);
    FlaggedOverrideDto {
        key: key.to_owned(),
        tenant_id: row.tenant_id,
        scope: scope_path(row.tenant_id, root),
        value,
        masked,
        needs_review_detail: row.needs_review_detail.clone(),
        last_change_at: rfc3339(row.last_change_at),
        set_by: row.set_by.clone(),
        etag: row.updated_at.unix_timestamp_nanos().to_string(),
    }
}

/// One entry of the browse page: a key with its own outcome.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct SettingItemDto {
    /// The setting key.
    pub key: String,
    /// `resolved`, `needs_review`, `not_found`, `retired`, `unavailable` or
    /// `error`.
    pub outcome: String,
    /// The effective value, when `outcome` is `resolved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<EffectiveValueDto>,
    /// The flagged override, when `outcome` is `needs_review`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flagged: Option<FlaggedOverrideDto>,
    /// What went wrong, for the failure outcomes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `standard` or `advanced`, from the declaration. Absent only for a
    /// requested key that has no declaration. Mode is a tag, never a filter:
    /// every page carries every setting and a client groups them itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl SettingItemDto {
    /// The same entry, tagged with its declaration's mode.
    #[must_use]
    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_owned());
        self
    }

    /// A resolved entry.
    #[must_use]
    pub fn resolved(effective: EffectiveValueDto) -> Self {
        Self {
            key: effective.key.clone(),
            outcome: "resolved".to_owned(),
            effective: Some(effective),
            flagged: None,
            detail: None,
            mode: None,
        }
    }

    /// A flagged-override entry.
    #[must_use]
    pub fn flagged(flagged: FlaggedOverrideDto) -> Self {
        Self {
            key: flagged.key.clone(),
            outcome: "needs_review".to_owned(),
            effective: None,
            flagged: Some(flagged),
            detail: None,
            mode: None,
        }
    }

    /// A key that could not be resolved, carrying its own outcome rather than
    /// failing the request.
    #[must_use]
    pub fn failed(key: &str, err: &DomainError) -> Self {
        let outcome = match err {
            DomainError::NotFound { .. } => "not_found",
            DomainError::Retired { .. } => "retired",
            DomainError::Unavailable { .. } => "unavailable",
            _ => "error",
        };
        Self {
            key: key.to_owned(),
            outcome: outcome.to_owned(),
            effective: None,
            flagged: None,
            detail: Some(err.to_string()),
            mode: None,
        }
    }

    /// A requested key with no declaration.
    #[must_use]
    pub fn not_found(key: &str) -> Self {
        Self::failed(
            key,
            &DomainError::NotFound {
                resource: "declaration",
            },
        )
    }
}

#[cfg(test)]
#[path = "setting_dto_tests.rs"]
mod setting_dto_tests;

/// One audit record, as `GET /settings-service/v1/settings/{key}/history`
/// returns it.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct AuditRecordDto {
    /// Record identity.
    pub id: Uuid,
    /// `create`, `change`, `revert`, `remove`, `clone` or `secret_use`.
    pub operation: String,
    /// Who did it, masked when the identity is PII the caller may not see.
    pub actor: String,
    /// Whether `actor` carries the mask token.
    pub actor_masked: bool,
    /// The value before, as recorded; a secret was recorded masked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_value: Option<Value>,
    /// The value after, as recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_value: Option<Value>,
    /// Whether the recorded values were masked for this caller.
    pub values_masked: bool,
    /// `success` or `failure`.
    pub outcome: String,
    /// The request that produced the record.
    pub request_id: String,
    /// The change set the mutation was produced under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<Uuid>,
    /// When it happened.
    pub occurred_at: String,
}

fn recorded(image: Option<&crate::audit::AuditValue>, mask_pii: bool) -> Option<Value> {
    image.map(|v| match v {
        crate::audit::AuditValue::Masked => Value::String(MASK_TOKEN.to_owned()),
        crate::audit::AuditValue::Clear(_) if mask_pii => Value::String(MASK_TOKEN.to_owned()),
        crate::audit::AuditValue::Clear(value) => value.clone(),
    })
}

// @cpt-dod:cpt-cf-settings-service-dod-audit-store-masking-classification:p1
/// Render a stored record for the history read.
///
/// `values_are_pii` says whether the setting's values are `pii`-classified;
/// `may_read_pii` whether the caller holds the entitlement. A secret needs no
/// decision here: it was never recorded in plaintext.
#[must_use]
pub fn render_record(
    record: &crate::audit::StoredAuditRecord,
    values_are_pii: bool,
    may_read_pii: bool,
) -> AuditRecordDto {
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-8
    let actor_masked =
        record.actor_classification == crate::audit::ActorClassification::Pii && !may_read_pii;
    let values_masked = values_are_pii && !may_read_pii;
    AuditRecordDto {
        id: record.id,
        operation: record.operation.as_str().to_owned(),
        actor: if actor_masked {
            MASK_TOKEN.to_owned()
        } else {
            record.actor.clone()
        },
        actor_masked,
        pre_value: recorded(record.pre_image.as_ref(), values_masked),
        post_value: recorded(record.post_image.as_ref(), values_masked),
        values_masked,
        outcome: record.outcome.as_str().to_owned(),
        request_id: record.request_id.clone(),
        change_set_id: record.change_set_id,
        occurred_at: rfc3339(record.occurred_at),
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-8
}

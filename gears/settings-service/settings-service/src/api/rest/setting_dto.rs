//! Wire shapes of the administrative read surface over effective values.

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, MASK_TOKEN, TrailEntry, scope_path};
use crate::domain::value::StoredValue;
use crate::domain::writes::value_state_tag;

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

/// Whether a value of this classification is shown to the caller unmasked —
/// the masking rule's own answer, asked without a value, so what a text
/// narrowing may match and what [`mask`] shows cannot drift apart.
#[must_use]
pub fn is_shown_unmasked(data_classification: &str, may_read_pii: bool) -> bool {
    !mask(&Value::Null, data_classification, may_read_pii).1
}

/// `skip_serializing_if` for a flag that is only ever worth saying when set.
// Serde passes the field by reference, so the signature is not ours to pick.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn is_false(flag: &bool) -> bool {
    !*flag
}

/// Who a response is rendered for, under the upward disclosure boundary.
///
/// The reader tenant is the caller's own tenant. A caller of the root tenant
/// has nothing above it, so nothing collapses and every identity is shown.
/// Every other reader sees what lies above its tenant as one anonymous entry
/// and an identity only where its recorded tenant lies on the path from the
/// reader down to the scope the identity concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// A caller of the root tenant: nothing above it, every identity shown.
    Platform,
    /// A caller of any other tenant, the one it carries: where the caller
    /// stands, not its role — what it may call is the PDP's to decide.
    Tenant(Uuid),
}

impl Reader {
    /// The reader for a caller whose own tenant is `caller_tenant`, `root`
    /// being the root tenant's id.
    #[must_use]
    pub fn new(caller_tenant: Uuid, root: Uuid) -> Self {
        if caller_tenant == root {
            Self::Platform
        } else {
            Self::Tenant(caller_tenant)
        }
    }

    /// The part of a root-to-target `chain` from the reader tenant down. A
    /// reader the chain does not hold — the target is in the reader's subtree,
    /// so this should not happen — fails closed to the target alone.
    #[must_use]
    pub fn path_in(self, chain: &[Uuid]) -> &[Uuid] {
        match self {
            Self::Platform => chain,
            Self::Tenant(tenant) => match chain.iter().position(|t| *t == tenant) {
                Some(at) => &chain[at..],
                None => &chain[chain.len().saturating_sub(1)..],
            },
        }
    }

    /// Whether `tenant` lies above the reader tenant on a root-to-target
    /// `chain`; nothing is above a caller of the root tenant. It fails closed:
    /// a tenant off the chain is above, and so is every tenant but the target
    /// for a reader the chain does not hold.
    #[must_use]
    pub fn is_above(self, tenant: Uuid, chain: &[Uuid]) -> bool {
        let Self::Tenant(reader) = self else {
            return false;
        };
        let at = |t: Uuid| chain.iter().position(|c| *c == t);
        match (at(reader), at(tenant)) {
            (Some(reader_at), Some(at)) => at < reader_at,
            // A trail only ever holds tenants of the chain it was resolved on:
            // one off the current chain is stale after a re-parent.
            (Some(_), None) => true,
            (None, _) => chain.last() != Some(&tenant),
        }
    }

    /// Whether an identity recorded from `recorded` is shown, given the `path`
    /// from the reader tenant down to the scope it concerns. A caller of the
    /// root tenant is shown every identity, recorded or not.
    #[must_use]
    pub fn is_shown(self, recorded: Option<Uuid>, path: &[Uuid]) -> bool {
        match self {
            Self::Platform => true,
            Self::Tenant(_) => recorded.is_some_and(|tenant| path.contains(&tenant)),
        }
    }
}

/// An identity as one reader is shown it, and whether the boundary withheld
/// it. A withheld identity is the mask token whatever the entitlement; a shown
/// one is still masked for a reader without the unmasked-PII entitlement.
#[must_use]
pub fn disclose(who: &str, shown: bool, may_read_pii: bool) -> (String, bool) {
    if !shown {
        (MASK_TOKEN.to_owned(), true)
    } else if may_read_pii {
        (who.to_owned(), false)
    } else {
        (MASK_TOKEN.to_owned(), false)
    }
}

/// One scope on the inheritance trail, with the setter identity and
/// timestamp the administrative read is allowed to show.
// Independent facts about one scope, each read on its own by a client
// rendering the trail; an enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct TrailEntryDto {
    /// `/` or `/tenants/{id}`; absent on the collapsed entry that stands for
    /// every scope above the reader tenant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The tenant inspected; absent on the collapsed entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    /// Set on the one leading entry that stands for every scope above the
    /// reader tenant: it carries `provided_value`, `has_override` and
    /// `needs_review`, the last always `false` — a flagged ancestor row is
    /// not the reader's to see.
    #[serde(default, skip_serializing_if = "is_false")]
    pub above: bool,
    /// Whether an override row exists here.
    pub has_override: bool,
    /// Whether this scope supplied the effective value.
    pub provided_value: bool,
    /// Whether the row here is flagged for review and was skipped.
    pub needs_review: bool,
    /// Who set the row here: an administrator's identity, masked for a
    /// caller not authorized for unmasked PII, and the mask token with
    /// `set_by_withheld` when its tenant is not on the path from the reader
    /// tenant down to this scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
    /// Whether `set_by` is withheld by the upward disclosure boundary rather
    /// than by the PII entitlement.
    #[serde(default, skip_serializing_if = "is_false")]
    pub set_by_withheld: bool,
    /// When the row here last changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_change_at: Option<String>,
}

impl TrailEntryDto {
    /// The one leading entry that stands for every scope above the reader
    /// tenant: no scope, tenant, setter or time, and never flagged.
    #[must_use]
    pub fn above(has_override: bool, provided_value: bool) -> Self {
        Self {
            scope: None,
            tenant_id: None,
            above: true,
            has_override,
            provided_value,
            needs_review: false,
            set_by: None,
            set_by_withheld: false,
            last_change_at: None,
        }
    }
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
    /// What this scope resolves to without a row of its own — the nearest
    /// valid ancestor override, else the Schema Default — masked by the same
    /// rule as `value`. Equal to `value` when the scope holds no override: the
    /// settings table's *Default* column, and what a revert would leave.
    pub fallback: Value,
    /// `inherited` or `schema_default`; never `own_override`.
    pub fallback_source: String,
    /// The scope that supplies the fallback; absent for a Schema Default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_scope: Option<String>,
    /// The value type's resolved trait set.
    pub traits: Value,
    /// The scopes inspected, root to self, with setter identity.
    pub inheritance_trail: Vec<TrailEntryDto>,
    /// Recency of the effective value the caller sees: the later of the
    /// declaration's own change and the resolved row's.
    pub last_change_at: String,
    /// `public`, `pii` or `secret`.
    pub data_classification: String,
    /// Whether `value` and `fallback` carry the mask token rather than the
    /// value: one decision, made on the declaration's classification, for
    /// both.
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

/// Render a resolved value for the administrative read, for `reader`.
///
/// The resolver and its cache keep the full trail; what a reader may see of
/// it is decided here, per request, so two readers of one cached scope are
/// each redacted for themselves. `chain` is the target's chain, root first:
/// what is above the reader tenant and the path an identity must lie on are
/// read off it.
#[must_use]
pub fn render(
    effective: &EffectiveValue,
    may_read_pii: bool,
    reader: Reader,
    chain: &[Uuid],
) -> EffectiveValueDto {
    let (value, masked) = mask(
        &effective.value,
        &effective.data_classification,
        may_read_pii,
    );
    // The same rule over the same classification: the fallback can be no
    // clearer than the value, and a secret's fallback is the mask token too.
    let (fallback, fallback_masked) = mask(
        &effective.fallback,
        &effective.data_classification,
        may_read_pii,
    );
    debug_assert_eq!(masked, fallback_masked, "one masking decision for both");
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
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-13
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-6
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-10
    // Above and the identity path are decided from the target's chain, never
    // from the trail: a `global` setting's trail is the root alone and a
    // `local` one's the target alone, and a tenant reader is on neither. The
    // reader tenant's place on the chain splits it; a reader the chain does
    // not hold fails closed — everything but the target is above, and no
    // identity is on a path.
    let reader_at = match reader {
        Reader::Platform => Some(0),
        Reader::Tenant(tenant) => chain.iter().position(|t| *t == tenant),
    };
    let platform = reader == Reader::Platform;
    let (above, from_reader): (Vec<&TrailEntry>, Vec<&TrailEntry>) = effective
        .trail
        .iter()
        .partition(|e| reader.is_above(e.tenant_id, chain));
    let mut inheritance_trail = Vec::with_capacity(from_reader.len() + 1);
    if !above.is_empty() {
        inheritance_trail.push(TrailEntryDto::above(
            // A valid, unflagged override above; a flagged ancestor row is not
            // the reader's to see and counts for nothing.
            above.iter().any(|e| e.has_override && !e.needs_review),
            // True when the winning row is above the reader tenant.
            above.iter().any(|e| e.provided_value),
        ));
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-10
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-11
    for e in from_reader {
        // The path from the reader tenant down to this entry's scope: the
        // target's chain between the two, inclusive.
        let path: &[Uuid] = match (reader_at, chain.iter().position(|t| *t == e.tenant_id)) {
            (Some(from), Some(to)) if from <= to => &chain[from..=to],
            _ => &[],
        };
        let shown = reader.is_shown(e.set_by_tenant, path);
        // An administrator's identity, shown to a reader who may read
        // unmasked and masked for one who may not — as the audit history
        // masks its actor. The entry itself stays: what is hidden is who.
        let (set_by, set_by_withheld) = e.set_by.as_ref().map_or((None, false), |who| {
            let (who, withheld) = disclose(who, shown, may_read_pii);
            (Some(who), withheld)
        });
        inheritance_trail.push(TrailEntryDto {
            scope: Some(e.scope.clone()),
            tenant_id: Some(e.tenant_id),
            above: false,
            has_override: e.has_override,
            provided_value: e.provided_value,
            needs_review: e.needs_review,
            set_by,
            set_by_withheld,
            last_change_at: e.last_change_at.map(rfc3339),
        });
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-11
    // A scope named by the source or the fallback that lies above the reader
    // tenant is not named: absent, as it is for a Schema Default. Its tenant is
    // read off the scope path and judged on the same chain.
    let scope_is_above = |scope: &Option<String>| {
        scope.as_ref().is_some_and(|named| {
            let tenant = if named == "/" {
                chain.first().copied()
            } else {
                named
                    .strip_prefix("/tenants/")
                    .and_then(|id| Uuid::parse_str(id).ok())
            };
            // A scope that names no tenant of the chain — unparseable, or the
            // root of an empty chain — fails closed as above.
            !platform && tenant.is_none_or(|tenant| reader.is_above(tenant, chain))
        })
    };
    let source_scope = if scope_is_above(&effective.source_scope) {
        None
    } else {
        effective.source_scope.clone()
    };
    let fallback_scope = if scope_is_above(&effective.fallback_scope) {
        None
    } else {
        effective.fallback_scope.clone()
    };
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-source-trail:p1:inst-vr-trail-6
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-read:p1:inst-vr-aread-13
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
        source_scope,
        fallback,
        fallback_source: source_name(effective.fallback_source),
        fallback_scope,
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

/// One flagged override row, as `GET /settings-service/v1/settings/review`
/// lists it: the row at the scope that holds it, where it must be fixed — not
/// a resolved value.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct ReviewItemDto {
    /// The setting key.
    pub key: String,
    /// The declaration's display name; omitted when it has none, never derived
    /// from the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `standard` or `advanced`, from the declaration.
    pub mode: String,
    /// The tenant whose override is flagged.
    pub tenant_id: Uuid,
    /// Its scope path: `/` or `/tenants/{id}`.
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
    /// Who set it: an administrator's identity, masked for a caller not
    /// authorized for unmasked PII, and the mask token with `set_by_withheld`
    /// when its tenant is not on the path from the reader tenant to the row's
    /// tenant.
    pub set_by: String,
    /// Whether `set_by` is withheld by the upward disclosure boundary rather
    /// than by the PII entitlement.
    #[serde(default, skip_serializing_if = "is_false")]
    pub set_by_withheld: bool,
    /// The override's value state tag — what a write correcting this row
    /// presents in `If-Match`.
    pub etag: String,
}

impl ReviewItemDto {
    /// The same row, tagged with its declaration's display name, if any.
    #[must_use]
    pub fn with_display_name(mut self, display_name: Option<&str>) -> Self {
        self.display_name = display_name.map(str::to_owned);
        self
    }
}

/// Render a flagged row for the review listing, under the declaration's
/// `mode`. Its display name is the caller's to add, as on a browse entry.
///
/// `setter_shown` is the identity rule's verdict for this row: whether its
/// recorded tenant lies on the path from the reader tenant down to the row's
/// tenant, which the caller reads off the target's chain and the subtree walk.
#[must_use]
pub fn render_flagged(
    key: &str,
    mode: &str,
    row: &StoredValue,
    root: Uuid,
    may_read_pii: bool,
    setter_shown: bool,
) -> ReviewItemDto {
    let stored = row
        .value
        .clone()
        .unwrap_or_else(|| Value::String(MASK_TOKEN.to_owned()));
    let (value, masked) = mask(&stored, &row.data_classification, may_read_pii);
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-10
    let (set_by, set_by_withheld) = disclose(&row.set_by, setter_shown, may_read_pii);
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-review-list:p1:inst-vr-review-10
    ReviewItemDto {
        key: key.to_owned(),
        display_name: None,
        mode: mode.to_owned(),
        tenant_id: row.tenant_id,
        scope: scope_path(row.tenant_id, root),
        value,
        masked,
        needs_review_detail: row.needs_review_detail.clone(),
        last_change_at: rfc3339(row.last_change_at),
        // Withheld or masked for the same reader the trail withholds and
        // masks it for.
        set_by,
        set_by_withheld,
        // The value state tag a correcting write presents — the same definition
        // the write path compares, never `updated_at`, which a flag moves alone.
        etag: value_state_tag(Some(row)).as_str().to_owned(),
    }
}

/// One entry of the browse page: a key with its own outcome.
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct SettingItemDto {
    /// The setting key.
    pub key: String,
    /// `resolved`, `not_found`, `retired`, `unavailable` or `error`.
    pub outcome: String,
    /// The effective value, when `outcome` is `resolved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<EffectiveValueDto>,
    /// What went wrong, for the failure outcomes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `standard` or `advanced`, from the declaration. Absent only for a
    /// requested key that has no declaration. Mode is a tag, never a filter:
    /// every page carries every setting and a client groups them itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The declaration's display name; omitted when it has none, never derived
    /// from the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl SettingItemDto {
    /// The same entry, tagged with its declaration's mode.
    #[must_use]
    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_owned());
        self
    }

    /// The same entry, tagged with its declaration's display name, if any.
    #[must_use]
    pub fn with_display_name(mut self, display_name: Option<&str>) -> Self {
        self.display_name = display_name.map(str::to_owned);
        self
    }

    /// A resolved entry.
    #[must_use]
    pub fn resolved(effective: EffectiveValueDto) -> Self {
        Self {
            key: effective.key.clone(),
            outcome: "resolved".to_owned(),
            effective: Some(effective),
            detail: None,
            mode: None,
            display_name: None,
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
            detail: Some(err.wire_message()),
            mode: None,
            display_name: None,
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
// Three independent facts about the actor and the images, each read on its own
// by a client; an enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct AuditRecordDto {
    /// Record identity.
    pub id: Uuid,
    /// The scope the record is about, or `null` for a record about the
    /// setting's definition — the declaration created, changed, retired —
    /// which belongs to no tenant and appears in every scope's history.
    #[schema(required)]
    pub tenant_id: Option<Uuid>,
    /// `create`, `change`, `revert`, `remove`, `clone`, `secret_use` or `stage`.
    pub operation: String,
    /// Who did it, masked when the identity is PII the caller may not see.
    pub actor: String,
    /// Whether `actor` carries the mask token, whether because the caller lacks
    /// the unmasked-PII entitlement or because the boundary withheld it
    /// (`actor_withheld` tells the two apart).
    pub actor_masked: bool,
    /// Whether `actor` is withheld by the upward disclosure boundary, whatever
    /// the entitlement: its tenant is not on the path from the reader tenant to
    /// the record's.
    #[serde(default, skip_serializing_if = "is_false")]
    pub actor_withheld: bool,
    /// The actor's name as the Actor Name Resolver reads it: a user's current
    /// name, `"System"` for the nil id, or `null` when the actor is masked or
    /// withheld, a module, or no name could be had.
    #[schema(required)]
    pub actor_name: Option<String>,
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

/// One image as a reader is shown it. A secret was recorded as its mask. A
/// `pii` image is masked for a reader without the entitlement whatever the
/// setting is classified as now; a clear one is masked too while the setting
/// is `pii`, which covers what was recorded before it was classified so.
fn recorded(
    image: Option<&crate::audit::AuditValue>,
    setting_is_pii: bool,
    may_read_pii: bool,
) -> Option<Value> {
    let mask = || Value::String(MASK_TOKEN.to_owned());
    image.map(|v| match v {
        crate::audit::AuditValue::Masked => mask(),
        crate::audit::AuditValue::Pii(_) if !may_read_pii => mask(),
        crate::audit::AuditValue::Clear(_) if setting_is_pii && !may_read_pii => mask(),
        crate::audit::AuditValue::Pii(value) | crate::audit::AuditValue::Clear(value) => {
            value.clone()
        }
    })
}

// @cpt-dod:cpt-cf-settings-service-dod-audit-store-masking-classification:p1
/// Render a stored record for the history read.
///
/// `values_are_pii` says whether the setting's values are `pii`-classified
/// now; `may_read_pii` whether the caller holds the entitlement. A `pii` image
/// carries its own class and is masked without the entitlement whatever the
/// setting is classified as now. A secret needs no decision here: it was never
/// recorded in plaintext.
#[must_use]
pub fn render_record(
    record: &crate::audit::StoredAuditRecord,
    values_are_pii: bool,
    may_read_pii: bool,
    actor_shown: bool,
) -> AuditRecordDto {
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-10
    // The actor is withheld, whatever the entitlement and whatever its
    // classification, unless the boundary shows it to this reader.
    let actor_withheld = !actor_shown;
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-10
    // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-8
    // The mask token is shown for either reason, so `actor_masked` stays true
    // whenever it is; `actor_withheld` says which.
    let actor_masked = actor_withheld
        || (record.actor_classification == crate::audit::ActorClassification::Pii && !may_read_pii);
    let values_masked = (values_are_pii || record.has_pii_image()) && !may_read_pii;
    AuditRecordDto {
        id: record.id,
        tenant_id: record.tenant_id,
        operation: record.operation.as_str().to_owned(),
        actor: if actor_masked {
            MASK_TOKEN.to_owned()
        } else {
            record.actor.clone()
        },
        actor_masked,
        actor_withheld,
        actor_name: None,
        pre_value: recorded(record.pre_image.as_ref(), values_are_pii, may_read_pii),
        post_value: recorded(record.post_image.as_ref(), values_are_pii, may_read_pii),
        values_masked,
        outcome: record.outcome.as_str().to_owned(),
        request_id: record.request_id.clone(),
        change_set_id: record.change_set_id,
        occurred_at: rfc3339(record.occurred_at),
    }
    // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-8
}

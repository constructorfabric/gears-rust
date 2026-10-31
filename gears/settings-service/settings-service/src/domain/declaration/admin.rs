//! Administrative authoring of declarations: create, revive, edit metadata,
//! retire.
//!
//! Everything here is immediate and never touches the value write path. What
//! it guards is that no edit changes a live setting's resolution without a
//! gate: behaviour-affecting fields are refused as immutable, loosening a gate
//! or a classification and every lifecycle change demand credential step-up,
//! and a gear's contributed declarations are not admin-editable at all.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_macros::domain_model;

use futures_util::StreamExt as _;
use secrecy::ExposeSecret;
use serde_json::{Map, Value};
use settings_service_sdk::{SettingKey, SettingKeyError};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::category::repo::CategoryRepository;
use crate::domain::category::visibility;
use crate::domain::contribution::SettingTypeRegistrar;
use crate::domain::declaration::{
    DataClassification, Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository,
    DeclarationStatus,
};
use crate::domain::error::DomainError;
use crate::domain::ports::{SecretManager, StepUpOperation, WriteMetrics};
use crate::domain::precondition::{self, ETag};
use crate::domain::resolution::EffectiveCache;
use crate::domain::secrets::pending::PendingSecretRepository;
use crate::domain::stepup::{StepUpRefusal, StepUpVerifier};
use crate::domain::validation::{TraitSet, TypeValidator};
use crate::domain::value::{StoredValue, ValueDraft, ValueRepository};
use crate::domain::writes::WriteActor;
use crate::field;
use crate::log_text::LogSafe;

/// Conflict reasons this path raises; the text a `409` carries starts with one.
pub mod conflict {
    /// A gear's declaration changes only through its owning module.
    pub const CONTRIBUTED_IMMUTABLE: &str = "contributed_declaration_immutable";
    /// An active declaration already holds this key.
    pub const KEY_CONFLICT: &str = "declaration_key_conflict";
    /// An active declaration in the category already holds this leaf name.
    pub const LEAF_NAME_TAKEN: &str = "leaf_name_taken";
    /// A revive names a value type on the other side of the secret boundary;
    /// values stored one way are not re-interpreted as the other.
    pub const SECRETNESS_CHANGED: &str = "secretness_changed";
    /// A revive changes the scope class; where a value may exist is not revived.
    pub const SCOPE_CLASS_CHANGED: &str = "scope_class_changed";
    /// A revive names another value type; the setting's own GTS type stays
    /// registered with the one it was declared with.
    pub const VALUE_TYPE_CHANGED: &str = "value_type_changed";
    /// A purge removes only a retired declaration; it never retires one.
    pub const NOT_RETIRED: &str = "declaration_not_retired";
}

/// How many Credential Store releases a purge has in flight at once.
const RELEASE_CONCURRENCY: usize = 4;

/// A Credential Store entry a purged declaration held: what the release asks
/// the Secret Manager to delete.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSecret {
    /// The tenant the entry was stored for.
    pub tenant: Uuid,
    /// The reference of the entry; never logged.
    pub secret_ref: String,
}

/// What a committed purge hands on: the key it freed and the entries to release.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgedDeclaration {
    /// The id of the declaration that is gone.
    pub id: Uuid,
    /// The key of the declaration that is gone.
    pub key: String,
    /// Every entry its values and staged secrets held, each reference once,
    /// less those another declaration still holds.
    pub secrets: Vec<HeldSecret>,
    /// The references left out because a value or staged row of another
    /// declaration still names them: an evolution copies a retired
    /// predecessor's references into its successor. Checked again after the
    /// commit ([`DeclarationAdmin::reclaim_kept`]): the other declaration may
    /// have been purged meanwhile, and then nobody else would release them.
    pub kept_secrets: Vec<HeldSecret>,
}

/// How a release ended.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseSummary {
    /// Entries the store released, or already did not hold.
    pub released: usize,
    /// Entries the store refused; left to an operator's reconcile.
    pub failed: usize,
}

/// What an administrator supplies to declare a setting.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CreateDeclaration {
    pub value_type_id: String,
    pub vendor: String,
    pub name: String,
    pub category_id: Uuid,
    pub default_value: Value,
    pub scope_class: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub mode: Option<String>,
    pub requires_step_up: Option<bool>,
    pub anonymous_exposable: Option<bool>,
    pub domain_affinity: Option<String>,
    pub licence_feature: Option<String>,
    pub data_classification: Option<String>,
}

/// The outcome of a create: a new row, or a retired one revived.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub declaration: Declaration,
    /// A retired declaration was revived rather than a row inserted.
    pub reactivated: bool,
    /// The active declaration on the path was evolved to this new major.
    pub evolved: bool,
    /// The key an evolution retired, for the caller to evict with the new one.
    pub retired: Option<String>,
}

/// What the create's first phase found: the composed key, the normalised
/// display name and the declarations at the key and on its path.
#[domain_model]
#[derive(Debug, Clone)]
struct Located {
    key: SettingKey,
    display_name: Option<String>,
    existing: Option<Declaration>,
    on_path: Vec<Declaration>,
}

impl Located {
    /// The active declaration on the version-stripped path, whatever its major.
    fn active(&self) -> Option<&Declaration> {
        self.on_path.iter().find(|d| d.status.is_active())
    }

    /// The retired declaration a create here would revive: the one at the
    /// composed key, when nothing active is on the path.
    fn retired(&self) -> Option<&Declaration> {
        if self.active().is_some() {
            return None;
        }
        self.existing.as_ref().filter(|d| d.status.is_retired())
    }
}

/// What the create's second phase resolved for the dispatch: the derived
/// classification and the metadata a new or revived row carries.
#[domain_model]
struct Checked {
    derived: DerivedClassification,
    metadata: DeclarationMetadata,
}

/// The problem code of a revival report that has nothing to revive: no retired
/// declaration at the key, or an active one on its path. It rides the
/// `NotFound` the report answers, distinct from a missing category's.
pub const NOTHING_TO_REVIVE: &str = "nothing_to_revive";

/// A body that would revive a retired declaration, as far as the create's
/// first checks can tell.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RevivalPlan {
    retired: Declaration,
    located: Located,
}

impl RevivalPlan {
    /// The retired declaration the body would revive.
    #[must_use]
    pub fn retired(&self) -> &Declaration {
        &self.retired
    }
}

/// The tenants a report walks, the caller's first and the rest in
/// breadth-first order, and whether the node budget cut the walk.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    pub tenants: Vec<Uuid>,
    pub truncated: bool,
}

/// One retained value a revive would re-validate, as far as it is listed.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RevivalRow {
    pub tenant_id: Uuid,
    /// The inline value; `None` for a secret, held by reference.
    pub value: Option<Value>,
    /// The classification the row itself is stored under.
    pub data_classification: DataClassification,
    /// Whether the revive would flag the value: it no longer validates.
    pub needs_review: bool,
    /// Why, when it would be flagged.
    pub needs_review_detail: Option<String>,
}

/// What a revive would do to the retained values: every row counted, the
/// first `limit` of them, in the walk's order, listed.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RevivalReport {
    pub key: String,
    pub declaration_id: Uuid,
    /// The stricter of the declaration's stored classification and the body's
    /// derived one; a listed value is masked by the strictest of this and its
    /// row's own.
    pub data_classification: DataClassification,
    /// The first `limit` rows, values and details kept for these alone.
    pub rows: Vec<RevivalRow>,
    /// Every row found at the walked tenants.
    pub total: usize,
    /// How many of them the revive would flag.
    pub total_needs_review: usize,
    /// The tenants walked, the caller included.
    pub scanned: usize,
    /// Whether the walk was whole: the counts are all there are.
    pub complete: bool,
}

/// How many tenants of the walk one listing read asks for: the listing is
/// fetched in the walk's order, a batch at a time, until `limit` rows are in
/// hand. Well inside the bind-parameter bound of every backend.
const REVIVAL_TENANT_BATCH: usize = 500;

/// The port the report's read gate is asked through: whether the caller may
/// read the retired declaration the way the single read would show it — its
/// grant, administrative domain and `hidden` restriction, the licence
/// predicate joining in R2. The adapter owns the caller's scope and chain.
#[async_trait::async_trait]
pub trait RevivalReadGate: Send + Sync {
    /// # Errors
    /// [`DomainError`] when the gate's own reads fail.
    async fn may_read(&self, declaration: &Declaration) -> Result<bool, DomainError>;
}

/// The strictest of any number of classifications: `secret` over `pii` over
/// `public`, and `public` for none. A stored string reaches this through
/// [`DataClassification::from_stored`], which is where an unknown class fails
/// closed.
#[must_use]
pub fn strictest_classification(
    classifications: impl IntoIterator<Item = DataClassification>,
) -> DataClassification {
    classifications
        .into_iter()
        .max()
        .unwrap_or(DataClassification::Public)
}

/// What the value type's traits and the author's wish resolve to.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedClassification {
    pub has_secret_trait: bool,
    pub data_classification: DataClassification,
}

/// The class a field of an update request falls into.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldClass {
    /// Descriptive, or tightening a gate: applied at once.
    Immediate,
    /// Behaviour-affecting: expressible only as a replacement declaration.
    Immutable,
    /// Loosening a gate or a classification: needs credential step-up.
    StepUp,
}

/// The longest display name, in Unicode characters after trimming.
const DISPLAY_NAME_MAX_CHARS: usize = 256;

/// Trim a display name and check it is 1..=256 characters with no control
/// characters.
///
/// Counted in `chars()`, not bytes: the bound is about what a person reads.
///
/// # Errors
/// [`DomainError::Validation`] naming `display_name` when the trimmed value is
/// empty, longer than the bound, or containing a control character.
pub fn normalise_display_name(raw: &str) -> Result<String, DomainError> {
    let trimmed = raw.trim();
    let length = trimmed.chars().count();
    if length == 0 || length > DISPLAY_NAME_MAX_CHARS {
        return Err(validation(
            "display_name",
            field::VALIDATION,
            format!(
                "`display_name` must be 1 to {DISPLAY_NAME_MAX_CHARS} characters after trimming"
            ),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(validation(
            "display_name",
            field::VALIDATION,
            "`display_name` must not contain control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

fn validation(field: &str, code: &'static str, message: impl Into<String>) -> DomainError {
    DomainError::Validation {
        field: field.to_owned(),
        code,
        message: message.into(),
    }
}

/// What a revive may not change, refused before anything is written: the
/// secret boundary and the scope class, which would move stored values rather
/// than re-interpret them, and the value type, which the setting's own GTS
/// type is registered with.
fn revive_refusal(
    retired: &Declaration,
    request: &CreateDeclaration,
    derived: DerivedClassification,
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-13
    // What a revive may not change: the secret boundary (a secret's values
    // live by reference, everything else inline) and the scope class, which
    // would move values rather than re-interpret them; and the value type,
    // which the setting's own GTS type is registered with — the registry
    // does not replace a registered type, so a retyped row would disagree
    // with it for good. The secret boundary is checked first: crossing it is
    // always a retype too, and the more specific refusal says why.
    if retired.has_secret_trait != derived.has_secret_trait {
        return Err(conflict(
            conflict::SECRETNESS_CHANGED,
            format!(
                "`{}` is declared with `{}`; `{}` is on the other side of the secret \
                 boundary, and a revive does not move stored values across it",
                retired.key, retired.value_type_id, request.value_type_id
            ),
        ));
    }
    if retired.scope_class != request.scope_class {
        return Err(conflict(
            conflict::SCOPE_CLASS_CHANGED,
            format!(
                "`{}` is a `{}` setting; the scope class decides where a value may exist \
                 and is not revived into something else",
                retired.key, retired.scope_class
            ),
        ));
    }
    if retired.value_type_id != request.value_type_id {
        return Err(conflict(
            conflict::VALUE_TYPE_CHANGED,
            format!(
                "`{}` is declared with `{}` and its setting type is registered with it; \
                 a revive keeps that value type and cannot adopt `{}`",
                retired.key, retired.value_type_id, request.value_type_id
            ),
        ));
    }
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-13
    Ok(())
}

/// Whether a re-declaration changes what only a new major may carry: the value
/// type, the Schema Default or the scope class. Anything else is metadata.
fn changes_behaviour(active: &Declaration, request: &CreateDeclaration) -> bool {
    active.value_type_id != request.value_type_id
        || active.default_value != request.default_value
        || active.scope_class != request.scope_class
}

fn conflict(code: &str, message: impl std::fmt::Display) -> DomainError {
    DomainError::Conflict {
        detail: format!("{code}: {message}"),
    }
}

/// The `ETag` of a declaration: its normalized `updated_at`, as every tag here.
#[must_use]
pub fn etag_of(declaration: &Declaration) -> ETag {
    ETag::new(declaration.updated_at.unix_timestamp_nanos().to_string())
}

/// An empty value: the first half of the only default a secret setting may
/// carry — the other half, that it is a value of the declared type, is the
/// Type Validator's to decide. Shared by both declaration paths.
#[must_use]
pub fn is_empty_placeholder(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Derive `has_secret_trait` and `data_classification` from the value type's
/// traits and whatever the author supplied.
///
/// # Errors
/// [`DomainError::Validation`] when the author's classification contradicts
/// the trait: `secret` on a non-secret type, or anything but `secret` on a
/// secret one.
pub fn derive_classification(
    traits: &TraitSet,
    supplied: Option<&str>,
) -> Result<DerivedClassification, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-1
    let has_secret_trait = traits.secret;
    // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-1
    let data_classification = if has_secret_trait {
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-2
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-3
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-4
        if let Some(other) = supplied
            && other != DataClassification::Secret.as_str()
        {
            return Err(validation(
                "data_classification",
                field::CLASSIFICATION_CONFLICT,
                format!(
                    "the value type carries the secret trait; the classification is derived as \
                     `secret` and cannot be declared `{other}`"
                ),
            ));
        }
        DataClassification::Secret
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-4
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-3
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-2
    } else {
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-5
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-6
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-7
        match supplied {
            Some("secret") => {
                return Err(validation(
                    "data_classification",
                    field::CLASSIFICATION_CONFLICT,
                    "a value type without the secret trait cannot carry a `secret` \
                     classification",
                ));
            }
            Some("pii") => DataClassification::Pii,
            Some("public") | None => DataClassification::Public,
            Some(other) => {
                return Err(validation(
                    "data_classification",
                    field::VALIDATION,
                    format!("`{other}` is not a classification; use `public` or `pii`"),
                ));
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-7
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-6
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-5
    };
    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-8
    Ok(DerivedClassification {
        has_secret_trait,
        data_classification,
    })
    // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-classification:p1:inst-decl-class-8
}

/// Classify one field of an update request against the current declaration.
///
/// # Errors
/// [`DomainError::Validation`] when the value has the wrong shape for the field.
pub fn classify_field(
    name: &str,
    value: &Value,
    current: &Declaration,
) -> Result<FieldClass, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-1
    let wrong_shape = |expected: &str| {
        validation(
            name,
            field::VALIDATION,
            format!("`{name}` must be {expected}"),
        )
    };
    let class = match name {
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-2
        "description" | "domain_affinity" | "licence_feature" => {
            if !(value.is_string() || value.is_null()) {
                return Err(wrong_shape("a string or null"));
            }
            FieldClass::Immediate
        }
        "display_name" => {
            if !value.is_null() {
                let Some(raw) = value.as_str() else {
                    return Err(wrong_shape("a string or null"));
                };
                normalise_display_name(raw)?;
            }
            FieldClass::Immediate
        }
        "mode" => match value.as_str() {
            Some("standard" | "advanced") => FieldClass::Immediate,
            _ => return Err(wrong_shape("`standard` or `advanced`")),
        },
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-2
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-8
        // Tightening a gate is immediate; loosening one needs step-up whatever
        // the flag says now, or a live session could clear the gate and then
        // write with no re-verification anywhere in the sequence.
        "requires_step_up" => match value.as_bool() {
            Some(true) => FieldClass::Immediate,
            Some(false) => FieldClass::StepUp,
            None => return Err(wrong_shape("a boolean")),
        },
        "anonymous_exposable" => match value.as_bool() {
            Some(false) => FieldClass::Immediate,
            Some(true) => FieldClass::StepUp,
            None => return Err(wrong_shape("a boolean")),
        },
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-8
        "data_classification" => match value.as_str() {
            // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-6
            Some("secret") => FieldClass::Immutable,
            // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-6
            Some(requested @ ("public" | "pii")) => {
                match (current.classification(), requested) {
                    // Derived from the trait: never author-changed.
                    (DataClassification::Secret, _) => FieldClass::Immutable,
                    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-5
                    // Loosening un-masks content previously withheld.
                    (DataClassification::Pii, "public") => FieldClass::StepUp,
                    // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-5
                    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-4
                    // Tightening, and a no-op, both apply at once.
                    _ => FieldClass::Immediate,
                    // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-4
                }
            }
            _ => return Err(wrong_shape("`public` or `pii`")),
        },
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-3
        // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-7
        // Behaviour-affecting, and everything unrecognized with them: an
        // unknown field can never take the immediate path.
        _ => FieldClass::Immutable,
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-7
        // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-3
    };
    Ok(class)
    // @cpt-end:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-1
}

fn snapshot(declaration: &Declaration) -> Value {
    serde_json::json!({
        "key": declaration.key,
        "value_type_id": declaration.value_type_id,
        "default_value": declaration.default_value,
        "scope_class": declaration.scope_class,
        "mode": declaration.mode,
        "status": declaration.status.as_str(),
        "description": declaration.description,
        "display_name": declaration.display_name,
        "domain_affinity": declaration.domain_affinity,
        "licence_feature": declaration.licence_feature,
        "data_classification": declaration.data_classification,
        "requires_step_up": declaration.requires_step_up,
        "anonymous_exposable": declaration.anonymous_exposable,
        "source": declaration.source,
    })
}

fn optional_string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// The administrative authoring service.
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-key:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-schema-default:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-scope-class:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-classification:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-mutation-classes:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-lifecycle:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-contributed-protection:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-audit:p1
// @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-revival-report:p1
#[domain_model]
pub struct DeclarationAdmin<R, Cat, Val, S, P> {
    declarations: R,
    categories: Cat,
    values: Val,
    validator: Arc<dyn TypeValidator>,
    registrar: Arc<dyn SettingTypeRegistrar>,
    step_up: Arc<dyn StepUpVerifier>,
    sink: S,
    cache: Arc<EffectiveCache>,
    pending: P,
    secrets: Arc<dyn SecretManager>,
    metrics: Arc<dyn WriteMetrics>,
}

impl<R, Cat, Val, S, P> DeclarationAdmin<R, Cat, Val, S, P>
where
    R: DeclarationRepository,
    Cat: CategoryRepository,
    Val: ValueRepository,
    S: AuditSink,
    P: PendingSecretRepository,
{
    /// Assemble the service over its repositories and ports.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        declarations: R,
        categories: Cat,
        values: Val,
        validator: Arc<dyn TypeValidator>,
        registrar: Arc<dyn SettingTypeRegistrar>,
        step_up: Arc<dyn StepUpVerifier>,
        sink: S,
        cache: Arc<EffectiveCache>,
        pending: P,
        secrets: Arc<dyn SecretManager>,
        metrics: Arc<dyn WriteMetrics>,
    ) -> Self {
        Self {
            declarations,
            categories,
            values,
            validator,
            registrar,
            step_up,
            sink,
            cache,
            pending,
            secrets,
            metrics,
        }
    }

    /// Create a declaration, or revive the retired one at the same key.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] on the category; [`DomainError::Validation`]
    /// for a key segment, classification, default or scope class the rules
    /// refuse; [`DomainError::Conflict`] for an active declaration at the key
    /// or a leaf name held in the category, and for a revive that changes what
    /// a revive may not; [`DomainError::StepUpRequired`] /
    /// [`DomainError::Unauthorized`] when a revive lacks step-up.
    pub async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        request: CreateDeclaration,
        actor: &WriteActor,
    ) -> Result<Created, DomainError> {
        let located = self.locate(conn, scope, &request).await?;
        let Checked { derived, metadata } = self.check(&located, &request).await?;
        let Located {
            key,
            existing,
            on_path,
            ..
        } = located;

        if let Some(active) = on_path.iter().find(|d| d.status.is_active()) {
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-2
            // A request matching the active declaration is not evolution: a
            // retry after a lost response must not mint a major by accident,
            // and a metadata change goes through PATCH.
            if !changes_behaviour(active, &request) {
                return Err(conflict(
                    conflict::KEY_CONFLICT,
                    format!(
                        "`{}` is the active declaration of this setting and the request changes \
                         none of its value type, Schema Default or scope class; metadata changes \
                         use PATCH",
                        active.key
                    ),
                ));
            }
            // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-2
            return self
                .evolve(
                    conn,
                    scope,
                    &key,
                    active.clone(),
                    &on_path,
                    &request,
                    derived,
                    metadata,
                    actor,
                )
                .await;
        }

        match existing {
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-6
            Some(active) if active.status.is_active() => Err(conflict(
                conflict::KEY_CONFLICT,
                format!(
                    "`{}` is an active declaration; metadata changes use PATCH",
                    active.key
                ),
            )),
            // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-6
            Some(retired) => {
                self.revive(
                    conn, scope, &key, retired, &request, derived, metadata, actor,
                )
                .await
            }
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-5
            None => {
                self.insert_new(conn, scope, &key, &request, derived, metadata, actor)
                    .await
            } // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-5
        }
    }

    /// Plan a revival report: the create's first checks on the body, whether
    /// the body would revive a retired declaration at all, and whether the
    /// caller may read that declaration.
    ///
    /// # Errors
    /// The create's own refusals up to the lookup, as [`Self::create`] answers
    /// them; [`DomainError::NotFound`] with [`NOTHING_TO_REVIVE`] when no
    /// retired declaration sits at the key, an active one sits on the path —
    /// the create would then evolve or duplicate, not revive — or the gate
    /// refuses the retired declaration to the caller.
    pub async fn plan_revival<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        request: &CreateDeclaration,
        gate: &dyn RevivalReadGate,
    ) -> Result<RevivalPlan, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-3
        let located = self.locate(conn, scope, request).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-3
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-4
        let nothing = || DomainError::NotFound {
            resource: NOTHING_TO_REVIVE,
        };
        let retired = located.retired().cloned().ok_or_else(nothing)?;
        // One answer for every way of not being there, so what the caller
        // may not read is not told apart from what does not exist.
        if !gate.may_read(&retired).await? {
            return Err(nothing());
        }
        Ok(RevivalPlan { retired, located })
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-4
    }

    /// Report what a revive of the planned declaration would do to the values
    /// it retained, changing nothing: the create's remaining checks, the leaf
    /// name, the retained subject-less rows at the walked tenants, each
    /// re-validated as the revive would re-validate it. Every row is counted;
    /// the first `limit`, in the walk's order, are listed.
    ///
    /// The walk is asked for only here, after every refusal.
    ///
    /// # Errors
    /// The create's refusals up to step-up, as [`Self::create`] answers them;
    /// [`DomainError::Conflict`] with `leaf_name_taken` when an active
    /// declaration holds the leaf name in the category;
    /// [`DomainError::Unavailable`] when the Type Validator cannot answer.
    pub async fn revival_report<C, F, Fut>(
        &self,
        conn: &C,
        value_scope: &AccessScope,
        plan: RevivalPlan,
        request: &CreateDeclaration,
        limit: usize,
        walk: F,
    ) -> Result<RevivalReport, DomainError>
    where
        C: DBRunner,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Walk, DomainError>>,
    {
        let RevivalPlan { retired, located } = plan;
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-5
        let checked = self.check(&located, request).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-5
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-6
        // Unscoped, as the revive's unique index is: a holder outside the
        // caller's scope still blocks the revive, so it must be found. What
        // this tells the caller — that the leaf name is taken in the
        // declaration's own category — the create tells it too, as its insert
        // collides on the same index.
        if self
            .declarations
            .find_active_by_leaf(
                conn,
                &AccessScope::allow_all(),
                retired.category_id,
                &retired.leaf_slug,
            )
            .await?
            .is_some_and(|holder| holder.id != retired.id)
        {
            return Err(conflict(
                conflict::LEAF_NAME_TAKEN,
                format!(
                    "the leaf name `{}` is held by an active declaration in this category; \
                     a revive of `{}` would collide with it",
                    retired.leaf_slug, retired.key
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-6
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-7
        let walked = walk().await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-7
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-8
        // Three reads, none of them whole rows. The count is every row of the
        // declaration at the walked tenants; the grouped read is its distinct
        // `(value, classification)` pairs with their multiplicity; the listing
        // is the first `limit` rows in the walk's order, fetched a batch of
        // tenants at a time and no further than `limit` needs. The work grows
        // with `limit` and the number of distinct values, not the subtree.
        let total = self
            .values
            .count_in_tenants(conn, value_scope, retired.id, &walked.tenants)
            .await?;
        let groups = self
            .values
            .distinct_values_in_tenants(conn, value_scope, retired.id, &walked.tenants)
            .await?;
        // The walk's position of every tenant, built once: the order of a
        // batch is a sort on an O(1) lookup.
        let position: HashMap<Uuid, usize> = walked
            .tenants
            .iter()
            .enumerate()
            .map(|(at, tenant)| (*tenant, at))
            .collect();
        let mut listed = Vec::with_capacity(limit.min(walked.tenants.len()));
        for batch in walked.tenants.chunks(REVIVAL_TENANT_BATCH) {
            if listed.len() >= limit {
                break;
            }
            let mut found = self
                .values
                .list_in_tenants(conn, value_scope, retired.id, batch)
                .await?;
            found.sort_by_key(|row| position.get(&row.tenant_id).copied().unwrap_or(usize::MAX));
            listed.extend(found);
        }
        listed.truncate(limit);
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-9
        // A value shared by many rows — the same override copied down a
        // tree — is judged once: the verdict depends on the value and the
        // type alone, so it is reused for every row holding that value. The
        // groups are judged first; a group that fails adds its multiplicity to
        // the count. The verdicts are keyed by the value itself: `Value`
        // hashes an object's members in key order whatever the build, as its
        // equality ignores their order, so one verdict stands per value — a
        // backend that groups JSON textually (sqlite) may return two equal
        // objects with different key order as two groups, and they still meet
        // one judgement. A secret row has no value here to judge.
        let mut verdicts: HashMap<Value, Option<String>> = HashMap::new();
        let mut total_needs_review: u64 = 0;
        for group in &groups {
            if let Some(value) = &group.value
                && self
                    .verdict_of(&retired.value_type_id, &mut verdicts, value)
                    .await?
                    .is_some()
            {
                total_needs_review += group.count;
            }
        }
        // Each listed row takes its verdict from the same map; a value the
        // groups somehow did not cover is judged on the spot.
        let mut rows = Vec::with_capacity(listed.len());
        for row in listed {
            let detail = match &row.value {
                None => None,
                Some(value) => {
                    self.verdict_of(&retired.value_type_id, &mut verdicts, value)
                        .await?
                }
            };
            rows.push(RevivalRow {
                tenant_id: row.tenant_id,
                value: row.value,
                data_classification: DataClassification::from_stored(&row.data_classification),
                needs_review: detail.is_some(),
                needs_review_detail: detail,
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-revival-report:p1:inst-decl-revival-9
        Ok(RevivalReport {
            key: retired.key.clone(),
            declaration_id: retired.id,
            data_classification: strictest_classification([
                retired.classification(),
                checked.derived.data_classification,
            ]),
            rows,
            total: usize::try_from(total).unwrap_or(usize::MAX),
            total_needs_review: usize::try_from(total_needs_review).unwrap_or(usize::MAX),
            scanned: walked.tenants.len(),
            complete: !walked.truncated,
        })
    }

    /// The create's first phase, which the revival report shares: the
    /// category, the key composed from the body, its display name and Scope
    /// Class, the declarations found at the key and on its version-stripped
    /// path, and the refusal of an active `module_contributed` one on the path.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] on the category; [`DomainError::Validation`]
    /// for a key segment, display name or Scope Class the rules refuse;
    /// [`DomainError::Conflict`] for an active contributed declaration on the
    /// path.
    async fn locate<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        request: &CreateDeclaration,
    ) -> Result<Located, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-5
        let category = self
            .categories
            .find(conn, scope, request.category_id)
            .await?
            .ok_or(DomainError::NotFound {
                resource: "category",
            })?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-5
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-6
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-7
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-4
        let key = SettingKey::compose(&request.vendor, category.key.as_str(), &request.name)
            .map_err(|err| match err {
                SettingKeyError::InvalidSegment { segment, cause, .. } => validation(
                    "key",
                    field::SETTING_KEY_SEGMENT,
                    format!("segment `{segment}` violates the GTS grammar: {cause}"),
                ),
                other => validation("key", field::SETTING_KEY_SEGMENT, other.to_string()),
            })?;
        let display_name = request
            .display_name
            .as_deref()
            .map(normalise_display_name)
            .transpose()?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-7
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-6
        if !matches!(
            request.scope_class.as_str(),
            "global" | "cascading" | "local"
        ) {
            return Err(validation(
                "scope_class",
                field::SCOPE_CLASS_INVALID,
                "every declaration names its scope class: `global`, `cascading` or `local`",
            ));
        }
        let existing = self
            .declarations
            .find_by_key(conn, scope, key.as_str())
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-1
        // The active declaration on the version-stripped path comes first,
        // whatever its major: after `v1 → v2` the composed `v1` key is retired,
        // and looking it up alone would revive it beside the live `v2`.
        let on_path = self.declarations_on_path(conn, scope, &key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-1
        let located = Located {
            key,
            display_name,
            existing,
            on_path,
        };
        // Who may change the row at all is judged before anything about the
        // request is resolved: a gear's declaration answers with its owner,
        // whatever value type, classification or default the request names —
        // no registry lookup and no classification rule stands in front of
        // that answer — and ahead of step-up, which is asked only of a change
        // that could go through. Nothing keeps a module out of the `settings`
        // package an admin key is composed with, so its declaration can sit at
        // this key or on this path.
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-8
        if let Some(active) = located.active()
            && active.source == "module_contributed"
        {
            return Err(conflict(
                conflict::CONTRIBUTED_IMMUTABLE,
                format!(
                    "`{}` is contributed by a gear; it evolves when that gear registers a new \
                     major",
                    active.key
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-8
        Ok(located)
    }

    /// The create's second phase, which the revival report shares: everything
    /// the create judges before it would ask for step-up — a retired
    /// `module_contributed` declaration, the value type's traits and the
    /// derived classification, a revive's own refusals, the Schema Default and
    /// the remaining body fields.
    ///
    /// # Errors
    /// [`DomainError::Conflict`], [`DomainError::Validation`] and
    /// [`DomainError::Unavailable`], as the create answers them.
    async fn check(
        &self,
        located: &Located,
        request: &CreateDeclaration,
    ) -> Result<Checked, DomainError> {
        let retired = located.retired();
        if let Some(retired) = retired {
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-15
            // A revive would replace its Schema Default and metadata.
            Self::refuse_contributed(retired)?;
            // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-15
        }
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-9
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-10
        let traits = self
            .validator
            .resolve_traits(&request.value_type_id)
            .await?;
        let derived = derive_classification(&traits, request.data_classification.as_deref())?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-10
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-9
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-8
        // A revive's own refusals are about the retired row, not the default:
        // judged first, so a retype is refused as one rather than as a default
        // that does not fit the type it could never adopt.
        if let Some(retired) = retired {
            revive_refusal(retired, request, derived)?;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-16
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-12
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-13
        if derived.has_secret_trait && !is_empty_placeholder(&request.default_value) {
            return Err(validation(
                "default_value",
                field::SECRET_DEFAULT_NOT_EMPTY,
                "a secret setting has no secret default: the placeholder is an empty value \
                 of the type, and the credential is set as a value at a scope",
            ));
        }
        // Every default is a value of its type — a secret's placeholder too:
        // empty is not enough, `[]` is no string and `null` only fits a type
        // that admits it (DESIGN §4.2). The contribution path applies the same
        // rule.
        self.validator
            .validate_value(&request.value_type_id, &request.default_value)
            .await?
            .into_result()?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-13
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-12
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-14
        let anonymous_exposable = request.anonymous_exposable.unwrap_or(false);
        if anonymous_exposable && derived.data_classification != DataClassification::Public {
            return Err(validation(
                "anonymous_exposable",
                field::EXPOSABLE_NOT_SENSITIVE,
                format!(
                    "a `{}` setting cannot be exposed on the anonymous surface",
                    derived.data_classification.as_str()
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-14
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-15
        let mode = match request.mode.as_deref() {
            Some("advanced") => "advanced",
            Some("standard") | None => "standard",
            Some(other) => {
                return Err(validation(
                    "mode",
                    field::VALIDATION,
                    format!("`{other}` is not a mode; use `standard` or `advanced`"),
                ));
            }
        };
        let metadata = DeclarationMetadata {
            mode: mode.to_owned(),
            display_name: located.display_name.clone(),
            description: request.description.clone(),
            domain_affinity: request.domain_affinity.clone(),
            licence_feature: request.licence_feature.clone(),
            data_classification: derived.data_classification.as_str().to_owned(),
            requires_step_up: request.requires_step_up.unwrap_or(true),
            anonymous_exposable,
        };
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-15
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-16
        Ok(Checked { derived, metadata })
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_new<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &SettingKey,
        request: &CreateDeclaration,
        derived: DerivedClassification,
        metadata: DeclarationMetadata,
        actor: &WriteActor,
    ) -> Result<Created, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-16
        // The setting is itself a GTS type. Registration is idempotent, so a
        // retry after a failed insert reuses the type rather than minting one.
        self.registrar
            .register_setting_type(key, &request.value_type_id)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-16
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-17
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-18
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-19
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-1
        let draft = DeclarationDraft {
            key: key.to_string(),
            leaf_slug: key.leaf_slug().to_owned(),
            value_type_id: request.value_type_id.clone(),
            category_id: request.category_id,
            default_value: request.default_value.clone(),
            scope_class: request.scope_class.clone(),
            mode: metadata.mode.clone(),
            requires_step_up: metadata.requires_step_up,
            anonymous_exposable: metadata.anonymous_exposable,
            domain_affinity: metadata.domain_affinity.clone(),
            has_secret_trait: derived.has_secret_trait,
            data_classification: derived.data_classification.as_str().to_owned(),
            source: "admin_authored".to_owned(),
            owner_module: None,
            licence_feature: metadata.licence_feature.clone(),
            display_name: metadata.display_name.clone(),
            description: metadata.description.clone(),
            created_by: actor.subject(),
        };
        // The key was looked up just above, so a unique violation here is the
        // leaf name held by an active declaration in this category (or a race
        // on the key, which the same answer covers).
        let inserted = match self.declarations.insert(conn, scope, draft).await {
            Ok(inserted) => inserted,
            Err(DomainError::Conflict { .. }) => {
                return Err(conflict(
                    conflict::LEAF_NAME_TAKEN,
                    format!(
                        "the leaf name `{}` is held by an active declaration in this category, \
                         or `{key}` was declared concurrently",
                        key.leaf_slug()
                    ),
                ));
            }
            Err(other) => return Err(other),
        };
        // @cpt-end:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-1
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-19
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-18
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-17
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-20
        self.record(
            conn,
            &inserted,
            actor,
            AuditOperation::Create,
            None,
            Some(snapshot(&inserted)),
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-20
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-21
        Ok(Created {
            declaration: inserted,
            reactivated: false,
            evolved: false,
            retired: None,
        })
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-21
    }

    #[allow(clippy::too_many_arguments)]
    async fn revive<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &SettingKey,
        retired: Declaration,
        request: &CreateDeclaration,
        derived: DerivedClassification,
        metadata: DeclarationMetadata,
        actor: &WriteActor,
    ) -> Result<Created, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-7
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-8
        // Reactivation changes whether a live setting resolves: gated like a
        // value change, before anything else is inspected.
        self.verify_step_up(actor, StepUpOperation::Reactivate)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-8
        // The row for update, then read again by id: a purge of this retired
        // declaration holds the row until it commits, so this waits for it and
        // finds the row gone (`404`) rather than writing against it. What was
        // read before the lock is only a hint.
        self.declarations
            .lock_for_update(conn, scope, retired.id)
            .await?;
        let retired = self.find(conn, scope, retired.id).await?;
        if !retired.status.is_retired() {
            return Err(conflict(
                conflict::KEY_CONFLICT,
                format!(
                    "`{}` is an active declaration; metadata changes use PATCH",
                    retired.key
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-7
        // Again here, whoever calls: `create` asks first, before the default.
        revive_refusal(&retired, request, derived)?;
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-14
        // The type the row goes live under is the registered one, confirmed
        // before anything is written — as a create registers before it inserts.
        self.registrar
            .register_setting_type(key, &retired.value_type_id)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-14
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-9
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-3
        let classification_changed = retired.data_classification != metadata.data_classification;
        let new_classification = metadata.data_classification.clone();
        if retired.default_value != request.default_value {
            // Adopted as declared: the Schema Default was validated against the
            // type on the way in.
            self.declarations
                .set_default(conn, scope, retired.id, &request.default_value)
                .await?;
        }
        let redefines = metadata.redefines(&retired);
        self.declarations
            .update_metadata(conn, scope, retired.id, metadata, None, redefines)
            .await?;
        // Every retained value is re-validated against the type it goes live
        // under, which may have gained a compatible revision while the setting
        // sat retired. What fails is flagged with its detail
        // and falls through on read rather than being served or discarded;
        // what validates again has its flag cleared.
        for row in self.values.find_all(conn, scope, retired.id).await? {
            let detail = self.revalidate(&retired.value_type_id, &row).await?;
            if detail.is_some() != row.needs_review {
                self.values.flag(conn, scope, row.id, detail).await?;
            }
        }
        if let Err(err) = self
            .declarations
            .set_status(conn, scope, retired.id, DeclarationStatus::Active, None)
            .await
        {
            return Err(match err {
                DomainError::Conflict { .. } => conflict(
                    conflict::LEAF_NAME_TAKEN,
                    format!(
                        "the leaf name `{}` is held by an active declaration in this category; \
                         the name was re-used and the two cannot both be live",
                        retired.leaf_slug
                    ),
                ),
                other => other,
            });
        }
        if classification_changed {
            self.values
                .resync_classification(conn, scope, retired.id, &new_classification)
                .await?;
        }
        let revived = self.reload(conn, scope, &retired.key).await?;
        // @cpt-end:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-3
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-9
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-10
        // Retained values re-enter resolution: nothing cached may keep saying
        // the setting is gone.
        self.cache.invalidate_key(&revived.key);
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-10
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-11
        self.record(
            conn,
            &revived,
            actor,
            AuditOperation::Change,
            Some(snapshot(&retired)),
            Some(snapshot(&revived)),
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-12
        Ok(Created {
            declaration: revived,
            reactivated: true,
            evolved: false,
            retired: None,
        })
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-12
    }

    /// Evolve the active declaration of a setting to the next free major: the
    /// predecessor retired, the successor inserted under its own key and type,
    /// every value copied to it and re-validated, all in the caller's
    /// transaction. Nothing outside it sees the path without an active major,
    /// and a failure anywhere leaves the predecessor active and untouched.
    #[allow(clippy::too_many_arguments)]
    async fn evolve<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        first: &SettingKey,
        active: Declaration,
        on_path: &[Declaration],
        request: &CreateDeclaration,
        derived: DerivedClassification,
        metadata: DeclarationMetadata,
        actor: &WriteActor,
    ) -> Result<Created, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-3
        // Evolution retires a live setting's current major: gated like a value
        // change, before anything else is inspected.
        self.verify_step_up(actor, StepUpOperation::Evolve).await?;
        // The active row for update, then read again by id: a retire and a
        // purge of it, or another evolution, hold it until they commit, so
        // this waits for them and sees what they left — a row gone is `404`,
        // one no longer active is the conflict a second evolution gets.
        self.declarations
            .lock_for_update(conn, scope, active.id)
            .await?;
        let active = self.find(conn, scope, active.id).await?;
        if !active.status.is_active() {
            return Err(conflict(
                conflict::KEY_CONFLICT,
                format!(
                    "`{}` is no longer the active declaration of this setting; read it again",
                    active.key
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-3
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-4
        // A contributed declaration never reaches this point: `create` refuses
        // it before anything else.
        // A secret's values live by reference and everything else inline: a
        // copy across that boundary would re-interpret stored values, not move
        // them, exactly as a revive may not.
        if active.has_secret_trait != derived.has_secret_trait {
            return Err(conflict(
                conflict::SECRETNESS_CHANGED,
                format!(
                    "`{}` is declared with `{}`; `{}` is on the other side of the secret \
                     boundary, and an evolution does not move stored values across it",
                    active.key, active.value_type_id, request.value_type_id
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-5
        // One above the highest major the path has ever used, retired ones
        // included: a retired major keeps its key, its type and its values.
        let highest = on_path
            .iter()
            .filter_map(|d| SettingKey::parse(&d.key).ok())
            .map(|k| k.major())
            .max()
            .unwrap_or(1);
        let next = highest
            .checked_add(1)
            .and_then(std::num::NonZeroU32::new)
            .ok_or_else(|| DomainError::Internal {
                diagnostic: format!("`{}` has no major left above {highest}", active.key),
            })?;
        let successor_key =
            SettingKey::compose_at(&request.vendor, first.category_slug(), &request.name, next)
                .map_err(|err| DomainError::Internal {
                    diagnostic: format!("the next major of `{first}` does not compose: {err}"),
                })?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-5
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-6
        // The predecessor retires first, and only because it must: both majors
        // hold one leaf name in one category, and an active row per pair is
        // all the unique index admits.
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-5
        self.declarations
            .set_status(conn, scope, active.id, DeclarationStatus::Retired, None)
            .await?;
        // @cpt-end:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-5
        let retired = self.reload(conn, scope, &active.key).await?;
        self.record(
            conn,
            &retired,
            actor,
            AuditOperation::Remove,
            Some(snapshot(&active)),
            Some(snapshot(&retired)),
        )
        .await?;
        // The successor as any new declaration: its own type registered before
        // its row is written, its create recorded.
        let successor = self
            .insert_new(
                conn,
                scope,
                &successor_key,
                request,
                derived,
                metadata,
                actor,
            )
            .await?
            .declaration;
        // Every value moves to the successor at the same scope, read under an
        // update lock so a write gated while the predecessor was active is
        // either copied here or refused when it sees the row retired. A copy
        // that no longer validates is stored flagged, never coerced or dropped.
        for row in self.values.lock_all(conn, scope, active.id).await? {
            let detail = self.revalidate(&request.value_type_id, &row).await?;
            self.values
                .insert(
                    conn,
                    scope,
                    ValueDraft {
                        declaration_id: successor.id,
                        tenant_id: row.tenant_id,
                        value: row.value.clone(),
                        secret_ref: row.secret_ref.clone(),
                        data_classification: derived.data_classification.as_str().to_owned(),
                        needs_review: detail.is_some(),
                        needs_review_detail: detail,
                        set_by: row.set_by.clone(),
                        set_by_tenant: row.set_by_tenant,
                    },
                )
                .await?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-evolve:p1:inst-decl-evolve-6
        Ok(Created {
            declaration: successor,
            reactivated: false,
            evolved: true,
            retired: Some(active.key),
        })
    }

    /// Every declaration on the version-stripped path of `key`, whatever its
    /// major or status.
    async fn declarations_on_path<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &SettingKey,
    ) -> Result<Vec<Declaration>, DomainError> {
        let path = key.version_stripped_path();
        let prefix = format!("{}{path}.v", settings_service_sdk::gts::SETTING_TYPE_BASE);
        Ok(self
            .declarations
            .find_by_key_prefix(conn, scope, &prefix)
            .await?
            .into_iter()
            // A `LIKE` prefix is not a path — `_` in a slug is a wildcard there,
            // so `retry_policy` also matches `retryXpolicy` — and each key is
            // re-checked against the exact path.
            .filter(|d| SettingKey::parse(&d.key).is_ok_and(|k| k.version_stripped_path() == path))
            .collect())
    }

    /// The verdict on one value, judged once: found in `verdicts` by hash and
    /// `Value` equality, or judged now and remembered.
    async fn verdict_of(
        &self,
        value_type_id: &str,
        verdicts: &mut HashMap<Value, Option<String>>,
        value: &Value,
    ) -> Result<Option<String>, DomainError> {
        if let Some(verdict) = verdicts.get(value) {
            return Ok(verdict.clone());
        }
        let judged = self.judge(value_type_id, value).await?;
        verdicts.insert(value.clone(), judged.clone());
        Ok(judged)
    }

    /// The first violation of a value against `value_type_id`, as the detail
    /// a `needs_review` flag carries; `None` when it validates.
    async fn judge(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<Option<String>, DomainError> {
        let result = self.validator.validate_value(value_type_id, value).await?;
        Ok(result
            .violations
            .first()
            .map(|first| format!("{} — {}", first.field, first.message)))
    }

    /// The first violation of a retained value against `value_type_id`, as
    /// the detail a `needs_review` flag carries; `None` when it validates, and
    /// for a secret row, whose plaintext is not here to validate.
    async fn revalidate(
        &self,
        value_type_id: &str,
        row: &StoredValue,
    ) -> Result<Option<String>, DomainError> {
        let Some(value) = &row.value else {
            return Ok(None);
        };
        self.judge(value_type_id, value).await
    }

    /// Edit a declaration's metadata under the mutation-class discipline.
    ///
    /// # Errors
    /// [`DomainError::NotFound`]; [`DomainError::Conflict`] on a contributed
    /// declaration; [`DomainError::PreconditionRequired`] /
    /// [`DomainError::PreconditionFailed`] on the tag; [`DomainError::Validation`]
    /// for an immutable, unknown or ill-shaped field and for exposing a
    /// sensitive setting; [`DomainError::StepUpRequired`] /
    /// [`DomainError::Unauthorized`] when a field needs step-up the caller
    /// cannot supply.
    pub async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        if_match: Option<&str>,
        patch: &Map<String, Value>,
        actor: &WriteActor,
    ) -> Result<Declaration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-5
        let current = self.find(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-5
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-6
        Self::refuse_contributed(&current)?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-6
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-7
        precondition::evaluate(if_match, &etag_of(&current))?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-7
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-9
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-10
        let mut needs_step_up = false;
        for (name, value) in patch {
            match classify_field(name, value, &current)? {
                FieldClass::Immediate => {}
                FieldClass::StepUp => needs_step_up = true,
                FieldClass::Immutable => {
                    return Err(validation(
                        name,
                        field::DECLARATION_FIELD_IMMUTABLE,
                        format!(
                            "`{name}` is not editable in place: the change is expressible only \
                             as a replacement declaration"
                        ),
                    ));
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-9
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-11
        if needs_step_up {
            self.verify_step_up(actor, StepUpOperation::Update).await?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-11
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-10
        let mut metadata = DeclarationMetadata {
            mode: current.mode.clone(),
            display_name: current.display_name.clone(),
            description: current.description.clone(),
            domain_affinity: current.domain_affinity.clone(),
            licence_feature: current.licence_feature.clone(),
            data_classification: current.data_classification.clone(),
            requires_step_up: current.requires_step_up,
            anonymous_exposable: current.anonymous_exposable,
        };
        for (name, value) in patch {
            match name.as_str() {
                "description" => metadata.description = optional_string(value),
                "display_name" => {
                    // Validated by `classify_field` before this point; `null`
                    // clears the label.
                    metadata.display_name = value.as_str().map(|raw| raw.trim().to_owned());
                }
                "domain_affinity" => metadata.domain_affinity = optional_string(value),
                "licence_feature" => metadata.licence_feature = optional_string(value),
                "mode" => metadata.mode = value.as_str().unwrap_or("standard").to_owned(),
                "requires_step_up" => metadata.requires_step_up = value.as_bool().unwrap_or(true),
                "anonymous_exposable" => {
                    metadata.anonymous_exposable = value.as_bool().unwrap_or(false);
                }
                "data_classification" => {
                    metadata.data_classification = value
                        .as_str()
                        .unwrap_or(DataClassification::Public.as_str())
                        .to_owned();
                }
                _ => {}
            }
        }
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-12
        if metadata.anonymous_exposable
            && DataClassification::from_stored(&metadata.data_classification)
                != DataClassification::Public
        {
            return Err(validation(
                "anonymous_exposable",
                field::EXPOSABLE_NOT_SENSITIVE,
                format!(
                    "a `{}` setting cannot be exposed on the anonymous surface",
                    metadata.data_classification
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-12
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-13
        let classification_changed = metadata.data_classification != current.data_classification;
        let new_classification = metadata.data_classification.clone();
        // At the version the tag was compared against: a row that moved in
        // between is changed by nobody twice. The definition recency moves
        // only when what a reader is served changes — a description does not.
        let redefines = metadata.redefines(&current);
        self.declarations
            .update_metadata(
                conn,
                scope,
                id,
                metadata,
                Some(current.updated_at),
                redefines,
            )
            .await?;
        if classification_changed {
            // The denormalized class on every stored value follows, so masking
            // reads one column and never disagrees with the declaration.
            self.values
                .resync_classification(conn, scope, id, &new_classification)
                .await?;
        }
        let updated = self.reload(conn, scope, &current.key).await?;
        // The cached effective value is a function of the declaration — it
        // carries the classification masking reads and the domain affinity
        // the visibility rule applies — so the key is evicted with the change,
        // here inside the transaction and again by the caller once it commits,
        // exactly as a retire is. Unconditionally: choosing by field would be
        // a fragile saving on a rare operation.
        self.cache.invalidate_key(&updated.key);
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-13
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-14
        self.record(
            conn,
            &updated,
            actor,
            AuditOperation::Change,
            Some(snapshot(&current)),
            Some(snapshot(&updated)),
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-14
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-15
        Ok(updated)
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-15
    }

    /// Retire a declaration: a soft delete that keeps every value.
    ///
    /// # Errors
    /// [`DomainError::StepUpRequired`] / [`DomainError::Unauthorized`] without
    /// step-up; [`DomainError::NotFound`]; [`DomainError::Conflict`] on a
    /// contributed declaration; the precondition errors on the tag.
    pub async fn retire<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        if_match: Option<&str>,
        actor: &WriteActor,
    ) -> Result<Declaration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-5
        // Retire drops a live setting out of resolution at once: gated like a
        // value change, before the declaration is even looked up.
        self.verify_step_up(actor, StepUpOperation::Retire).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-5
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-6
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-7
        let current = self.find(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-7
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-6
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-8
        Self::refuse_contributed(&current)?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-9
        precondition::evaluate(if_match, &etag_of(&current))?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-9
        if current.status.is_retired() {
            return Ok(current);
        }
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-10
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-11
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-2
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-4
        // The status flips; every row in setting_values stays where it is,
        // excluded from resolution by the status alone and revived with it. A
        // retired declaration keeps occupying its category with them.
        self.declarations
            .set_status(
                conn,
                scope,
                id,
                DeclarationStatus::Retired,
                Some(current.updated_at),
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-4
        // @cpt-end:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-2
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-11
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-10
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-12
        // Evicted here, inside the transaction, and again by the caller once it
        // commits, so a read between the two cannot pin the old row until the
        // TTL runs out.
        self.cache.invalidate_key(&current.key);
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-12
        let retired = self.reload(conn, scope, &current.key).await?;
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-13
        self.record(
            conn,
            &retired,
            actor,
            AuditOperation::Remove,
            Some(snapshot(&current)),
            Some(snapshot(&retired)),
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-13
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-14
        Ok(retired)
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-retire:p1:inst-decl-retire-14
    }

    /// Purge a retired, admin-authored declaration: the row, and by cascade
    /// every value, restriction and staged secret filed under it. Irreversible.
    ///
    /// Answers the Credential Store entries those rows held, for the caller to
    /// release once the transaction has committed ([`Self::release_purged`]).
    ///
    /// # Errors
    /// [`DomainError::StepUpRequired`] / [`DomainError::Unauthorized`] /
    /// [`DomainError::Unavailable`] without step-up, judged before anything is
    /// read; [`DomainError::NotFound`]; [`DomainError::Conflict`] on a
    /// contributed declaration and on one that is not retired; the
    /// precondition errors on the tag.
    // @cpt-dod:cpt-cf-settings-service-dod-setting-declarations-purge:p2
    pub async fn purge<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        if_match: Option<&str>,
        actor: &WriteActor,
    ) -> Result<PurgedDeclaration, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-5
        // Irreversible, and it frees the key and the category: gated before the
        // declaration is even looked up.
        self.verify_step_up(actor, StepUpOperation::Purge).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-5
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-4
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-7
        // The row for update: a value write in flight holds it for share, and a
        // concurrent revive or retire takes it too, so each is serialized
        // against this purge and what is read below is what gets deleted.
        self.declarations.lock_for_update(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-7
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-8
        let current = self.find(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-8
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-9
        // Before the status: a module owns what it contributes, retired or not.
        Self::refuse_contributed(&current)?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-9
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-10
        if !current.status.is_retired() {
            return Err(conflict(
                conflict::NOT_RETIRED,
                format!(
                    "`{}` is {}; only a retired declaration is purged, and a purge never \
                     retires",
                    current.key,
                    current.status.as_str()
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-10
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-11
        precondition::evaluate(if_match, &etag_of(&current))?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-12
        // Collected under the lock and before the delete: the cascade removes
        // the rows, and with them the only record of what the store holds.
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        let held = self.values.secret_refs(conn, id).await?;
        let staged = self
            .pending
            .list_for_declaration(conn, id)
            .await?
            .into_iter()
            .map(|row| (row.tenant_id, row.secret_ref));
        for (tenant, secret_ref) in held.into_iter().chain(staged) {
            if seen.insert(secret_ref.clone()) {
                entries.push(HeldSecret { tenant, secret_ref });
            }
        }
        // An evolution copies the predecessor's references into the successor's
        // rows, and the store deletes by reference: what another declaration's
        // value or staged row still names is that declaration's, not ours.
        let references: Vec<String> = entries.iter().map(|e| e.secret_ref.clone()).collect();
        let mut others = self
            .values
            .references_held_by_others(conn, id, &references)
            .await?;
        others.extend(
            self.pending
                .references_held_by_others(conn, id, &references)
                .await?,
        );
        let (entries, kept): (Vec<HeldSecret>, Vec<HeldSecret>) = entries
            .into_iter()
            .partition(|entry| !others.contains(&entry.secret_ref));
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-12
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-13
        self.declarations
            .delete(conn, scope, id, current.updated_at)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-13
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-14
        // The definition is the pre-image; nothing is left to be a post-image.
        self.record(
            conn,
            &current,
            actor,
            AuditOperation::Remove,
            Some(snapshot(&current)),
            None,
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-14
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-15
        self.cache.invalidate_key(&current.key);
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-15
        Ok(PurgedDeclaration {
            id,
            key: current.key,
            secrets: entries,
            kept_secrets: kept,
        })
    }

    /// After the commit, re-check the Credential Store references the purge
    /// kept because another declaration still held them, and move every one
    /// nobody holds any more into the set to release.
    ///
    /// Needed when two retired declarations sharing an entry are purged at the
    /// same time: each keeps it for the other, so without this re-check nobody
    /// would release it. Runs on its own connection, after the purge's
    /// transaction. A failed check leaves the references kept and logs it; the
    /// purge is committed and stands.
    pub async fn reclaim_kept<C: DBRunner>(&self, conn: &C, purged: &mut PurgedDeclaration) {
        if purged.kept_secrets.is_empty() {
            return;
        }
        let references: Vec<String> = purged
            .kept_secrets
            .iter()
            .map(|e| e.secret_ref.clone())
            .collect();
        let mut others = match self
            .values
            .references_held_by_others(conn, purged.id, &references)
            .await
        {
            Ok(others) => others,
            Err(err) => {
                tracing::warn!(
                    key = %purged.key,
                    kept = purged.kept_secrets.len(),
                    err = %LogSafe(&err),
                    "declaration purge: the kept references could not be checked again; they stay kept"
                );
                return;
            }
        };
        match self
            .pending
            .references_held_by_others(conn, purged.id, &references)
            .await
        {
            Ok(more) => others.extend(more),
            Err(err) => {
                tracing::warn!(
                    key = %purged.key,
                    kept = purged.kept_secrets.len(),
                    err = %LogSafe(&err),
                    "declaration purge: the kept references could not be checked again; they stay kept"
                );
                return;
            }
        }
        let (still, freed): (Vec<HeldSecret>, Vec<HeldSecret>) =
            std::mem::take(&mut purged.kept_secrets)
                .into_iter()
                .partition(|entry| others.contains(&entry.secret_ref));
        purged.kept_secrets = still;
        purged.secrets.extend(freed);
    }

    /// Release the Credential Store entries of a committed purge, at most
    /// [`RELEASE_CONCURRENCY`] at a time, and answer how it went.
    ///
    /// The work runs in a spawned task, so a caller that is dropped
    /// mid-release does not cut it short. A refusal never undoes the purge:
    /// it is counted and logged without the secret, and the entry is left as
    /// an orphan an operator reconciles; nothing retries it. A task that
    /// panics or is torn down counts every entry it did not report as failed.
    pub async fn release_purged(
        &self,
        purged: PurgedDeclaration,
        request_id: &str,
    ) -> ReleaseSummary {
        let store = Arc::clone(&self.secrets);
        let metrics = Arc::clone(&self.metrics);
        let request_id = request_id.to_owned();
        let total = purged.secrets.len();
        let key_for_loss = purged.key.clone();
        // Shared with the task, so what it reported survives it.
        let released = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicUsize::new(0));
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-18
        let task = tokio::spawn({
            let metrics = Arc::clone(&metrics);
            let request_id = request_id.clone();
            let released = Arc::clone(&released);
            let failed = Arc::clone(&failed);
            async move {
                let PurgedDeclaration {
                    key,
                    secrets,
                    kept_secrets,
                    ..
                } = purged;
                tracing::info!(
                    key = %key,
                    entries = secrets.len(),
                    kept = kept_secrets.len(),
                    request_id = %request_id,
                    "declaration purge: releasing credential store entries"
                );
                // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-18
                // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-19
                let mut in_flight = futures_util::stream::iter(secrets)
                    .map(|entry| {
                        let store = Arc::clone(&store);
                        let key = key.as_str();
                        async move {
                            let outcome = store
                                .delete_secret(key, entry.tenant, &entry.secret_ref)
                                .await;
                            (entry.tenant, outcome)
                        }
                    })
                    .buffer_unordered(RELEASE_CONCURRENCY);
                // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-19
                while let Some((tenant, outcome)) = in_flight.next().await {
                    match outcome {
                        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-20
                        Ok(()) => {
                            metrics.purge_released();
                            released.fetch_add(1, Ordering::SeqCst);
                        }
                        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-20
                        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-21
                        Err(err) => {
                            // Neither the secret nor its reference: the
                            // reference names the entry an operator would
                            // reach for.
                            tracing::error!(
                                key = %key,
                                tenant = %tenant,
                                err = %LogSafe(&err),
                                "declaration purge: a credential store entry was not released"
                            );
                            metrics.purge_release_failed();
                            failed.fetch_add(1, Ordering::SeqCst);
                        } // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-21
                    }
                }
                // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-22
                let summary = ReleaseSummary {
                    released: released.load(Ordering::SeqCst),
                    failed: failed.load(Ordering::SeqCst),
                };
                tracing::info!(
                    key = %key,
                    released = summary.released,
                    failed = summary.failed,
                    request_id = %request_id,
                    "declaration purge: credential store release finished"
                );
                // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-purge:p2:inst-decl-purge-22
                summary
            }
        });
        match task.await {
            Ok(summary) => summary,
            Err(err) => {
                // The purge is committed; a task that panicked or was torn down
                // with the runtime does not turn that into a failed request.
                // What it did not report is counted as failed: no one knows
                // those entries were released.
                let done_ok = released.load(Ordering::SeqCst);
                let done_failed = failed.load(Ordering::SeqCst);
                let unreached = total.saturating_sub(done_ok + done_failed);
                for _ in 0..unreached {
                    metrics.purge_release_failed();
                }
                tracing::error!(
                    key = %key_for_loss,
                    request_id = %request_id,
                    entries = total,
                    err = %LogSafe(&err),
                    "declaration purge: the release task did not finish"
                );
                ReleaseSummary {
                    released: done_ok,
                    failed: done_failed + unreached,
                }
            }
        }
    }

    /// Evict a key from the effective cache, once the change is durable.
    pub fn evict(&self, key: &str) {
        self.cache.invalidate_key(key);
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Declaration, DomainError> {
        let visible = visibility::domain_visibility(scope);
        self.declarations
            .find(conn, scope, &visible, id)
            .await?
            .ok_or(DomainError::NotFound {
                resource: "declaration",
            })
    }

    async fn reload<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &str,
    ) -> Result<Declaration, DomainError> {
        self.declarations
            .find_by_key(conn, scope, key)
            .await?
            .ok_or_else(|| DomainError::Internal {
                diagnostic: format!("`{key}` vanished inside its own transaction"),
            })
    }

    // @cpt-dod:cpt-cf-settings-service-dod-module-contributions-immutable:p1
    fn refuse_contributed(declaration: &Declaration) -> Result<(), DomainError> {
        if declaration.source == "module_contributed" {
            return Err(conflict(
                conflict::CONTRIBUTED_IMMUTABLE,
                format!(
                    "`{}` is contributed by `{}` and changes only through its owning module",
                    declaration.key,
                    declaration.owner_module.as_deref().unwrap_or("a gear")
                ),
            ));
        }
        Ok(())
    }

    /// The step-up gate, as the value write path applies it: an interactive
    /// caller must present a fresh assertion; a service principal cannot and
    /// is refused outright. The outcome is counted in `settings_step_up_total`
    /// under `operation`.
    async fn verify_step_up(
        &self,
        actor: &WriteActor,
        operation: StepUpOperation,
    ) -> Result<(), DomainError> {
        if !actor.is_interactive() {
            self.metrics.step_up(operation, "service_principal");
            return Err(DomainError::Unauthorized {
                resource: settings_service_sdk::gts::DECLARATION_SCHEMA,
            });
        }
        let subject = actor.step_up_subject();
        match self
            .step_up
            .verify(
                actor
                    .step_up_token
                    .as_ref()
                    .map(ExposeSecret::expose_secret),
                &subject,
            )
            .await
        {
            Ok(()) => {
                self.metrics.step_up(operation, "verified");
                Ok(())
            }
            Err(refusal) => {
                self.metrics.step_up(operation, refusal.code());
                match refusal {
                    // An outage of the platform's AuthN is not a verdict on the
                    // token: the client retries rather than re-authenticates.
                    StepUpRefusal::Unavailable(detail) => Err(DomainError::Unavailable {
                        detail: format!("step-up could not be verified: {detail}"),
                    }),
                    refusal => {
                        let requirement = self.step_up.requirement();
                        Err(DomainError::StepUpRequired {
                            reason: refusal.code(),
                            max_age_seconds: requirement.max_age.as_secs(),
                            acr_values: requirement.acr_values.clone(),
                        })
                    }
                }
            }
        }
    }

    async fn record<C: DBRunner>(
        &self,
        conn: &C,
        declaration: &Declaration,
        actor: &WriteActor,
        operation: AuditOperation,
        pre: Option<Value>,
        post: Option<Value>,
    ) -> Result<(), DomainError> {
        // A declaration is a platform-wide definition and sits at no scope, so
        // its record carries none. It used to borrow the root tenant, which
        // meant asking the Tenant Resolver for an id no reader of this record
        // ever matches on.
        let mut record = AuditRecord::new(
            declaration.key.as_str(),
            None,
            actor.subject(),
            ActorTenant::of(actor.tenant()),
            operation,
            actor.request_id.clone(),
        );
        if let Some(pre) = pre {
            record = record.with_pre_image(AuditValue::definition(pre));
        }
        if let Some(post) = post {
            record = record.with_post_image(AuditValue::definition(post));
        }
        self.sink
            .append(conn, &AccessScope::allow_all(), record)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "admin_tests.rs"]
mod admin_tests;

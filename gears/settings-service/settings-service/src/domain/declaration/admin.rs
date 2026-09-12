// Created: 2026-09-08 by Constructor Tech
//! Administrative authoring of declarations: create, revive, edit metadata,
//! retire.
//!
//! Everything here is immediate and never touches the value write path. What
//! it guards is that no edit changes a live setting's resolution without a
//! gate: behaviour-affecting fields are refused as immutable, loosening a gate
//! or a classification and every lifecycle change demand credential step-up,
//! and a gear's contributed declarations are not admin-editable at all.

use std::sync::Arc;

use serde_json::{Map, Value};
use settings_service_sdk::{SettingKey, SettingKeyError};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::api::precondition::{self, ETag};
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::category::repo::CategoryRepository;
use crate::domain::category::visibility;
use crate::domain::contribution::SettingTypeRegistrar;
use crate::domain::declaration::{
    Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository,
};
use crate::domain::error::DomainError;
use crate::domain::resolution::EffectiveCache;
use crate::domain::stepup::StepUpVerifier;
use crate::domain::validation::{TraitSet, TypeValidator};
use crate::domain::value::ValueRepository;
use crate::domain::writes::WriteActor;
use crate::field;

/// Conflict reasons this path raises; the text a `409` carries starts with one.
pub mod conflict {
    /// A gear's declaration changes only through its owning module.
    pub const CONTRIBUTED_IMMUTABLE: &str = "contributed_declaration_immutable";
    /// An active declaration already holds this key.
    pub const KEY_CONFLICT: &str = "declaration_key_conflict";
    /// An active declaration in the category already holds this leaf name.
    pub const LEAF_NAME_TAKEN: &str = "leaf_name_taken";
    /// A revive names a different value type; that is a new major, not a revive.
    pub const VALUE_TYPE_CHANGED: &str = "value_type_changed";
    /// A revive changes the scope class; where a value may exist is not revived.
    pub const SCOPE_CLASS_CHANGED: &str = "scope_class_changed";
    /// A revive changes the Schema Default; the declared floor rides a new major.
    pub const DEFAULT_CHANGED: &str = "default_changed";
}

/// What an administrator supplies to declare a setting.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateDeclaration {
    pub value_type_id: String,
    pub vendor: String,
    pub name: String,
    pub category_id: Uuid,
    pub default_value: Value,
    pub scope_class: String,
    pub description: Option<String>,
    pub mode: Option<String>,
    pub requires_step_up: Option<bool>,
    pub anonymous_exposable: Option<bool>,
    pub domain_affinity: Option<String>,
    pub licence_feature: Option<String>,
    pub data_classification: Option<String>,
}

/// The outcome of a create: a new row, or a retired one revived.
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub declaration: Declaration,
    pub reactivated: bool,
}

/// What the value type's traits and the author's wish resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedClassification {
    pub has_secret_trait: bool,
    pub data_classification: &'static str,
}

/// The class a field of an update request falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldClass {
    /// Descriptive, or tightening a gate: applied at once.
    Immediate,
    /// Behaviour-affecting: expressible only as a replacement declaration.
    Immutable,
    /// Loosening a gate or a classification: needs credential step-up.
    StepUp,
}

fn validation(field: &str, code: &'static str, message: impl Into<String>) -> DomainError {
    DomainError::Validation {
        field: field.to_owned(),
        code,
        message: message.into(),
    }
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

/// An empty value of the type: the only default a secret setting may carry.
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
            && other != "secret"
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
        "secret"
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
            Some("pii") => "pii",
            Some("public") | None => "public",
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
                match (current.data_classification.as_str(), requested) {
                    // Derived from the trait: never author-changed.
                    ("secret", _) => FieldClass::Immutable,
                    // @cpt-begin:cpt-cf-settings-service-algo-setting-declarations-mutation-class:p1:inst-decl-mutcls-5
                    // Loosening un-masks content previously withheld.
                    ("pii", "public") => FieldClass::StepUp,
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
        "status": declaration.status,
        "description": declaration.description,
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
pub struct DeclarationAdmin<R, Cat, Val, S> {
    declarations: R,
    categories: Cat,
    values: Val,
    validator: Arc<dyn TypeValidator>,
    registrar: Arc<dyn SettingTypeRegistrar>,
    step_up: Arc<dyn StepUpVerifier>,
    sink: S,
    cache: Arc<EffectiveCache>,
}

impl<R, Cat, Val, S> DeclarationAdmin<R, Cat, Val, S>
where
    R: DeclarationRepository,
    Cat: CategoryRepository,
    Val: ValueRepository,
    S: AuditSink,
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
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-12
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-13
        if derived.has_secret_trait {
            if !is_empty_placeholder(&request.default_value) {
                return Err(validation(
                    "default_value",
                    field::SECRET_DEFAULT_NOT_EMPTY,
                    "a secret setting has no secret default: the placeholder is an empty value \
                     of the type, and the credential is set as a value at a scope",
                ));
            }
        } else {
            self.validator
                .validate_value(&request.value_type_id, &request.default_value)
                .await?
                .into_result()?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-13
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-12
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-11
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-14
        let anonymous_exposable = request.anonymous_exposable.unwrap_or(false);
        if anonymous_exposable && derived.data_classification != "public" {
            return Err(validation(
                "anonymous_exposable",
                field::EXPOSABLE_NOT_SENSITIVE,
                format!(
                    "a `{}` setting cannot be exposed on the anonymous surface",
                    derived.data_classification
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
            description: request.description.clone(),
            domain_affinity: request.domain_affinity.clone(),
            licence_feature: request.licence_feature.clone(),
            data_classification: derived.data_classification.to_owned(),
            requires_step_up: request.requires_step_up.unwrap_or(true),
            anonymous_exposable,
        };
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-15

        match existing {
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-6
            Some(active) if active.status == "active" => Err(conflict(
                conflict::KEY_CONFLICT,
                format!(
                    "`{}` is an active declaration; metadata changes use PATCH",
                    active.key
                ),
            )),
            // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-6
            Some(retired) => {
                self.revive(conn, scope, retired, &request, derived, metadata, actor)
                    .await
            }
            // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-5
            None => {
                self.insert_new(conn, scope, &key, &request, derived, metadata, actor)
                    .await
            } // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-5
        }
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
            data_classification: derived.data_classification.to_owned(),
            source: "admin_authored".to_owned(),
            owner_module: None,
            licence_feature: metadata.licence_feature.clone(),
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
        })
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-create:p1:inst-decl-create-21
    }

    #[allow(clippy::too_many_arguments)]
    async fn revive<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
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
        self.verify_step_up(actor).await?;
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-8
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-7
        // What a revive may not change, because it would move values rather
        // than re-interpret them: the value type (and with it the secret
        // boundary), the scope class, and the declared floor.
        if retired.value_type_id != request.value_type_id {
            return Err(conflict(
                conflict::VALUE_TYPE_CHANGED,
                format!(
                    "`{}` is declared with `{}`; a revive keeps the value type",
                    retired.key, retired.value_type_id
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
        if retired.default_value != request.default_value {
            return Err(conflict(
                conflict::DEFAULT_CHANGED,
                format!(
                    "`{}` keeps its Schema Default on a revive; a new floor is a new major",
                    retired.key
                ),
            ));
        }
        let _ = derived;
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-9
        // @cpt-begin:cpt-cf-settings-service-state-setting-declarations-lifecycle:p1:inst-decl-state-3
        let classification_changed = retired.data_classification != metadata.data_classification;
        let new_classification = metadata.data_classification.clone();
        self.declarations
            .update_metadata(conn, scope, retired.id, metadata)
            .await?;
        if let Err(err) = self
            .declarations
            .set_status(conn, scope, retired.id, "active")
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
        })
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-reactivate:p1:inst-decl-react-12
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
            self.verify_step_up(actor).await?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-11
        // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-10
        let mut metadata = DeclarationMetadata {
            mode: current.mode.clone(),
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
                "domain_affinity" => metadata.domain_affinity = optional_string(value),
                "licence_feature" => metadata.licence_feature = optional_string(value),
                "mode" => metadata.mode = value.as_str().unwrap_or("standard").to_owned(),
                "requires_step_up" => metadata.requires_step_up = value.as_bool().unwrap_or(true),
                "anonymous_exposable" => {
                    metadata.anonymous_exposable = value.as_bool().unwrap_or(false);
                }
                "data_classification" => {
                    metadata.data_classification = value.as_str().unwrap_or("public").to_owned();
                }
                _ => {}
            }
        }
        // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-update:p1:inst-decl-update-12
        if metadata.anonymous_exposable && metadata.data_classification != "public" {
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
        self.declarations
            .update_metadata(conn, scope, id, metadata)
            .await?;
        if classification_changed {
            // The denormalized class on every stored value follows, so masking
            // reads one column and never disagrees with the declaration.
            self.values
                .resync_classification(conn, scope, id, &new_classification)
                .await?;
        }
        let updated = self.reload(conn, scope, &current.key).await?;
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
        self.verify_step_up(actor).await?;
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
        if current.status == "retired" {
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
            .set_status(conn, scope, id, "retired")
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
    /// is refused outright.
    async fn verify_step_up(&self, actor: &WriteActor) -> Result<(), DomainError> {
        if !actor.is_interactive() {
            return Err(DomainError::Unauthorized {
                resource: settings_service_sdk::gts::DECLARATION_SCHEMA,
            });
        }
        let subject = actor.step_up_subject();
        match self
            .step_up
            .verify(actor.step_up_token.as_deref(), &subject)
            .await
        {
            Ok(()) => Ok(()),
            Err(refusal) => {
                let requirement = self.step_up.requirement();
                Err(DomainError::StepUpRequired {
                    reason: refusal.code(),
                    max_age_seconds: requirement.max_age.as_secs(),
                    acr_values: requirement.acr_values.clone(),
                })
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
            operation,
            actor.request_id.clone(),
        );
        if let Some(pre) = pre {
            record = record.with_pre_image(AuditValue::record(pre, false));
        }
        if let Some(post) = post {
            record = record.with_post_image(AuditValue::record(post, false));
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

// Created: 2026-09-06 by Constructor Tech
//! The reconcile itself, one declaration at a time.

use std::sync::Arc;

use serde_json::Value;
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::SETTING_TYPE_BASE;
use settings_service_sdk::models::{
    ContributedClassification, ContributedDeclaration, ScopeClass, SettingMode,
};
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{SettingTypeRegistrar, reason};
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::category::{CategoryDraft, CategoryKey, CategoryRepository};
use crate::domain::declaration::{
    Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository,
};
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::validation::TypeValidator;
use crate::domain::value::ValueRepository;

/// What one declaration's reconcile did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Inserted as new.
    Registered,
    /// Metadata updated in place.
    Updated,
    /// Already as contributed; nothing written.
    Unchanged,
    /// Revived from retired.
    Reactivated,
}

/// Why one item did not reconcile.
#[derive(Debug)]
pub enum ItemError {
    /// The declaration was refused; the rest of the set is unaffected.
    Refused {
        /// A stable reason from [`reason`].
        code: &'static str,
        /// Human-readable detail.
        message: String,
    },
    /// Infrastructure failed; the transaction rolls back.
    Failed(DomainError),
}

impl From<DomainError> for ItemError {
    fn from(err: DomainError) -> Self {
        Self::Failed(err)
    }
}

fn refused(code: &'static str, message: impl Into<String>) -> ItemError {
    ItemError::Refused {
        code,
        message: message.into(),
    }
}

/// The reconciler.
pub struct ContributionService<D, Cat, V, S> {
    declarations: D,
    categories: Cat,
    values: V,
    validator: Arc<dyn TypeValidator>,
    registrar: Arc<dyn SettingTypeRegistrar>,
    sink: S,
    scope: Arc<dyn PlatformScope>,
}

/// What the key admitted: the parts of the contributed key the reconcile
/// matches and files by.
struct Admitted<'k> {
    category_slug: &'k str,
    leaf: &'k str,
    major: u32,
    stripped_path: &'k str,
}

/// What the reconcile derived from a contributed declaration and its type.
// The flags mirror the declaration's own: each is a separate fact an
// administrator reads back, and a state enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
struct Derived {
    has_secret_trait: bool,
    data_classification: &'static str,
    mode: &'static str,
    requires_step_up: bool,
    anonymous_exposable: bool,
}

impl Derived {
    fn metadata(&self, contributed: &ContributedDeclaration) -> DeclarationMetadata {
        DeclarationMetadata {
            mode: self.mode.to_owned(),
            description: contributed.description.clone(),
            domain_affinity: contributed.domain_affinity.clone(),
            licence_feature: contributed.licence_feature.clone(),
            data_classification: self.data_classification.to_owned(),
            requires_step_up: self.requires_step_up,
            anonymous_exposable: self.anonymous_exposable,
        }
    }
}

fn scope_class_name(class: ScopeClass) -> &'static str {
    match class {
        ScopeClass::Global => "global",
        ScopeClass::Cascading => "cascading",
        ScopeClass::Local => "local",
    }
}

fn is_empty_placeholder(value: &Value) -> bool {
    matches!(value, Value::Null) || value.as_str().is_some_and(str::is_empty)
}

fn major_of(declaration: &Declaration) -> Option<u32> {
    SettingKey::parse(&declaration.key).ok().map(|k| k.major())
}

fn metadata_differs(existing: &Declaration, metadata: &DeclarationMetadata) -> bool {
    existing.mode != metadata.mode
        || existing.description != metadata.description
        || existing.domain_affinity != metadata.domain_affinity
        || existing.licence_feature != metadata.licence_feature
        || existing.data_classification != metadata.data_classification
        || existing.requires_step_up != metadata.requires_step_up
        || existing.anonymous_exposable != metadata.anonymous_exposable
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
        "owner_module": declaration.owner_module,
    })
}

// @cpt-dod:cpt-cf-settings-service-dod-module-contributions-key:p1
/// Contributed key admission: what the grammar cannot know about the key.
///
/// The SDK hands over a parsed `SettingKey`, so the base and the single
/// derived type were checked on construction; what is admitted here is that the
/// derived half names the category the setting files under.
fn admit(key: &SettingKey) -> Result<Admitted<'_>, ItemError> {
    // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-1
    // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-2
    let category_slug = key.category_slug();
    if category_slug.is_empty() {
        return Err(refused(
            reason::KEY_NOT_NAMESPACED,
            format!("`{key}` carries no category segment"),
        ));
    }
    // @cpt-end:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-2
    // @cpt-end:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-1
    // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-3
    // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-4
    Ok(Admitted {
        category_slug,
        leaf: key.leaf_slug(),
        major: key.major(),
        stripped_path: key.version_stripped_path(),
    })
    // @cpt-end:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-4
    // @cpt-end:cpt-cf-settings-service-algo-module-contributions-key:p1:inst-mc-key-3
}

impl<D, Cat, V, S> ContributionService<D, Cat, V, S>
where
    D: DeclarationRepository,
    Cat: CategoryRepository,
    V: ValueRepository,
    S: AuditSink,
{
    /// Build the reconciler over its repositories and ports.
    pub fn new(
        declarations: D,
        categories: Cat,
        values: V,
        validator: Arc<dyn TypeValidator>,
        registrar: Arc<dyn SettingTypeRegistrar>,
        sink: S,
        scope: Arc<dyn PlatformScope>,
    ) -> Self {
        Self {
            declarations,
            categories,
            values,
            validator,
            registrar,
            sink,
            scope,
        }
    }

    /// Reconcile one contributed declaration, matched by its version-stripped path.
    ///
    /// Runs inside the caller's transaction: a refusal writes nothing, and an
    /// infrastructure failure rolls back whatever was written.
    ///
    /// # Errors
    /// [`ItemError::Refused`] with a stable reason, or [`ItemError::Failed`]
    /// when a repository, the validator or the registry failed.
    pub async fn reconcile_one<C: DBRunner>(
        &self,
        conn: &C,
        owner_module: &str,
        request_id: &str,
        contributed: &ContributedDeclaration,
    ) -> Result<Outcome, ItemError> {
        let scope = AccessScope::allow_all();
        let key = &contributed.key;
        let admitted = admit(key)?;

        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-1
        let derived = self.derive(contributed).await?;
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-1
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-2
        self.check_default(contributed).await?;
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-2

        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-4
        let on_path = self
            .declarations_on_path(conn, &scope, admitted.stripped_path)
            .await?;
        let same_major = on_path
            .iter()
            .find(|d| major_of(d) == Some(admitted.major))
            .cloned();
        let highest = on_path.iter().filter_map(major_of).max();
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-4

        match (same_major, highest) {
            (None, None) => {
                self.register_new(
                    conn,
                    &scope,
                    owner_module,
                    request_id,
                    contributed,
                    &admitted,
                    &derived,
                )
                .await
            }
            (Some(existing), _) => {
                self.reconcile_existing(
                    conn,
                    &scope,
                    owner_module,
                    request_id,
                    contributed,
                    &derived,
                    existing,
                )
                .await
            }
            (None, Some(highest)) if admitted.major > highest => {
                // The upgrade migration — successor inserted, values copied and
                // re-validated, predecessor retired — needs the value listing
                // the read path brings; until then a higher major is refused
                // rather than half-applied.
                Err(refused(
                    reason::UPGRADE_UNSUPPORTED,
                    format!(
                        "`{key}` is a higher major than the stored v{highest}; the upgrade \
                         migration is not available yet"
                    ),
                ))
            }
            // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-8
            (None, Some(highest)) => Err(refused(
                reason::MAJOR_REGRESSION,
                format!(
                    "`{key}` is a lower major than the stored v{highest}; a gear does not roll a \
                     setting back by re-registering an older major"
                ),
            )),
            // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-8
        }
    }

    /// Retire one declaration the module owns.
    ///
    /// # Errors
    /// [`ItemError::Refused`] when no declaration exists at the key or another
    /// module owns it; [`ItemError::Failed`] on infrastructure failure. An
    /// already retired declaration is `Ok(false)`.
    pub async fn retire_one<C: DBRunner>(
        &self,
        conn: &C,
        owner_module: &str,
        request_id: &str,
        key: &SettingKey,
    ) -> Result<bool, ItemError> {
        let scope = AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-2
        let existing = self
            .declarations
            .find_by_key(conn, &scope, key.as_str())
            .await?
            .ok_or_else(|| refused(reason::NOT_FOUND, format!("no declaration at `{key}`")))?;
        if existing.owner_module.as_deref() != Some(owner_module) {
            return Err(refused(
                reason::NOT_OWNER,
                format!("`{key}` is owned by another module"),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-2
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-3
        if existing.status == "retired" {
            return Ok(false);
        }
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-3
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-4
        // Retire, never delete: values are retained and excluded from
        // resolution, and the type stays registered so a re-registration is a
        // lookup rather than a re-mint.
        self.declarations
            .set_status(conn, &scope, existing.id, "retired")
            .await?;
        let retired = self.reload(conn, &scope, key).await?;
        self.record(
            conn,
            key,
            owner_module,
            request_id,
            AuditOperation::Remove,
            Some(snapshot(&existing)),
            Some(snapshot(&retired)),
        )
        .await?;
        Ok(true)
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-retire:p1:inst-mc-ret-4
    }

    /// Classification, secret handling and the flags, derived from the value
    /// type's traits and what the caller supplied.
    async fn derive(&self, contributed: &ContributedDeclaration) -> Result<Derived, ItemError> {
        let traits = match self
            .validator
            .resolve_traits(&contributed.value_type_id)
            .await
        {
            Ok(traits) => traits,
            Err(DomainError::Validation { message, .. }) => {
                return Err(refused(reason::VALUE_TYPE_UNKNOWN, message));
            }
            Err(other) => return Err(other.into()),
        };
        let data_classification = if traits.secret {
            if matches!(
                contributed.data_classification,
                Some(ContributedClassification::Pii)
            ) {
                return Err(refused(
                    reason::CLASSIFICATION_CONFLICT,
                    "the value type carries the secret trait; `secret` is derived and \
                     cannot be declared `pii`",
                ));
            }
            // @cpt-dod:cpt-cf-settings-service-dod-secret-values-placeholder:p1
            if !is_empty_placeholder(&contributed.default_value) {
                return Err(refused(
                    reason::SECRET_DEFAULT_NOT_EMPTY,
                    "a secret setting has no secret default: the placeholder is an empty \
                     value of the type, and the credential is set as a value at a scope",
                ));
            }
            "secret"
        } else {
            match contributed.data_classification {
                Some(ContributedClassification::Pii) => "pii",
                Some(ContributedClassification::Public) | None => "public",
            }
        };
        let anonymous_exposable = contributed.anonymous_exposable.unwrap_or(false);
        if anonymous_exposable && data_classification != "public" {
            return Err(refused(
                reason::EXPOSABLE_NOT_SENSITIVE,
                format!(
                    "a `{data_classification}` setting cannot be exposed on the anonymous surface"
                ),
            ));
        }
        Ok(Derived {
            has_secret_trait: traits.secret,
            data_classification,
            mode: match contributed.mode {
                Some(SettingMode::Advanced) => "advanced",
                Some(SettingMode::Standard) | None => "standard",
            },
            requires_step_up: contributed.requires_step_up.unwrap_or(true),
            anonymous_exposable,
        })
    }

    /// The Schema Default must be a valid value of the type: a declaration
    /// whose default fails would make every unset scope unreadable.
    async fn check_default(&self, contributed: &ContributedDeclaration) -> Result<(), ItemError> {
        let result = self
            .validator
            .validate_value(&contributed.value_type_id, &contributed.default_value)
            .await?;
        match result.violations.first() {
            None => Ok(()),
            Some(first) if first.code == crate::field::VALUE_TYPE_UNKNOWN => Err(refused(
                reason::VALUE_TYPE_UNKNOWN,
                format!("{} — {}", first.field, first.message),
            )),
            Some(first) => Err(refused(
                reason::DEFAULT_INVALID,
                format!("{} — {}", first.field, first.message),
            )),
        }
    }

    /// Every major on the stripped path, whatever its status.
    ///
    /// The LIKE prefix is re-checked exactly because `_` is a wildcard there.
    async fn declarations_on_path<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        stripped_path: &str,
    ) -> Result<Vec<Declaration>, ItemError> {
        let prefix = format!("{SETTING_TYPE_BASE}{stripped_path}.v");
        Ok(self
            .declarations
            .find_by_key_prefix(conn, scope, &prefix)
            .await?
            .into_iter()
            .filter(|d| {
                SettingKey::parse(&d.key).is_ok_and(|k| k.version_stripped_path() == stripped_path)
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn register_new<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        owner_module: &str,
        request_id: &str,
        contributed: &ContributedDeclaration,
        admitted: &Admitted<'_>,
        derived: &Derived,
    ) -> Result<Outcome, ItemError> {
        let key = &contributed.key;
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-3
        let category_id = self
            .category_for(conn, scope, admitted.category_slug)
            .await?;
        self.registrar
            .register_setting_type(key, &contributed.value_type_id)
            .await?;
        let draft = DeclarationDraft {
            key: key.to_string(),
            leaf_slug: admitted.leaf.to_owned(),
            value_type_id: contributed.value_type_id.clone(),
            category_id,
            default_value: contributed.default_value.clone(),
            scope_class: scope_class_name(contributed.scope_class).to_owned(),
            mode: derived.mode.to_owned(),
            requires_step_up: derived.requires_step_up,
            anonymous_exposable: derived.anonymous_exposable,
            domain_affinity: contributed.domain_affinity.clone(),
            has_secret_trait: derived.has_secret_trait,
            data_classification: derived.data_classification.to_owned(),
            source: "module_contributed".to_owned(),
            owner_module: Some(owner_module.to_owned()),
            licence_feature: contributed.licence_feature.clone(),
            description: contributed.description.clone(),
            created_by: owner_module.to_owned(),
        };
        let inserted = self.declarations.insert(conn, scope, draft).await?;
        self.record(
            conn,
            key,
            owner_module,
            request_id,
            AuditOperation::Create,
            None,
            Some(snapshot(&inserted)),
        )
        .await?;
        Ok(Outcome::Registered)
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-3
    }

    #[allow(clippy::too_many_arguments)]
    async fn reconcile_existing<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        owner_module: &str,
        request_id: &str,
        contributed: &ContributedDeclaration,
        derived: &Derived,
        existing: Declaration,
    ) -> Result<Outcome, ItemError> {
        let key = &contributed.key;
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-4
        if existing.value_type_id != contributed.value_type_id {
            return Err(refused(
                reason::VALUE_TYPE_CHANGED,
                format!(
                    "`{key}` is declared with `{}`; a value type change is a new major, not an \
                     edit — this reconcile runs with nobody watching",
                    existing.value_type_id
                ),
            ));
        }
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-4
        // The other behavior-affecting fields ride the same rule: a changed
        // Schema Default or scope class at the same major would alter a live
        // setting's resolution through an unattended path.
        if existing.default_value != contributed.default_value
            || existing.scope_class != scope_class_name(contributed.scope_class)
        {
            return Err(refused(
                reason::BEHAVIOR_AFFECTING_CHANGE,
                format!(
                    "`{key}` changes its Schema Default or scope class at the same major; such a \
                     change is carried by a new major"
                ),
            ));
        }
        if existing.owner_module.as_deref() != Some(owner_module) {
            return Err(refused(
                reason::NOT_OWNER,
                format!("`{key}` is owned by another module"),
            ));
        }
        let metadata = derived.metadata(contributed);
        let changed = metadata_differs(&existing, &metadata);
        let classification_changed = existing.data_classification != metadata.data_classification;

        if existing.status == "retired" {
            // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-6
            // Revive in place: the row keeps its id and its values stay where
            // they are. Re-validation of retained values against the type
            // follows the value listing, which arrives with the read path.
            self.declarations
                .update_metadata(conn, scope, existing.id, metadata)
                .await?;
            self.declarations
                .set_status(conn, scope, existing.id, "active")
                .await?;
            if classification_changed {
                self.values
                    .resync_classification(conn, scope, existing.id, derived.data_classification)
                    .await?;
            }
            let revived = self.reload(conn, scope, key).await?;
            self.record(
                conn,
                key,
                owner_module,
                request_id,
                AuditOperation::Change,
                Some(snapshot(&existing)),
                Some(snapshot(&revived)),
            )
            .await?;
            return Ok(Outcome::Reactivated);
            // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-6
        }
        if !changed {
            // Idempotent: a repeated boot converges and changes nothing.
            return Ok(Outcome::Unchanged);
        }
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-5
        self.declarations
            .update_metadata(conn, scope, existing.id, metadata)
            .await?;
        if classification_changed {
            // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-3
            // In the same transaction as the declaration change, so no window
            // exists in which the two disagree.
            self.values
                .resync_classification(conn, scope, existing.id, derived.data_classification)
                .await?;
            // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-classification-sync:p1:inst-tvv-sync-3
        }
        let updated = self.reload(conn, scope, key).await?;
        self.record(
            conn,
            key,
            owner_module,
            request_id,
            AuditOperation::Change,
            Some(snapshot(&existing)),
            Some(snapshot(&updated)),
        )
        .await?;
        Ok(Outcome::Updated)
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-reconcile:p1:inst-mc-rec-5
    }

    /// The category a key's third segment names, created on first use.
    async fn category_for<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        slug: &str,
    ) -> Result<Uuid, ItemError> {
        // @cpt-begin:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-3
        let category_key = CategoryKey::parse(slug).map_err(|e| {
            refused(
                reason::KEY_NOT_NAMESPACED,
                format!("the category segment `{slug}` is not a valid category key: {e}"),
            )
        })?;
        if let Some(existing) = self
            .categories
            .find_by_key(conn, scope, &category_key)
            .await?
        {
            return Ok(existing.id);
        }
        // Auto-vivified with the slug as both key and display name; an
        // administrator may rename it later, the key stays.
        let created = self
            .categories
            .insert(
                conn,
                scope,
                CategoryDraft {
                    key: category_key,
                    name: slug.to_owned(),
                    description: None,
                    domain_affinity: None,
                    sort_order: 0,
                    icon: None,
                },
            )
            .await?;
        Ok(created.id)
        // @cpt-end:cpt-cf-settings-service-flow-module-contributions-register:p1:inst-mc-reg-3
    }

    async fn reload<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &SettingKey,
    ) -> Result<Declaration, ItemError> {
        self.declarations
            .find_by_key(conn, scope, key.as_str())
            .await?
            .ok_or_else(|| {
                ItemError::Failed(DomainError::Internal {
                    diagnostic: format!("`{key}` vanished inside its own transaction"),
                })
            })
    }

    /// One audit record per changed row, at platform scope, with the module as
    /// the actor.
    #[allow(clippy::too_many_arguments)]
    async fn record<C: DBRunner>(
        &self,
        conn: &C,
        key: &SettingKey,
        owner_module: &str,
        request_id: &str,
        operation: AuditOperation,
        pre: Option<Value>,
        post: Option<Value>,
    ) -> Result<(), DomainError> {
        let tenant = self.scope.root_tenant().await?;
        let mut rec =
            AuditRecord::new(key.as_str(), tenant, owner_module, operation, request_id).by_module();
        if let Some(pre) = pre {
            rec = rec.with_pre_image(AuditValue::record(pre, false));
        }
        if let Some(post) = post {
            rec = rec.with_post_image(AuditValue::record(post, false));
        }
        // Declarations have no tenant of their own; the record is written on
        // the same unscoped path as the row it audits, inside its transaction.
        self.sink.append(conn, &AccessScope::allow_all(), rec).await
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

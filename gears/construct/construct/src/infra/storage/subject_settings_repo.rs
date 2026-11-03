use async_trait::async_trait;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{
    DBRunner, ScopableEntity, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue};

use crate::domain::error::DomainError;
use crate::domain::subject_settings::{
    ScopeRefusal, SettingsLookup, SettingsWrite, SubjectKey, SubjectSettings,
    SubjectSettingsRepository, UncheckablePredicate,
};

use super::scope_error::map_scope_error;
use super::subject_settings_entity::{self as entity, Entity as SubjectSettingsEntity};

/// `SeaORM`-backed [`SubjectSettingsRepository`]; every query goes through the
/// secure ORM and is constrained by the caller's `AccessScope`. Before a read,
/// whether that scope covers the key is decided from the key with the entity's
/// own property-to-column mapping, so a row the scope leaves out is reported as
/// such and never as absent.
#[derive(Debug, Default)]
pub struct SeaOrmSubjectSettingsRepository;

impl SeaOrmSubjectSettingsRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// The value the row of `key` holds in `column`, for the columns a key
/// determines. Every column is named, so a new one has to be placed here.
fn key_value(column: entity::Column, key: SubjectKey) -> Option<uuid::Uuid> {
    match column {
        entity::Column::TenantId => Some(key.tenant_id),
        entity::Column::SubjectId => Some(key.subject_id),
        entity::Column::PersonalizationEnabled | entity::Column::ErasureInProgress => None,
    }
}

/// One filter against the key. The property is mapped to its column by the
/// entity itself (`ScopableEntity::resolve_property`), so this cannot disagree
/// with the column the secure ORM filters on.
fn filter_coverage(filter: &ScopeFilter, key: SubjectKey) -> Result<(), ScopeRefusal> {
    let Some(expected) =
        SubjectSettingsEntity::resolve_property(filter.property()).and_then(|c| key_value(c, key))
    else {
        return Err(ScopeRefusal::NotCovered);
    };
    match filter {
        ScopeFilter::Eq(_) | ScopeFilter::In(_) => {
            // Only a UUID value matches. The secure ORM binds a string value as
            // text and compares values by kind, so a UUID held as a string
            // covers nothing there either.
            if filter
                .values()
                .iter()
                .any(|value| matches!(value, ScopeValue::Uuid(u) if *u == expected))
            {
                Ok(())
            } else {
                Err(ScopeRefusal::NotCovered)
            }
        }
        ScopeFilter::InGroup(_) => Err(ScopeRefusal::Undecidable(UncheckablePredicate::Group)),
        ScopeFilter::InGroupSubtree(_) => Err(ScopeRefusal::Undecidable(
            UncheckablePredicate::GroupSubtree,
        )),
        ScopeFilter::InTenantSubtree(_) => Err(ScopeRefusal::Undecidable(
            UncheckablePredicate::TenantSubtree,
        )),
        // `ScopeFilter` is `#[non_exhaustive]`: a kind added later fails closed.
        _ => Err(ScopeRefusal::Undecidable(UncheckablePredicate::Unknown)),
    }
}

/// The filters of one constraint are AND-ed: any miss and it does not cover,
/// any undecidable filter and it cannot be decided. A constraint without
/// filters cannot be built today (`ScopeConstraint` refuses one); if one ever
/// arrives it covers nothing, as the secure ORM refuses it too.
fn constraint_coverage(constraint: &ScopeConstraint, key: SubjectKey) -> Result<(), ScopeRefusal> {
    if constraint.is_empty() {
        return Err(ScopeRefusal::NotCovered);
    }
    let mut undecidable = None;
    for filter in constraint.filters() {
        match filter_coverage(filter, key) {
            Ok(()) => {}
            Err(ScopeRefusal::NotCovered) => return Err(ScopeRefusal::NotCovered),
            Err(ScopeRefusal::Undecidable(kind)) => undecidable = undecidable.or(Some(kind)),
        }
    }
    undecidable.map_or(Ok(()), |kind| Err(ScopeRefusal::Undecidable(kind)))
}

/// Whether `scope` covers the row of `key`. Constraints are OR-ed: one
/// covering constraint is enough. Otherwise an undecidable constraint makes
/// the scope undecidable, and the read fails closed either way.
pub(crate) fn coverage(scope: &AccessScope, key: SubjectKey) -> Result<(), ScopeRefusal> {
    // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-cover
    if scope.is_deny_all() {
        return Err(ScopeRefusal::NotCovered);
    }
    if scope.is_unconstrained() {
        return Ok(());
    }
    let mut undecidable = None;
    for constraint in scope.constraints() {
        match constraint_coverage(constraint, key) {
            Ok(()) => return Ok(()),
            Err(ScopeRefusal::NotCovered) => {}
            Err(ScopeRefusal::Undecidable(kind)) => undecidable = undecidable.or(Some(kind)),
        }
    }
    Err(undecidable.map_or(ScopeRefusal::NotCovered, ScopeRefusal::Undecidable))
    // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-cover
}

#[async_trait]
impl SubjectSettingsRepository for SeaOrmSubjectSettingsRepository {
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SettingsLookup, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SettingsLookup::OutOfScope(refusal));
        }

        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select
        let row = SubjectSettingsEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(entity::Column::TenantId.eq(key.tenant_id))
                    .add(entity::Column::SubjectId.eq(key.subject_id)),
            )
            .one(conn)
            .await
            .map_err(map_scope_error)?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select

        Ok(row.map_or(SettingsLookup::Absent, |row| {
            SettingsLookup::Stored(SubjectSettings::from(row))
        }))
    }

    async fn set_personalization<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        personalization_enabled: bool,
    ) -> Result<SettingsWrite, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SettingsWrite::OutOfScope(refusal));
        }

        // The erasure flag is left out of the insert, so a new row takes the
        // column default (no erasure under way), and out of the update, so an
        // existing row keeps it.
        let row = entity::ActiveModel {
            tenant_id: ActiveValue::Set(key.tenant_id),
            subject_id: ActiveValue::Set(key.subject_id),
            personalization_enabled: ActiveValue::Set(personalization_enabled),
            erasure_in_progress: ActiveValue::NotSet,
        };
        let on_conflict = SecureOnConflict::<SubjectSettingsEntity>::columns([
            entity::Column::TenantId,
            entity::Column::SubjectId,
        ])
        .update_columns([entity::Column::PersonalizationEnabled])
        .map_err(map_scope_error)?;

        // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
        SubjectSettingsEntity::insert(row.clone())
            .secure()
            .scope_with_model(scope, &row)
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
        Ok(SettingsWrite::Written)
    }
}

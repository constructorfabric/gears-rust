// @cpt-dod:cpt-cf-settings-service-dod-category-management-no-orphan:p1
//! SeaORM-backed category persistence.

use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, ExprTrait, QueryFilter};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use toolkit_db::odata::LimitCfg;
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};

use crate::domain::category::visibility::DomainVisibility;
use crate::domain::category::{
    Category, CategoryDraft, CategoryKey, CategoryPatch, CategoryRepository, Patch,
};
use crate::domain::error::DomainError;
use crate::domain::odata::Listing;
use crate::domain::precondition;
use crate::domain::precondition::ETag;
use crate::infra::storage::clock::{now, stamp_after};
use crate::infra::storage::entity::category::{self, Entity as CategoryEntity};
use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
use crate::infra::storage::listing::list_counted;
use crate::infra::storage::odata_mapper::CategoryODataMapper;
use settings_service_sdk::odata::CategoryFilterField;

/// Page bounds for category listings. The platform ceiling is
/// `ODataLimits::max_top` (1000); these are this resource's own default and
/// clamp, chosen for an administrative listing where a page shows tens of rows.
const CATEGORY_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 200,
};

/// Persistence for categories.
pub struct CategoryRepo;

/// Derive the entity tag from the row's last write.
///
/// `updated_at` is refreshed on every write and stored to microsecond
/// precision, so two writes to one row cannot share a tag. Deriving it rather
/// than storing a separate version column keeps the tag and the row impossible
/// to desynchronise: there is no second field to forget to bump.
// @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-2
fn etag_of(model: &category::Model) -> ETag {
    ETag::new(format!("{}", model.updated_at.unix_timestamp_nanos()))
}
// @cpt-end:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-2

/// Converts a stored category row to the domain type, failing if its key no longer parses.
pub(crate) fn to_domain(model: category::Model) -> Result<Category, DomainError> {
    let etag = etag_of(&model);
    Ok(Category {
        id: model.id,
        // A key that failed validation cannot have been written through this
        // service, so a stored key that will not parse means the row was
        // written by something else — an internal fault, not a bad request.
        key: CategoryKey::parse(&model.key).map_err(|err| DomainError::Internal {
            diagnostic: format!("stored category key `{}` is invalid: {err}", model.key),
        })?,
        name: model.name,
        description: model.description,
        domain_affinity: model.domain_affinity,
        sort_order: model.sort_order,
        icon: model.icon,
        etag,
        updated_at: model.updated_at,
    })
}

/// Project a write failure.
///
/// Uniqueness is decided by `uq_category_key` / `uq_category_name`, never by a
/// prior read: two administrators creating the same key concurrently can both
/// pass a check-then-insert, and only the index catches the second. The driver
/// classifies the violation by SQLSTATE, so this does not sniff message text —
/// a wording change upstream would otherwise turn a conflict into a 500.
fn map_write_error(err: &toolkit_db::secure::ScopeError) -> DomainError {
    if err.is_unique_violation() {
        DomainError::Conflict {
            detail: "a category with this key or name already exists".to_owned(),
        }
    } else {
        DomainError::Internal {
            diagnostic: err.to_string(),
        }
    }
}

/// `ON DELETE RESTRICT` as `PostgreSQL` reports it: SQLSTATE `restrict_violation`.
const PG_RESTRICT_VIOLATION: &str = "23001";
/// A plain foreign-key rejection on `PostgreSQL`: SQLSTATE `foreign_key_violation`.
const PG_FOREIGN_KEY_VIOLATION: &str = "23503";
/// `ON DELETE RESTRICT` as `SQLite` reports it: extended code `SQLITE_CONSTRAINT_TRIGGER`.
const SQLITE_CONSTRAINT_TRIGGER: &str = "1811";
/// A plain foreign-key rejection on `SQLite`: extended code `SQLITE_CONSTRAINT_FOREIGNKEY`.
const SQLITE_CONSTRAINT_FOREIGNKEY: &str = "787";

/// Whether the driver itself reported a foreign-key refusal, by code.
///
/// `SeaORM`'s `sql_err()` files only the plain foreign-key pair, and the
/// toolkit's classifier then falls back to English message text, which a
/// `PostgreSQL` running with another `lc_messages` does not produce. The code is
/// read off the driver error here, so the race path does not depend on wording.
fn driver_reports_fk_refusal(err: &toolkit_db::secure::ScopeError) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};

    let toolkit_db::secure::ScopeError::Db(DbErr::Exec(RuntimeErr::SqlxError(driver))) = err else {
        return false;
    };
    let sqlx::Error::Database(db) = driver.as_ref() else {
        return false;
    };
    db.code().is_some_and(|code| {
        matches!(
            code.as_ref(),
            PG_RESTRICT_VIOLATION
                | PG_FOREIGN_KEY_VIOLATION
                | SQLITE_CONSTRAINT_TRIGGER
                | SQLITE_CONSTRAINT_FOREIGNKEY
        )
    })
}

/// Project a delete failure.
///
/// A foreign-key rejection means a declaration still references the category.
/// `ON DELETE RESTRICT` is the **authoritative** guard: the service's advisory
/// check runs first for a better message, but a declaration created between
/// that check and this delete is caught only here. Reporting it as an internal
/// error would turn a correct refusal into a 500.
// @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-11
fn map_delete_error(err: &toolkit_db::secure::ScopeError) -> DomainError {
    // The driver's code first, the toolkit's classifier as the second chance
    // for an error that was re-wrapped and lost its typed shape.
    let is_fk_violation = driver_reports_fk_refusal(err) || err.is_foreign_key_violation();
    if is_fk_violation {
        DomainError::Conflict {
            detail: "the category still has declarations referencing it".to_owned(),
        }
    } else {
        map_write_error(err)
    }
}
// @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-11

fn active_from(draft: &CategoryDraft) -> category::ActiveModel {
    category::ActiveModel {
        key: Set(draft.key.as_str().to_owned()),
        name: Set(draft.name.clone()),
        description: Set(draft.description.clone()),
        domain_affinity: Set(draft.domain_affinity.clone()),
        sort_order: Set(draft.sort_order),
        icon: Set(draft.icon.clone()),
        updated_at: Set(now()),
        ..Default::default()
    }
}

#[async_trait]
impl CategoryRepository for CategoryRepo {
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<Category>, DomainError> {
        let found = CategoryEntity::find_by_id(id)
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(|err| DomainError::Internal {
                diagnostic: err.to_string(),
            })?;
        found.map(to_domain).transpose()
    }

    async fn find_by_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: &CategoryKey,
    ) -> Result<Option<Category>, DomainError> {
        let found = CategoryEntity::find()
            .filter(category::Column::Key.eq(key.as_str()))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(|err| DomainError::Internal {
                diagnostic: err.to_string(),
            })?;
        found.map(to_domain).transpose()
    }

    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: CategoryDraft,
    ) -> Result<Category, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-8
        // `active_from` already stamped `updated_at`; a fresh row carries the
        // same instant in both columns.
        let mut active = active_from(&draft);
        active.id = Set(Uuid::new_v4());
        active.created_at = Set(now());
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-8

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-9
        let model = toolkit_db::secure::secure_insert::<CategoryEntity>(active, scope, conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-9
        to_domain(model)
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        patch: CategoryPatch,
        expected: time::OffsetDateTime,
    ) -> Result<Category, DomainError> {
        // `update_many` rather than `update`: the secure extension scopes a
        // filtered update, and scoping is the point — an id the caller cannot
        // see must not become an update they can perform. Filtered to one id
        // at one version, so it touches at most one row, and only the row the
        // caller's tag was compared against: a second writer holding the same
        // tag passes that comparison too, and only this filter stops it from
        // landing on top of the first.
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-12
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-13
        // Uniqueness on `name` is the index's call, surfaced by `map_write_error`
        // as a conflict. `key` is not re-checked here: the patch cannot carry one.
        // Only the columns the patch carries are set: a field it leaves alone
        // keeps its value, and an explicit clear writes NULL.
        fn write_of(patch: &Patch<String>) -> Option<sea_orm::sea_query::SimpleExpr> {
            match patch {
                Patch::Keep => None,
                Patch::Clear => Some(sea_orm::sea_query::Expr::value(None::<String>)),
                Patch::Set(value) => Some(sea_orm::sea_query::Expr::value(value.clone())),
            }
        }
        let mut update = CategoryEntity::update_many();
        if let Some(name) = patch.name.clone() {
            update = update.col_expr(
                category::Column::Name,
                sea_orm::sea_query::Expr::value(name),
            );
        }
        if let Some(description) = write_of(&patch.description) {
            update = update.col_expr(category::Column::Description, description);
        }
        if let Some(domain_affinity) = write_of(&patch.domain_affinity) {
            update = update.col_expr(category::Column::DomainAffinity, domain_affinity);
        }
        if let Some(sort_order) = patch.sort_order {
            update = update.col_expr(
                category::Column::SortOrder,
                sea_orm::sea_query::Expr::value(sort_order),
            );
        }
        if let Some(icon) = write_of(&patch.icon) {
            update = update.col_expr(category::Column::Icon, icon);
        }
        let outcome = update
            .col_expr(
                category::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(stamp_after(Some(expected))),
            )
            .filter(category::Column::Id.eq(id))
            .filter(category::Column::UpdatedAt.eq(expected))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| map_write_error(&err))?;
        if outcome.rows_affected == 0 {
            return Err(precondition::stale());
        }
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-13
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-12

        // Re-read so the caller receives the refreshed `updated_at`, and with it
        // the new ETag. Returning the draft instead would hand back a tag that
        // does not match what a subsequent `If-Match` would be compared against.
        self.find(conn, scope, id)
            .await?
            .ok_or(DomainError::NotFound {
                resource: "category",
            })
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-10
        // `delete_many` filtered to the id: sea-orm 2 hands back a validated
        // one-row delete from `delete_by_id`, which the secure extension does not
        // wrap, and scoping is the point of the call. Filtered to the version
        // too, so the row deleted is the one the caller's tag was compared
        // against and not whatever replaced it since.
        let outcome = CategoryEntity::delete_many()
            .filter(category::Column::Id.eq(id))
            .filter(category::Column::UpdatedAt.eq(expected))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(|err| map_delete_error(&err))?;
        if outcome.rows_affected == 0 {
            return Err(precondition::stale());
        }
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-10
        Ok(())
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        visibility: &DomainVisibility,
        query: &ODataQuery,
    ) -> Result<Listing<Category>, DomainError> {
        let mut select = CategoryEntity::find();

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-4
        // Inside the query, before pagination runs. Applied to the page after
        // the fact, this would return short pages and a cursor pointing past
        // rows the caller was entitled to see.
        //
        // The null arm is what keeps an undomained category universally
        // visible; without it every category with no domain vanishes for every
        // scoped administrator.
        if let DomainVisibility::Restricted(domains) = visibility {
            select = select.filter(
                category::Column::DomainAffinity
                    .is_null()
                    .or(category::Column::DomainAffinity.is_in(domains.clone())),
            );
        }
        // @cpt-end:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-4

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-7
        // A request with neither `$orderby` nor a cursor gets `sort_order asc`
        // injected as its order. A continuation carries its order inside the
        // cursor and must not be touched, or the page boundary would move.
        //
        // The paginator appends the `name asc` tiebreaker when `name` is not
        // already an order key. `name` is unique, so a page boundary can
        // neither repeat nor skip a row; the default order is therefore
        // `sort_order asc, name asc`, and a caller's `$orderby` is passed
        // through unchanged with the same tiebreaker behind it.
        let default_ordered;
        let query = if query.order.is_empty() && query.cursor.is_none() {
            let mut q = query.clone();
            q.order = ODataOrderBy(vec![OrderKey {
                field: "sort_order".to_owned(),
                dir: SortDir::Asc,
            }]);
            default_ordered = q;
            &default_ordered
        } else {
            query
        };
        // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-7
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-4
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-5
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-8
        // One gate for three refusals: an unmapped `$filter` field, an
        // unsupported operator, and a cursor that no longer decodes all
        // surface here as a validation failure rather than as an empty page.
        // Reporting any of them as "no results" would let an administrator
        // read a broken query as an empty catalogue.
        // The page and, beside it, how many categories the walk holds under
        // the same predicate — the rail's scrollbar and its heading count.
        let page = list_counted::<CategoryFilterField, CategoryODataMapper, _, _, _, _>(
            select,
            scope,
            conn,
            query,
            // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-7
            ("name", SortDir::Asc),
            // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-7
            CATEGORY_LIMIT_CFG,
            |m: category::Model| m,
        )
        .await
        .map_err(|err| DomainError::Validation {
            field: "query".to_owned(),
            code: crate::field::ODATA_QUERY,
            message: err.to_string(),
        })?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-8
        // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-5
        // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-4

        let items = page
            .items
            .into_iter()
            .map(to_domain)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Listing {
            items,
            page_info: page.page_info,
            total_count: page.total_count,
        })
    }

    async fn count_referencing_declarations<C: DBRunner>(
        &self,
        conn: &C,
        id: Uuid,
    ) -> Result<u64, DomainError> {
        // Deliberately unscoped. The question is whether the *database* still
        // holds a reference, not whether this caller may see one: a declaration
        // outside their visibility would otherwise answer "no" here while the
        // foreign key answers "yes", turning a clean `409` into a constraint
        // error the caller cannot act on.
        //
        // Every status counts, `retired` included. Retirement is a soft delete
        // that keeps the declaration filed under its category, so the row would
        // be orphaned by the delete exactly as an active one would.
        //
        // @cpt-begin:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-1
        // @cpt-begin:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-2
        // No `status` predicate, and that is the whole of step 2: retirement is
        // a soft delete that keeps the declaration filed under its category, so
        // a retired row would be orphaned by the delete exactly as an active one
        // would.
        DeclarationEntity::find()
            .filter(declaration::Column::CategoryId.eq(id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .count(conn)
            .await
            .map_err(|err| DomainError::Internal {
                diagnostic: err.to_string(),
            })
        // @cpt-end:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-2
        // @cpt-end:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-1
    }
}

#[cfg(test)]
#[path = "category_repo_tests.rs"]
mod category_repo_tests;

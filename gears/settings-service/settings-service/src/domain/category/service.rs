// Created: 2026-08-13 by Constructor Tech
//! Category operations.
//!
//! Generic over the repository rather than holding a trait object: the
//! repository's methods are generic over [`DBRunner`] so the same code runs on
//! a connection and inside a transaction, and a generic method cannot be part
//! of an object-safe trait. The cost is a type parameter; the benefit is that
//! every operation below can run in a transaction when its caller needs one.

use std::sync::Arc;

use toolkit_db::secure::DBRunner;
use toolkit_security::SecurityContext;

use crate::audit::{AuditEmitter, AuditRecord, AuditScope, AuditValue};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::visibility::{self, DomainVisibility};
use super::{Category, CategoryDraft, CategoryPatch, CategoryRepository};
use crate::api::precondition::{self, ETag};
use crate::domain::error::DomainError;

/// Refuse query options this resource does not implement.
///
/// `$select` is parsed by the platform but not honoured here: supporting it
/// means a response whose shape varies per request, and no caller has asked
/// for it. Refusing is deliberate rather than ignoring — a caller whose
/// projection was silently dropped receives every field believing it asked for
/// two, which is the same failure the declared filter surface exists to
/// prevent.
///
/// # Errors
/// [`DomainError::Validation`] naming the unsupported option.
fn reject_unsupported_options(query: &toolkit_odata::ODataQuery) -> Result<(), DomainError> {
    if query.select.is_some() {
        return Err(DomainError::Validation {
            field: "$select".to_owned(),
            code: crate::field::ODATA_UNSUPPORTED_OPTION,
            message: "$select is not supported on categories; omit it to receive the full \
                      representation"
                .to_owned(),
        });
    }
    Ok(())
}

/// Who performed a mutation and under which request.
///
/// The two always travel together — an audit record needs both, and splitting
/// them across parameters invites a call site that supplies one and forgets the
/// other.
#[derive(Clone, Copy)]
pub struct Actor<'a> {
    /// The authenticated caller.
    pub ctx: &'a SecurityContext,
    /// The id correlating this mutation with the response the caller saw.
    pub request_id: &'a str,
}

/// The audited representation of a category.
///
/// The pre/post images record what a reader needs to see what changed, which is
/// the mutable state — not the surrogate id, which never changes, nor the tag,
/// which is derived from the write itself.
fn snapshot(category: &Category) -> serde_json::Value {
    serde_json::json!({
        "key": category.key.as_str(),
        "name": category.name,
        "description": category.description,
        "domain_affinity": category.domain_affinity,
        "sort_order": category.sort_order,
        "icon": category.icon,
    })
}

/// Category create, read, update and delete.
pub struct CategoryService<R> {
    repo: R,
    audit: Arc<dyn AuditEmitter>,
}

impl<R: CategoryRepository> CategoryService<R> {
    /// Build the service over a repository and an audit destination.
    ///
    /// The emitter is required, not optional. An `Option` here would make
    /// "no audit configured" a supported state, and a mutation could then
    /// succeed leaving no trail — which is precisely what DESIGN.md §4.2's
    /// fail-closed rule forbids.
    pub fn new(repo: R, audit: Arc<dyn AuditEmitter>) -> Self {
        Self { repo, audit }
    }

    /// Record a mutation, failing the operation if the trail cannot be written.
    async fn record(
        &self,
        key: &super::CategoryKey,
        action: &'static str,
        pre: Option<AuditValue>,
        post: Option<AuditValue>,
        actor: Actor<'_>,
    ) -> Result<(), DomainError> {
        // Categories are platform-global -- the table has no tenant column --
        // so their audit scope is the platform row rather than a tenant's.
        let mut rec = AuditRecord::new(
            key.as_str(),
            AuditScope::Platform,
            actor.ctx.subject_id().to_string(),
            action,
            actor.request_id,
        );
        if let Some(pre) = pre {
            rec = rec.with_pre_image(pre);
        }
        if let Some(post) = post {
            rec = rec.with_post_image(post);
        }
        self.audit.audit(rec).await
    }

    /// Fetch one category.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when no category has that id **or** the caller
    /// cannot see it — the two are one answer on purpose, since a distinct
    /// "exists but forbidden" would let a caller enumerate ids it has no access
    /// to.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Category, DomainError> {
        let found = self.repo.find(conn, scope, id).await?;

        // @cpt-begin:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-3
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-6
        // The row-level arm of the visibility rule. `list` applies it as a SQL
        // predicate; a single-row fetch has no query to augment, so it is
        // applied here — and applied as *not found* rather than *forbidden*,
        // because a distinct denial would confirm the category exists.
        let visible = visibility::domain_visibility(scope);
        let found = found.filter(|category| {
            visibility::is_visible(&visible, category.domain_affinity.as_deref())
        });
        // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-6
        // @cpt-end:cpt-cf-settings-service-algo-category-management-visibility-filter:p1:inst-cat-visfilter-3

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-7
        // Not-found, not forbidden: a distinct denial would confirm that a
        // category the caller may not see exists.
        found.ok_or(DomainError::NotFound {
            resource: "category",
        })
        // @cpt-end:cpt-cf-settings-service-flow-category-management-get:p1:inst-cat-get-7
    }

    /// The caller's domain restriction, for a handler to fold into a list query.
    ///
    /// Exposed rather than applied here because `list` must put the predicate
    /// **inside** the query: filtering a page after the fact gives short pages
    /// and a cursor that skips rows the caller was entitled to.
    #[must_use]
    pub fn visibility(scope: &AccessScope) -> DomainVisibility {
        visibility::domain_visibility(scope)
    }

    /// List categories for the caller.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the query names an unmapped field, uses
    /// an unsupported operator, requests an unsupported option, or carries an
    /// undecodable cursor.
    pub async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<Category>, DomainError> {
        reject_unsupported_options(query)?;
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-6
        let visible = visibility::domain_visibility(scope);
        // @cpt-end:cpt-cf-settings-service-flow-category-management-list:p1:inst-cat-list-6
        self.repo.list(conn, scope, &visible, query).await
    }

    /// Create a category.
    ///
    /// # Errors
    /// [`DomainError::Conflict`] when the key or name is taken.
    pub async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: CategoryDraft,
        actor: Actor<'_>,
    ) -> Result<Category, DomainError> {
        // A courtesy check, not the guarantee. `uq_category_key` decides, and
        // the repository surfaces its violation — two administrators creating
        // the same key concurrently both pass here and only one insert wins.
        // Skipping this would be correct but would report the collision as a
        // database error rather than as the conflict it is.
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-10
        if self
            .repo
            .find_by_key(conn, scope, &draft.key)
            .await?
            .is_some()
        {
            return Err(DomainError::Conflict {
                detail: format!("a category with key `{}` already exists", draft.key),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-10
        let created = self.repo.insert(conn, scope, draft).await?;

        // A create has no pre-image. Audited after the write commits, so the
        // record describes what exists rather than what was attempted.
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-11
        self.record(
            &created.key,
            "category.create",
            None,
            Some(AuditValue::record(snapshot(&created), false)),
            actor,
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-create:p1:inst-cat-create-11
        Ok(created)
    }

    /// Update a category, guarded by `If-Match`.
    ///
    /// # Errors
    /// [`DomainError::PreconditionRequired`] when no `If-Match` was supplied,
    /// [`DomainError::PreconditionFailed`] when it is stale,
    /// [`DomainError::NotFound`] when the category is not visible to the caller.
    pub async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        if_match: Option<&str>,
        patch: CategoryPatch,
        actor: Actor<'_>,
    ) -> Result<Category, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-5
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-6
        // One call answers both: the row lookup and the not-found verdict,
        // including a row hidden by the visibility rule.
        let current = self.get(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-6
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-5
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-7
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-8
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-9
        // Absent and stale are different statuses (428 vs 412); the shared
        // evaluator owns that distinction so no handler re-decides it.
        precondition::evaluate(if_match, &current.etag)?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-9
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-8
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-7

        let updated = self.repo.update(conn, scope, id, patch).await?;
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-14
        self.record(
            &updated.key,
            "category.update",
            Some(AuditValue::record(snapshot(&current), false)),
            Some(AuditValue::record(snapshot(&updated), false)),
            actor,
        )
        .await?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-update:p1:inst-cat-update-14
        Ok(updated)
    }

    /// Delete a category, guarded by `If-Match` and the no-orphan rule.
    ///
    /// # Errors
    /// [`DomainError::PreconditionRequired`] / [`DomainError::PreconditionFailed`]
    /// as for [`Self::update`], [`DomainError::NotFound`] when not visible, and
    /// [`DomainError::Conflict`] while any declaration still references it.
    pub async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        if_match: Option<&str>,
        actor: Actor<'_>,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-4
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-5
        let current = self.get(conn, scope, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-5
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-4
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-6
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-7
        precondition::evaluate(if_match, &current.etag)?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-7
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-6

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-8
        // Advisory, by design. The foreign key's `ON DELETE RESTRICT` is the
        // authoritative guard and catches a declaration created between this
        // check and the delete; what this adds is a specific message instead of
        // a constraint error, for the overwhelmingly common case.
        //
        // The check answering `Err` still denies: the guard exists to prevent an
        // orphan, and an unanswerable question is not a negative answer.
        let referenced = self.repo.has_referencing_declarations(conn, id).await?;
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-8

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-9
        if referenced {
            return Err(DomainError::Conflict {
                detail: "the category still has declarations referencing it".to_owned(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-9

        self.repo.delete(conn, scope, id).await?;

        // A delete has no post-image. The pre-image is what makes the trail
        // useful: after the row is gone it is the only record of what was
        // removed.
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-12
        self.record(
            &current.key,
            "category.delete",
            Some(AuditValue::record(snapshot(&current), false)),
            None,
            actor,
        )
        .await
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-12
    }

    /// The tag a caller must echo to mutate this category.
    ///
    /// Exposed so a handler can set the `ETag` response header from the same
    /// value the next `If-Match` will be compared against.
    #[must_use]
    pub fn etag_of(category: &Category) -> &ETag {
        &category.etag
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

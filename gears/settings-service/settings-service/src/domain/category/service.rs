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

use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::visibility::{self, DomainVisibility};
use super::{Category, CategoryDraft, CategoryPatch, CategoryRepository};
use crate::api::precondition::{self, ETag};
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;

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
pub struct CategoryService<R, S> {
    repo: R,
    sink: S,
    /// Where the root tenant's id comes from. Categories are platform-scoped,
    /// and platform scope is that id rather than an absent tenant (DESIGN.md
    /// §4.1) — asked for at the first mutation, since the Tenant Resolver is
    /// fetched at first use and never during init (DESIGN.md §4.9).
    scope: Arc<dyn PlatformScope>,
}

impl<R: CategoryRepository, S: AuditSink> CategoryService<R, S> {
    /// Build the service over a repository and an audit sink.
    ///
    /// The sink is required, not optional. An `Option` here would make
    /// "no audit configured" a supported state, and a mutation could then
    /// succeed leaving no trail — which is precisely what DESIGN.md §4.2's
    /// fail-closed rule forbids.
    pub fn new(repo: R, sink: S, scope: Arc<dyn PlatformScope>) -> Self {
        Self { repo, sink, scope }
    }

    /// Record a mutation, failing the operation if the trail cannot be written.
    // The record's inputs are the mutation's own facts; bundling them would add
    // a struct that exists only to be unpacked here.
    // @cpt-dod:cpt-cf-settings-service-dod-category-management-audit:p1
    #[allow(clippy::too_many_arguments)]
    async fn record<C: DBRunner>(
        &self,
        conn: &C,
        access: &AccessScope,
        key: &super::CategoryKey,
        operation: AuditOperation,
        pre: Option<AuditValue>,
        post: Option<AuditValue>,
        actor: Actor<'_>,
    ) -> Result<(), DomainError> {
        // Categories are platform-global -- the table has no tenant column --
        // so their audit scope is the root tenant's, which is platform scope.
        // A mutation that cannot name its scope does not proceed.
        let tenant = self.scope.root_tenant().await?;
        let mut rec = AuditRecord::new(
            key.as_str(),
            tenant,
            actor.ctx.subject_id().to_string(),
            operation,
            actor.request_id,
        );
        if let Some(pre) = pre {
            rec = rec.with_pre_image(pre);
        }
        if let Some(post) = post {
            rec = rec.with_post_image(post);
        }
        // Last step of the mutation's transaction: the record commits with the
        // change or rolls back with it.
        self.sink.append(conn, access, rec).await
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
        crate::domain::odata::reject_unsupported_options(query, "categories")?;
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
            conn,
            scope,
            &created.key,
            AuditOperation::Create,
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
            conn,
            scope,
            &updated.key,
            AuditOperation::Change,
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
        // @cpt-begin:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-4
        // Advisory, by design. The foreign key's `ON DELETE RESTRICT` is the
        // authoritative guard and catches a declaration created between this
        // check and the delete; what this adds is a specific message instead of
        // a constraint error, for the overwhelmingly common case.
        //
        // The check answering `Err` still denies: the guard exists to prevent an
        // orphan, and an unanswerable question is not a negative answer.
        let referencing = self.repo.count_referencing_declarations(conn, id).await?;
        // @cpt-end:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-4
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-8

        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-9
        // @cpt-begin:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-3
        // The count travels into the message: an administrator who must clear
        // the category first is told how much is in the way, not merely that
        // something is.
        if referencing > 0 {
            return Err(DomainError::Conflict {
                detail: format!(
                    "the category still has {referencing} declaration(s) referencing it"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-category-management-no-orphan-guard:p1:inst-cat-orphan-3
        // @cpt-end:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-9

        self.repo.delete(conn, scope, id).await?;

        // A delete has no post-image. The pre-image is what makes the trail
        // useful: after the row is gone it is the only record of what was
        // removed.
        // @cpt-begin:cpt-cf-settings-service-flow-category-management-delete:p1:inst-cat-delete-12
        self.record(
            conn,
            scope,
            &current.key,
            AuditOperation::Remove,
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

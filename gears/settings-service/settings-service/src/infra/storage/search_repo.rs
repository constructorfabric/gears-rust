//! The search queries, in the dialect of the database in use.
//!
//! Two dialects, one shape. On `PostgreSQL` every predicate is `ILIKE` over
//! exactly the expression a trigram GIN index is built on — `key`,
//! `display_name`, `description`, `categories.name`, `(default_value #>> '{}')`,
//! `(value #>> '{}')` — with the classification predicate the split index
//! pairs of DESIGN §4.7 carry, so each half of a pair serves its own corpus.
//! On `SQLite` the same predicates are `LIKE` scans; the gear's single-node
//! shape accepts that (DESIGN §3), and the test suite runs on it.
//!
//! Correctness never rests on the plan: the corpus is a predicate in the
//! query, and an unentitled caller's query does not name a `pii` row.

use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    QueryTrait,
};
use toolkit_db::odata::LimitCfg;
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::SortDir;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::Category;
use crate::domain::declaration::{Declaration, DeclarationStatus};
use crate::domain::error::DomainError;
use crate::domain::odata::Listing;
use crate::domain::search::{Corpus, Needle, SearchRepository, SearchRequest};
use crate::domain::value::StoredValue;
use crate::infra::storage::declaration_odata_mapper::DeclarationODataMapper;
use crate::infra::storage::entity::category::{self, Entity as CategoryEntity};
use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};
use crate::infra::storage::listing::list_counted;
use crate::infra::storage::text_match::{Dialect, TextMatch};
use crate::infra::storage::{category_repo, declaration_repo, value_repo};
use settings_service_sdk::odata::DeclarationFilterField;

/// A page is a number of **settings**; a setting with several matching
/// overrides contributes one hit per override on top of its own.
pub const SEARCH_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 100,
};

/// The binding: the text match of the dialect in use, and nothing else —
/// every query takes its connection from the caller.
#[derive(Debug, Clone, Copy)]
pub struct SearchRepo {
    text: TextMatch,
}

impl SearchRepo {
    /// Bind to the database in use.
    #[must_use]
    pub const fn new(backend: DbBackend) -> Self {
        Self {
            text: TextMatch::new(Dialect::from_backend(backend)),
        }
    }

    /// The predicate every value match shares: never a secret row, never a
    /// subject-scoped row, only the corpus's classifications, only the given
    /// tenants, and the text projection containing the pattern. Stated once so
    /// the page's subquery and the override read cannot drift apart — and so
    /// that on `PostgreSQL` each is served by the same half of the split index
    /// pair, whose predicate this is.
    fn value_matches(self, corpus: Corpus, tenant_ids: &[Uuid], pattern: &str) -> Condition {
        Condition::all()
            // A flagged override is not served — resolution falls through it —
            // so what it holds is not a match.
            .add(setting_value::Column::NeedsReview.eq(false))
            .add(setting_value::Column::SecretRef.is_null())
            .add(value_repo::subjectless())
            .add(
                setting_value::Column::DataClassification
                    .is_in(corpus.classifications().iter().copied()),
            )
            .add(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .add(
                self.text
                    .json_text_contains("setting_values.value", pattern),
            )
    }
}

fn db_error(err: impl std::fmt::Display) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

// @cpt-dod:cpt-cf-settings-service-dod-search-discoverability-corpus:p2
#[async_trait]
impl SearchRepository for SearchRepo {
    async fn declarations<C: DBRunner>(
        &self,
        conn: &C,
        request: &SearchRequest<'_>,
    ) -> Result<Listing<Declaration>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-7
        let pattern = request.needle.like_pattern();

        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-1
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-2
        // Overrides that match, as the declarations they belong to.
        let overrides = ValueEntity::find()
            .select_only()
            .column(setting_value::Column::DeclarationId)
            .filter(self.value_matches(request.corpus, request.tenant_ids, &pattern));
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-2
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-1

        let matched = Condition::any()
            .add(self.text.declaration_text_matches(
                request.needle,
                request.corpus,
                request.visibility,
            ))
            .add(declaration::Column::Id.in_subquery(overrides.into_query()));

        let mut base = declaration_repo::exclude_hidden_for(
            declaration_repo::apply_visibility(DeclarationEntity::find(), request.visibility),
            request.hidden_for,
        )
        .filter(declaration::Column::Status.eq(DeclarationStatus::Active.as_str()))
        .filter(matched);
        // `needs_review eq true`, in the query: only settings whose own
        // override at the target is flagged for review — the same narrowing the
        // browse applies. A page filtered afterwards would page the unfiltered
        // set, which is the defect a filter in the query exists to avoid.
        if request.flagged_only {
            base = base.filter(
                declaration::Column::Id
                    .in_subquery(value_repo::flagged_declarations_at(request.target)),
            );
        }

        // Tiebreaker `key`, unique, so a page boundary neither repeats nor
        // skips a row. The query carries the page, the `$filter` remainder on
        // `key` and `category_id` — browse's grammar, on the declaration
        // columns — and the binding the cursor must match. The total beside
        // the page counts the settings that match in all, under the same
        // corpus and the same exclusions, so it discloses nothing a page of
        // hits does not.
        let page = list_counted::<DeclarationFilterField, DeclarationODataMapper, _, _, _, _>(
            base,
            request.scope,
            conn,
            request.query,
            ("key", SortDir::Asc),
            SEARCH_LIMIT_CFG,
            |m: declaration::Model| m,
        )
        .await
        .map_err(|err| DomainError::Validation {
            field: "cursor".to_owned(),
            code: crate::field::ODATA_QUERY,
            message: err.to_string(),
        })?;

        Ok(Listing {
            items: page
                .items
                .into_iter()
                .map(declaration_repo::to_domain)
                .collect(),
            page_info: page.page_info,
            total_count: page.total_count,
        })
        // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-7
    }

    async fn overrides<C: DBRunner>(
        &self,
        conn: &C,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
        needle: &Needle,
        corpus: Corpus,
        limit: usize,
    ) -> Result<Vec<StoredValue>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-8
        if declaration_ids.is_empty() || tenant_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = ValueEntity::find()
            .filter(setting_value::Column::DeclarationId.is_in(declaration_ids.iter().copied()))
            .filter(self.value_matches(corpus, tenant_ids, &needle.like_pattern()))
            .order_by_asc(setting_value::Column::DeclarationId)
            .order_by_asc(setting_value::Column::TenantId)
            // One past the bound: the service tells a full answer from a cut one.
            .limit(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
            // The subtree is the filter, as it is for the flagged listing.
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        Ok(rows.into_iter().map(value_repo::to_domain).collect())
        // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-8
    }

    async fn categories<C: DBRunner>(
        &self,
        conn: &C,
        ids: &[Uuid],
    ) -> Result<Vec<Category>, DomainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // Categories are platform-global; the declaration the caller already
        // sees names its category, so the breadcrumb is not a second decision.
        let rows = CategoryEntity::find()
            .filter(category::Column::Id.is_in(ids.iter().copied()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_error)?;
        rows.into_iter().map(category_repo::to_domain).collect()
    }
}

#[cfg(test)]
#[path = "search_repo_tests.rs"]
mod search_repo_tests;

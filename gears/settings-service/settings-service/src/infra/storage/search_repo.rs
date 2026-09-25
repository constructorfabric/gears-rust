// Created: 2026-09-17 by Virtuozzo International GmbH
//! The search queries, in the dialect of the database in use.
//!
//! Two dialects, one shape. On `PostgreSQL` every predicate is `ILIKE` over
//! exactly the expression a trigram GIN index is built on — `key`,
//! `description`, `categories.name`, `(default_value #>> '{}')`,
//! `(value #>> '{}')` — with the classification predicate the split index
//! pairs of DESIGN §4.7 carry, so each half of a pair serves its own corpus.
//! On `SQLite` the same predicates are `LIKE` scans; the gear's single-node
//! shape accepts that (DESIGN §3), and the test suite runs on it.
//!
//! Correctness never rests on the plan: the corpus is a predicate in the
//! query, and an unentitled caller's query does not name a `pii` row.

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, ExprTrait, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait,
};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::{Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::category::Category;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::search::{Corpus, Needle, SearchRepository, SearchRequest};
use crate::domain::value::StoredValue;
use crate::infra::storage::declaration_odata_mapper::DeclarationODataMapper;
use crate::infra::storage::entity::category::{self, Entity as CategoryEntity};
use crate::infra::storage::entity::declaration::{self, Entity as DeclarationEntity};
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};
use crate::infra::storage::{category_repo, declaration_repo, value_repo};
use settings_service_sdk::odata::DeclarationFilterField;

/// A page is a number of **settings**; a setting with several matching
/// overrides contributes one hit per override on top of its own.
pub const SEARCH_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 25,
    max: 100,
};

/// The spelling of the substring predicate and the JSON text projection.
// @cpt-dod:cpt-cf-settings-service-dod-search-discoverability-dialect:p2
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `ILIKE` over the trigram-indexed expressions.
    Postgres,
    /// `LIKE`, a scan; `SQLite`'s `LIKE` is case-insensitive for ASCII.
    Sqlite,
}

impl Dialect {
    /// The dialect for the database the gear was started against. `MySQL` is
    /// out of scope for this gear (DESIGN §3) and gets the portable spelling.
    #[must_use]
    pub const fn from_backend(backend: DbBackend) -> Self {
        match backend {
            DbBackend::Postgres => Self::Postgres,
            _ => Self::Sqlite,
        }
    }

    // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-3
    /// The case-insensitive substring operator.
    const fn contains_operator(self) -> &'static str {
        match self {
            Self::Postgres => "ILIKE",
            Self::Sqlite => "LIKE",
        }
    }

    /// The text projection of a JSON column: on `PostgreSQL` the exact
    /// expression `idx_declarations_default_trgm` and `idx_values_value_trgm`
    /// are built over, so the planner can use them.
    fn text_of(self, json_column: &str) -> String {
        match self {
            Self::Postgres => format!("({json_column} #>> '{{}}')"),
            // `json_extract(…, '$')` unwraps a string but projects a boolean
            // as the integer 1/0, which no word matches; `json()` keeps the
            // JSON spelling — `true`, `12.5`, an object — which is what the
            // other backend and the Rust-side attribution project.
            Self::Sqlite => format!(
                "(CASE json_type({json_column}) WHEN 'text' THEN json_extract({json_column}, '$') \
                 ELSE json({json_column}) END)"
            ),
        }
    }

    /// The predicate that keeps a JSON `null` out of the default corpus — the
    /// same term the partial indexes carry, so the query matches their
    /// predicate on `PostgreSQL`.
    fn not_json_null(self, json_column: &str) -> String {
        match self {
            Self::Postgres => format!("jsonb_typeof({json_column}) <> 'null'"),
            Self::Sqlite => format!("json_type({json_column}) <> 'null'"),
        }
    }

    /// The bound-parameter token a custom expression is written with. It
    /// follows the backend, not sea-query: a template written for the other
    /// backend is passed through as literal text and matches nothing.
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Postgres => "$1",
            Self::Sqlite => "?",
        }
    }
    // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-needle:p2:inst-sd-needle-3
}

/// The binding: the dialect, and nothing else — every query takes its
/// connection from the caller.
#[derive(Debug, Clone, Copy)]
pub struct SearchRepo {
    dialect: Dialect,
}

impl SearchRepo {
    /// Bind to the database in use.
    #[must_use]
    pub const fn new(backend: DbBackend) -> Self {
        Self {
            dialect: Dialect::from_backend(backend),
        }
    }

    /// `column_expr <op> <param> ESCAPE '\'` as a custom expression, the
    /// pattern bound as a parameter; `ESCAPE` names the backslash
    /// `like_escape` used, which `SQLite` has no default for.
    fn contains(self, column_expr: &str, pattern: &str) -> Expr {
        Expr::cust_with_values(
            format!(
                "{column_expr} {} {} ESCAPE '\\'",
                self.dialect.contains_operator(),
                self.dialect.placeholder()
            ),
            [pattern.to_owned()],
        )
    }

    /// The predicate every value match shares: never a secret row, never a
    /// subject-scoped row, only the corpus's classifications, only the given
    /// tenants, and the text projection containing the pattern. Stated once so
    /// the page's subquery and the override read cannot drift apart — and so
    /// that on `PostgreSQL` each is served by the same half of the split index
    /// pair, whose predicate this is.
    fn value_matches(self, corpus: Corpus, tenant_ids: &[Uuid], pattern: &str) -> Condition {
        Condition::all()
            .add(setting_value::Column::SecretRef.is_null())
            .add(value_repo::subjectless())
            .add(
                setting_value::Column::DataClassification
                    .is_in(corpus.classifications().iter().copied()),
            )
            .add(setting_value::Column::TenantId.is_in(tenant_ids.iter().copied()))
            .add(self.contains(&self.dialect.text_of("setting_values.value"), pattern))
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
    ) -> Result<Page<Declaration>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-7
        let pattern = request.needle.like_pattern();

        // Categories whose name matches. Domain visibility applies to them as
        // it applies to a category listing.
        let mut categories = CategoryEntity::find()
            .select_only()
            .column(category::Column::Id)
            .filter(self.contains("categories.name", &pattern));
        if let DomainVisibility::Restricted(domains) = request.visibility {
            categories = categories.filter(
                category::Column::DomainAffinity
                    .is_null()
                    .or(category::Column::DomainAffinity.is_in(domains.clone())),
            );
        }

        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-1
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-2
        // Overrides that match, as the declarations they belong to.
        let overrides = ValueEntity::find()
            .select_only()
            .column(setting_value::Column::DeclarationId)
            .filter(self.value_matches(request.corpus, request.tenant_ids, &pattern));
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-2
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-1

        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-3
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-4
        // The Schema Default, within the corpus and not JSON null: the column
        // is NOT NULL, so a setting with no meaningful default holds `null`,
        // whose text projection is the word.
        let default_matches = Condition::all()
            .add(
                declaration::Column::DataClassification
                    .is_in(request.corpus.classifications().iter().copied()),
            )
            .add(Expr::cust(
                self.dialect
                    .not_json_null("setting_declarations.default_value"),
            ))
            .add(self.contains(
                &self.dialect.text_of("setting_declarations.default_value"),
                &pattern,
            ));
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-4
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-3

        let matched = Condition::any()
            .add(self.contains("setting_declarations.key", &pattern))
            .add(self.contains("setting_declarations.description", &pattern))
            .add(declaration::Column::CategoryId.in_subquery(categories.into_query()))
            .add(default_matches)
            .add(declaration::Column::Id.in_subquery(overrides.into_query()));

        let base = declaration_repo::exclude_hidden_for(
            declaration_repo::apply_visibility(DeclarationEntity::find(), request.visibility),
            request.hidden_for,
        )
        .filter(declaration::Column::Status.eq("active"))
        .filter(matched)
        .secure()
        .scope_with(request.scope);

        // Tiebreaker `key`, unique, so a page boundary neither repeats nor
        // skips a row. No OData filter: the query carries only the page and
        // the binding the cursor must match.
        let page = paginate_odata::<DeclarationFilterField, DeclarationODataMapper, _, _, _, _>(
            base,
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

        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(declaration_repo::to_domain)
                .collect(),
            page_info: page.page_info,
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

//! The spelling of a text match, in the dialect of the database in use.
//!
//! Stated once for the search's page query and the browse narrowing's read
//! over a candidate batch, so the two cannot drift apart. A leaf: neither
//! repository is imported here, so both may depend on it without depending
//! on each other.

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, ExprTrait, QueryFilter, QuerySelect, QueryTrait,
};

use crate::domain::category::visibility::DomainVisibility;
use crate::domain::search::needle::{Corpus, Needle};
use crate::infra::storage::entity::category::{self, Entity as CategoryEntity};
use crate::infra::storage::entity::declaration;

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

/// The text match for one dialect: the substring predicate with its pattern
/// bound as a parameter, the text projection of a JSON column, and the
/// declaration-text half of a match.
#[derive(Debug, Clone, Copy)]
pub struct TextMatch {
    dialect: Dialect,
}

impl TextMatch {
    /// Bind to a dialect already decided.
    #[must_use]
    pub const fn new(dialect: Dialect) -> Self {
        Self { dialect }
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

    /// The text projection of a JSON column containing the pattern.
    pub(crate) fn json_text_contains(self, json_column: &str, pattern: &str) -> Expr {
        self.contains(&self.dialect.text_of(json_column), pattern)
    }

    /// The declaration-text half of a match: `key`, `display_name`,
    /// `description`, the name of the category (within the domains the caller
    /// may see), or the Schema Default within the corpus and not JSON `null`.
    ///
    /// Stated once for the search's page query and the browse narrowing's
    /// read over a candidate batch, so the two cannot drift apart.
    pub(crate) fn declaration_text_matches(
        self,
        needle: &Needle,
        corpus: Corpus,
        visibility: &DomainVisibility,
    ) -> Condition {
        let pattern = needle.like_pattern();

        // Categories whose name matches. Domain visibility applies to them as
        // it applies to a category listing.
        let mut categories = CategoryEntity::find()
            .select_only()
            .column(category::Column::Id)
            .filter(self.contains("categories.name", &pattern));
        if let DomainVisibility::Restricted(domains) = visibility {
            categories = categories.filter(
                category::Column::DomainAffinity
                    .is_null()
                    .or(category::Column::DomainAffinity.is_in(domains.clone())),
            );
        }

        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-3
        // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-4
        // The Schema Default, within the corpus and not JSON null: the column
        // is NOT NULL, so a setting with no meaningful default holds `null`,
        // whose text projection is the word.
        let default_matches = Condition::all()
            .add(
                declaration::Column::DataClassification
                    .is_in(corpus.classifications().iter().copied()),
            )
            .add(Expr::cust(
                self.dialect
                    .not_json_null("setting_declarations.default_value"),
            ))
            .add(self.json_text_contains("setting_declarations.default_value", &pattern));
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-4
        // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-corpus:p2:inst-sd-corpus-3

        Condition::any()
            .add(self.contains("setting_declarations.key", &pattern))
            .add(self.contains("setting_declarations.display_name", &pattern))
            .add(self.contains("setting_declarations.description", &pattern))
            .add(declaration::Column::CategoryId.in_subquery(categories.into_query()))
            .add(default_matches)
    }
}

#[cfg(test)]
#[path = "text_match_tests.rs"]
mod text_match_tests;

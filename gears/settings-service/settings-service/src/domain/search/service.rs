// Created: 2026-09-17 by Constructor Tech
//! The search service: one page of declarations, its matching overrides, and
//! the attribution of every hit to the field that matched.
//!
//! The repository returns rows the database matched; this module turns them
//! into hits a client can act on. It makes no authorization decision of its
//! own — the scope, the corpus and the tenant set arrive decided — and it
//! never drops a row the database returned: a match the client is not told
//! about is a match that leaked into a count and nowhere else.

use std::collections::HashMap;
use std::sync::Arc;

use toolkit_db::secure::DBRunner;
use toolkit_odata::PageInfo;
use uuid::Uuid;

use super::{Corpus, MatchedField, Needle, SearchRepository, SearchRequest, text_projection};
use crate::domain::category::Category;
use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::value::StoredValue;

/// One result: a declaration and the field that matched; for a value match,
/// the stored row it was found in.
#[derive(Debug, Clone)]
pub struct Hit {
    /// The setting.
    pub declaration: Arc<Declaration>,
    /// Its category, for the breadcrumb; absent only if the category row
    /// vanished between the two queries.
    pub category: Option<Arc<Category>>,
    /// Which field matched.
    pub matched: MatchedField,
    /// The override, for a [`MatchedField::Value`] hit.
    pub row: Option<StoredValue>,
}

/// A page of hits: the settings the page holds, expanded into their hits.
#[derive(Debug, Clone)]
pub struct SearchPage {
    /// In key order; a setting's own hit before its override hits, those in
    /// tenant order.
    pub hits: Vec<Hit>,
    /// The cursors of the underlying page of settings.
    pub page_info: PageInfo,
}

/// The service.
pub struct SearchService<R: SearchRepository> {
    repo: R,
}

impl<R: SearchRepository> SearchService<R> {
    /// Build the service over its repository.
    pub const fn new(repo: R) -> Self {
        Self { repo }
    }

    /// The repository, for a test that staged it.
    #[cfg(test)]
    pub const fn repository(&self) -> &R {
        &self.repo
    }

    /// Run a search: the page of declarations, their matching overrides, the
    /// categories for breadcrumbs, and one or more hits per declaration.
    ///
    /// # Errors
    /// As the repository's reads.
    pub async fn search<C: DBRunner>(
        &self,
        conn: &C,
        request: &SearchRequest<'_>,
    ) -> Result<SearchPage, DomainError> {
        let page = self.repo.declarations(conn, request).await?;
        let ids: Vec<Uuid> = page.items.iter().map(|d| d.id).collect();
        let overrides = self
            .repo
            .overrides(
                conn,
                &ids,
                request.tenant_ids,
                request.needle,
                request.corpus,
            )
            .await?;
        let mut category_ids: Vec<Uuid> = page.items.iter().map(|d| d.category_id).collect();
        category_ids.sort_unstable();
        category_ids.dedup();
        let categories: HashMap<Uuid, Arc<Category>> = self
            .repo
            .categories(conn, &category_ids)
            .await?
            .into_iter()
            .map(|c| (c.id, Arc::new(c)))
            .collect();

        // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-9
        let mut hits = Vec::new();
        for declaration in page.items {
            let declaration = Arc::new(declaration);
            let category = categories.get(&declaration.category_id).cloned();
            let own_rows: Vec<&StoredValue> = overrides
                .iter()
                .filter(|r| r.declaration_id == declaration.id)
                .collect();
            let matched = declaration_match(
                &declaration,
                category.as_deref(),
                request.needle,
                request.corpus,
            );
            // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-3
            // The database returned this declaration, so something matched.
            // If neither a field nor an override names it in Rust — a JSON
            // projection spelled with different whitespace, a case fold the
            // engines disagree on — attribute it rather than drop it: to the
            // default when that is in the corpus, since its projection is the
            // one that can differ, else to the key.
            let matched = matched.or_else(|| {
                own_rows.is_empty().then(|| {
                    if request.corpus.admits(&declaration.data_classification)
                        && !declaration.default_value.is_null()
                    {
                        MatchedField::DefaultValue
                    } else {
                        MatchedField::Key
                    }
                })
            });
            // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-3
            if let Some(matched) = matched {
                hits.push(Hit {
                    declaration: Arc::clone(&declaration),
                    category: category.clone(),
                    matched,
                    row: None,
                });
            }
            for row in own_rows {
                hits.push(Hit {
                    declaration: Arc::clone(&declaration),
                    category: category.clone(),
                    matched: MatchedField::Value,
                    row: Some(row.clone()),
                });
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-9

        Ok(SearchPage {
            hits,
            page_info: page.page_info,
        })
    }
}

/// The field a declaration-level hit is attributed to: the first that contains
/// the needle, in the order a client is told about them — key, description,
/// category name, Schema Default. `None` when none does, which is the case
/// for a declaration on the page only because an override matched.
///
/// The Schema Default counts only within the corpus and only when it is not
/// JSON `null` — the same terms the database admitted it on.
#[must_use]
pub fn declaration_match(
    declaration: &Declaration,
    category: Option<&Category>,
    needle: &Needle,
    corpus: Corpus,
) -> Option<MatchedField> {
    // @cpt-begin:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-1
    if needle.matches(&declaration.key) {
        return Some(MatchedField::Key);
    }
    if declaration
        .description
        .as_deref()
        .is_some_and(|d| needle.matches(d))
    {
        return Some(MatchedField::Description);
    }
    if category.is_some_and(|c| needle.matches(&c.name)) {
        return Some(MatchedField::CategoryName);
    }
    let default_in_corpus =
        corpus.admits(&declaration.data_classification) && !declaration.default_value.is_null();
    if default_in_corpus && needle.matches(&text_projection(&declaration.default_value)) {
        return Some(MatchedField::DefaultValue);
    }
    None
    // @cpt-end:cpt-cf-settings-service-algo-search-discoverability-attribution:p2:inst-sd-attr-1
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

// Created: 2026-08-26 by Constructor Tech
//! The declaration read service.
//!
//! Owns the two things a handler must not be trusted to remember: that the
//! visibility predicate travels into the query rather than filtering a page
//! afterwards, and that a gated single read answers *absent* rather than
//! *forbidden*.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use toolkit_db::secure::DBRunner;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use super::{Declaration, DeclarationRepository};
use crate::domain::category::visibility;
use crate::domain::error::DomainError;

/// A declaration together with the trait set a client renders it by.
#[derive(Debug, Clone)]
pub struct RenderedDeclaration {
    /// The declaration itself.
    pub declaration: Declaration,
    /// The value type's effective traits, merged across its inheritance chain.
    ///
    /// An empty object when the registry could not answer for this type. A read
    /// is not failed over it: the declaration is real and its key and value type
    /// are already known, and refusing the whole page because a rendering hint
    /// is unavailable would make the catalogue unreadable whenever the registry
    /// is slow.
    pub traits: Value,
}

/// Pair a declaration with its type's traits, defaulting to an empty object.
fn pair(declaration: Declaration, traits: &HashMap<String, Value>) -> RenderedDeclaration {
    let traits = traits
        .get(&declaration.value_type_id)
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    RenderedDeclaration {
        declaration,
        traits,
    }
}

/// Reads over the declaration catalogue.
pub struct DeclarationService<R: DeclarationRepository> {
    repo: R,
    types: Arc<dyn TypesRegistryClient>,
}

impl<R: DeclarationRepository> DeclarationService<R> {
    /// Build the service over a repository and the GTS types registry.
    pub fn new(repo: R, types: Arc<dyn TypesRegistryClient>) -> Self {
        Self { repo, types }
    }

    /// Resolve the effective trait set for each distinct value type in one call.
    ///
    /// Batched deliberately: a page of fifty declarations drawn from three value
    /// types is three lookups, not fifty. `get_type_schemas` collapses
    /// duplicates and reports per-item failures, so one unknown type costs its
    /// own traits and nothing else.
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-8
    async fn traits_for(&self, declarations: &[Declaration]) -> HashMap<String, Value> {
        let mut wanted: Vec<String> = declarations
            .iter()
            .map(|d| d.value_type_id.clone())
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return HashMap::new();
        }

        self.types
            .get_type_schemas(wanted)
            .await
            .into_iter()
            .filter_map(|(id, schema)| schema.ok().map(|s| (id, s.effective_traits())))
            .collect()
    }
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-8

    /// Attach traits to a resolved set of declarations.
    async fn render(&self, declarations: Vec<Declaration>) -> Vec<RenderedDeclaration> {
        let traits = self.traits_for(&declarations).await;
        declarations
            .into_iter()
            .map(|declaration| pair(declaration, &traits))
            .collect()
    }

    /// Attach traits to one declaration.
    ///
    /// Separate from [`Self::render`] rather than a one-element call into it:
    /// taking the single element back out of a vector would be an unwrap that
    /// only the shape of this function proves safe.
    async fn render_one(&self, declaration: Declaration) -> RenderedDeclaration {
        let traits = self.traits_for(std::slice::from_ref(&declaration)).await;
        pair(declaration, &traits)
    }

    /// Fetch one declaration, or report it absent.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] when no such declaration exists **or** it falls
    /// outside the caller's administrative domain. The two are one answer by
    /// design: a distinct denial would confirm that a declaration the caller may
    /// not see exists.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<RenderedDeclaration, DomainError> {
        let visible = visibility::domain_visibility(scope);
        let found =
            self.repo
                .find(conn, scope, &visible, id)
                .await?
                .ok_or(DomainError::NotFound {
                    resource: "declaration",
                })?;
        Ok(self.render_one(found).await)
    }

    /// List declarations for the caller.
    ///
    /// # Errors
    /// [`DomainError::Validation`] when the query names an unmapped field, uses
    /// an unsupported operator, requests an unsupported option, or carries an
    /// undecodable cursor.
    pub async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<RenderedDeclaration>, DomainError> {
        crate::domain::odata::reject_unsupported_options(query, "declarations")?;
        let visible = visibility::domain_visibility(scope);
        let page = self.repo.list(conn, scope, &visible, query).await?;
        Ok(Page {
            items: self.render(page.items).await,
            page_info: page.page_info,
        })
    }
}

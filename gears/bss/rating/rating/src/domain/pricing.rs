//! The pricing binding store (DESIGN 11 §3.2 `PricingBindingStore`, §4.2; DECISIONS T-D-73 as
//! amended by T-D-87).
//!
//! Two kinds of answer share `bss_rating__pricing_binding`, each stored once per distinct answer
//! under its digest:
//!
//! - the **binding of record**: each accepted binding a fact version carries, stored by fact
//!   intake as a one-cell answer under Pricing's `selected_bindings_digest` of that cell
//!   ([`PricingBindingStore::store_accepted`]);
//! - a **freshness answer**: `PricingReadV1::resolve` with the accepted pins, asked as
//!   `bss-rating.system` in the work item's seller tenant (pricing D-523) before a child's first
//!   final, stored under the digest of its covered cells, with the question in
//!   `bss_rating__pricing_resolve` once per query and UTC day ([`PricingBindingStore::ensure`]).
//!
//! The freshness proof itself — whether an answer still names the fact's binding — is
//! [`freshness`]. Rating never prices a successor an answer names but the fact does not carry
//! (Subscriptions SUB-D-41).

pub mod convert;
pub mod freshness;

use std::sync::Arc;

use async_trait::async_trait;
use bss_pricing_sdk::digest::selected_bindings_digest;
use bss_pricing_sdk::read::{
    BindingSelection, CatalogRef, PricePin as PricingPin, PricingReadV1, ResolveQuery,
    ResolvedBindings,
};
use bss_rating_core::engine::current::input::{CoreBinding, CoreCell, PricePin};
use serde_json::{Value, json};
use time::{Date, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::authz::service_context;
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::fact::model::AcceptedTerm;
use crate::domain::ports::{Clock, Dependency, TenantGrant};

/// One distinct answer: the bindings of a plan revision, as the core reads them.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAnswer {
    /// Pricing's `selected_bindings_digest` of the covered cells.
    pub binding_digest: Sha256Hex,
    /// The plan.
    pub plan_id: Uuid,
    /// The plan revision.
    pub revision_id: Uuid,
    /// The cells; an uncovered cell has no binding and is never free.
    pub cells: Vec<CoreCell>,
    /// When it was first stored.
    pub first_fetched_at: OffsetDateTime,
}

/// What storing an answer found.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// A new answer.
    Inserted,
    /// The same digest with the same content was stored already.
    AlreadyStored,
}

/// A `resolve` question: the key a stored answer is found by before Pricing is called.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveQuestion {
    /// The catalog tenant (`CatalogRef.tenant_id`).
    pub catalog_tenant_id: Uuid,
    /// The plan revision.
    pub revision_id: Uuid,
    /// The resolve date: a slice start or an `ends_on`.
    pub date: Date,
    /// The item a question is narrowed to.
    pub item_id: Option<Uuid>,
    /// The accepted pins.
    pub pins: Vec<PricePin>,
}

impl ResolveQuestion {
    /// `query_digest`: SHA-256 over the canonical JSON `{revision_id, resolve_date, item_id?,
    /// pins sorted by (item_id, dimension_value, price_id)}` (DESIGN §3.7); the catalog tenant is
    /// the row's `tenant_id`.
    #[must_use]
    pub fn query_digest(&self) -> Sha256Hex {
        let mut pins: Vec<&PricePin> = self.pins.iter().collect();
        pins.sort_by(|a, b| {
            (a.item_id, a.dimension_value.as_deref(), a.price_id).cmp(&(
                b.item_id,
                b.dimension_value.as_deref(),
                b.price_id,
            ))
        });
        let pins: Vec<Value> = pins
            .into_iter()
            .map(|pin| {
                json!({
                    "item_id": pin.item_id,
                    "dimension_value": pin.dimension_value,
                    "price_id": pin.price_id,
                })
            })
            .collect();
        let mut question = json!({
            "revision_id": self.revision_id,
            "resolve_date": self.date.to_string(),
            "pins": pins,
        });
        if let (Some(item_id), Some(object)) = (self.item_id, question.as_object_mut()) {
            object.insert("item_id".to_owned(), json!(item_id));
        }
        Sha256Hex::of_json(&question)
    }
}

/// One stored question: the answer a query got on one UTC day.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredResolve {
    /// The question's digest.
    pub query_digest: Sha256Hex,
    /// The UTC day it was fetched; the first fetch of a day stands.
    pub fetched_on: Date,
    /// The question.
    pub question: ResolveQuestion,
    /// The answer's digest.
    pub binding_digest: Sha256Hex,
    /// When it was fetched.
    pub fetched_at: OffsetDateTime,
}

/// What a freshness `resolve` got.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveOutcome {
    /// An answer, stored or fetched.
    Answered {
        /// The question as stored.
        resolve: Box<StoredResolve>,
        /// The answer.
        answer: Box<StoredAnswer>,
    },
    /// Pricing could not answer (503 `REGISTRY_UNAVAILABLE`, `ServiceUnavailable`): retried with
    /// backoff, the child `pending(pricing_unavailable)`.
    Unavailable {
        /// What Pricing said.
        detail: String,
    },
    /// Pricing refused the question (any 4xx): `pricing_refused(<code>)`, never retried until
    /// the fact changes.
    Refused {
        /// Pricing's code (`PIN_FOREIGN`, `PERMISSION_DENIED`, `REVISION_NOT_PUBLISHED`, …).
        code: String,
    },
    /// A covered cell could not be projected into a complete binding
    /// (`INCOMPLETE_COMMERCIAL_INPUTS`, pricing D-501).
    IncompleteCommercialInputs,
}

/// `bss_rating__pricing_binding` and `bss_rating__pricing_resolve`: both insert-only.
#[async_trait]
pub trait PricingBindingRepo: Send + Sync {
    /// Stores `answer` under its digest unless the digest is stored already.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when the digest is stored with other content (the alarm of
    /// DESIGN §3.7); a storage failure.
    async fn insert_answer(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        answer: &StoredAnswer,
    ) -> Result<StoreOutcome, DomainError>;

    /// The answer stored under `binding_digest`.
    async fn find_answer(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        binding_digest: &Sha256Hex,
    ) -> Result<Option<StoredAnswer>, DomainError>;

    /// The newest stored question of `query_digest`.
    async fn newest_resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        query_digest: &Sha256Hex,
    ) -> Result<Option<StoredResolve>, DomainError>;

    /// Stores `resolve` unless a question of the same digest was stored on the same day, and
    /// returns the row that stands: this one, or the day's first fetch a concurrent writer stored.
    async fn insert_resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        resolve: &StoredResolve,
    ) -> Result<StoredResolve, DomainError>;
}

/// Where `PricingReadV1` comes from: `ClientHub` in the gear, a double in tests. Resolved at
/// each call, so a client registered after Rating's `init` is found.
pub trait PricingClient: Send + Sync {
    /// The client.
    ///
    /// # Errors
    ///
    /// [`DomainError::DependencyUnavailable`] when none is bound.
    fn client(&self) -> Result<Arc<dyn PricingReadV1>, DomainError>;
}

/// The digest of one accepted binding: Pricing's `selected_bindings_digest` of a one-cell answer
/// of `plan_id` at `revision_id`.
///
/// # Errors
///
/// The reason Pricing's digest refuses it (`INVALID_BINDING`: a `money_digest` or
/// `template_digest` that does not verify, a blank SKU code, …), or a field that does not
/// convert.
pub fn cell_digest(
    plan_id: Uuid,
    revision_id: Uuid,
    binding: &CoreBinding,
) -> Result<Sha256Hex, String> {
    let pricing = convert::to_pricing(binding).map_err(|e| e.to_string())?;
    let selection = BindingSelection {
        item_id: pricing.item_id,
        dimension_value: pricing.dimension_value.clone(),
    };
    let resolved = ResolvedBindings {
        plan_id,
        revision_id,
        cells: vec![bss_pricing_sdk::read::ResolvedCell {
            selection: selection.clone(),
            binding: Some(pricing),
        }],
    };
    let digest = selected_bindings_digest(&resolved, &[selection]).map_err(|e| refusal_code(&e))?;
    Sha256Hex::parse(&convert::digest_hex(&digest)).map_err(|e| e.to_string())
}

/// The identity of an answer, which covers every cell it stores: Pricing's
/// `selected_bindings_digest` when every cell is covered; otherwise SHA-256 of Rating's canonical
/// document `rating.answer.v1` of that digest of the covered cells and the sorted selections of the
/// uncovered ones, so two answers that differ only in what they leave uncovered are two answers.
///
/// # Errors
///
/// As [`cell_digest`].
pub fn answer_digest(answer: &ResolvedBindings) -> Result<Sha256Hex, String> {
    let (covered, uncovered): (Vec<_>, Vec<_>) =
        answer.cells.iter().partition(|cell| cell.binding.is_some());
    let selections: Vec<BindingSelection> =
        covered.iter().map(|cell| cell.selection.clone()).collect();
    let digest = selected_bindings_digest(answer, &selections).map_err(|e| refusal_code(&e))?;
    let covered_hex = convert::digest_hex(&digest);
    if uncovered.is_empty() {
        return Sha256Hex::parse(&covered_hex).map_err(|e| e.to_string());
    }
    let mut uncovered: Vec<(Uuid, Option<&str>)> = uncovered
        .iter()
        .map(|cell| {
            (
                cell.selection.item_id,
                cell.selection.dimension_value.as_deref(),
            )
        })
        .collect();
    uncovered.sort_unstable();
    Ok(Sha256Hex::of_json(&json!({
        "rating.answer.v1": {
            "plan_id": answer.plan_id,
            "revision_id": answer.revision_id,
            "covered": covered_hex,
            "uncovered": uncovered
                .into_iter()
                .map(|(item_id, dimension_value)| json!({
                    "item_id": item_id,
                    "dimension_value": dimension_value,
                }))
                .collect::<Vec<_>>(),
        }
    })))
}

/// The code of a Pricing refusal: a field violation's reason, an aborted or denied reason, a
/// failed precondition's type, else the error's category.
#[must_use]
pub fn refusal_code(error: &CanonicalError) -> String {
    let code = match error {
        CanonicalError::InvalidArgument {
            ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
            ..
        } => field_violations.first().map(|v| v.reason.clone()),
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
        CanonicalError::FailedPrecondition { ctx, .. } => {
            ctx.violations.first().map(|v| v.type_.clone())
        }
        CanonicalError::PermissionDenied { .. } => Some("PERMISSION_DENIED".to_owned()),
        CanonicalError::NotFound { .. } => Some("NOT_FOUND".to_owned()),
        _ => None,
    };
    code.filter(|code| !code.trim().is_empty())
        .unwrap_or_else(|| error.title().to_uppercase().replace(' ', "_"))
}

/// Whether a Pricing error is worth a retry: unavailable, past its deadline, or a server fault.
#[must_use]
pub const fn is_unavailable(error: &CanonicalError) -> bool {
    matches!(
        error,
        CanonicalError::ServiceUnavailable { .. }
            | CanonicalError::DeadlineExceeded { .. }
            | CanonicalError::Internal { .. }
            | CanonicalError::Unknown { .. }
    )
}

/// The pricing binding store.
#[domain_model]
#[derive(Clone)]
pub struct PricingBindingStore {
    db: Db,
    repo: Arc<dyn PricingBindingRepo>,
    pricing: Arc<dyn PricingClient>,
    grant: Arc<dyn TenantGrant>,
    clock: Arc<dyn Clock>,
}

impl PricingBindingStore {
    /// A store over `repo`, asking Pricing through `pricing`; the scope of each write is the
    /// service grant's for the work item's tenant.
    #[must_use]
    pub fn new(
        db: Db,
        repo: Arc<dyn PricingBindingRepo>,
        pricing: Arc<dyn PricingClient>,
        grant: Arc<dyn TenantGrant>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            repo,
            pricing,
            grant,
            clock,
        }
    }

    /// Stores an accepted binding a fact version carries as a one-cell answer, in the caller's
    /// transaction: the binding of record of the slices that name it (T-D-87).
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when the digest is stored with other content; a storage failure.
    pub async fn store_accepted(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        term: &AcceptedTerm,
    ) -> Result<StoreOutcome, DomainError> {
        let answer = StoredAnswer {
            binding_digest: term.binding_digest.clone(),
            plan_id: term.plan_id,
            revision_id: term.revision_id,
            cells: vec![CoreCell {
                selection: bss_rating_core::engine::current::input::CellSelection {
                    item_id: term.binding.item_id,
                    dimension_value: term.binding.dimension_value.clone(),
                },
                binding: Some(term.binding.clone()),
            }],
            first_fetched_at: self.clock.now(),
        };
        self.repo.insert_answer(tx, scope, tenant_id, &answer).await
    }

    /// The answer to `question` for work of `subject_tenant` (the fact's seller): the newest
    /// stored answer of the question, unless `fresh_from` is later than the day it was fetched;
    /// otherwise `PricingReadV1::resolve` as `bss-rating.system` in `subject_tenant`'s context
    /// (pricing D-523: Pricing refuses a catalog of another tenant), stored once per distinct
    /// answer and once per query and UTC day.
    ///
    /// # Errors
    ///
    /// [`DomainError::OutsideGrant`] or [`DomainError::AuthzUnavailable`] from the service grant;
    /// [`DomainError::DependencyUnavailable`] when no Pricing client is bound; a storage failure.
    /// A Pricing refusal or outage is an [`ResolveOutcome`], not an error.
    pub async fn ensure(
        &self,
        subject_tenant: Uuid,
        question: &ResolveQuestion,
        fresh_from: Option<Date>,
    ) -> Result<ResolveOutcome, DomainError> {
        if question.catalog_tenant_id != subject_tenant {
            // Pricing reads only the caller's own catalog (D-523), and the question's digest does
            // not name the catalog (it is the row's tenant), so a foreign catalog is refused here,
            // before any stored answer is consulted.
            tracing::warn!(
                target: "bss_rating.pricing",
                %subject_tenant,
                catalog_tenant_id = %question.catalog_tenant_id,
                "a resolve for another tenant's catalog is refused"
            );
            return Ok(ResolveOutcome::Refused {
                code: "PERMISSION_DENIED".to_owned(),
            });
        }
        let scope = self.grant.scope_for(subject_tenant).await?;
        let query_digest = question.query_digest();
        if let Some(found) = self
            .stored(&scope, subject_tenant, &query_digest, fresh_from)
            .await?
        {
            return Ok(found);
        }
        let client = self.pricing.client()?;
        let ctx = service_context(subject_tenant)?;
        let query = ResolveQuery {
            catalog: CatalogRef {
                tenant_id: question.catalog_tenant_id,
            },
            revision_id: question.revision_id,
            date: question.date,
            item_id: question.item_id,
            pins: question
                .pins
                .iter()
                .map(|pin| PricingPin {
                    item_id: pin.item_id,
                    dimension_value: pin.dimension_value.clone(),
                    price_id: pin.price_id,
                })
                .collect(),
        };
        let answer = match client.resolve(&ctx, query).await {
            Ok(answer) => answer,
            Err(error) => return Ok(Self::failure(&error)),
        };
        let binding_digest = answer_digest(&answer).map_err(|detail| {
            DomainError::Internal(
                format!("pricing answered an invalid binding: {detail}"),
                None,
            )
        })?;
        let now = self.clock.now();
        let stored = StoredAnswer {
            binding_digest: binding_digest.clone(),
            plan_id: answer.plan_id,
            revision_id: answer.revision_id,
            cells: answer.cells.iter().map(convert::cell_to_core).collect(),
            first_fetched_at: now,
        };
        let resolve = StoredResolve {
            query_digest,
            fetched_on: now.date(),
            question: question.clone(),
            binding_digest,
            fetched_at: now,
        };
        let (resolve, answer) = self
            .persist(&scope, subject_tenant, &stored, &resolve)
            .await?;
        Ok(ResolveOutcome::Answered {
            resolve: Box::new(resolve),
            answer: Box::new(answer),
        })
    }

    async fn stored(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query_digest: &Sha256Hex,
        fresh_from: Option<Date>,
    ) -> Result<Option<ResolveOutcome>, DomainError> {
        let (repo, scope, digest) = (Arc::clone(&self.repo), scope.clone(), query_digest.clone());
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let Some(resolve) = repo.newest_resolve(tx, &scope, tenant_id, &digest).await?
                    else {
                        return Ok(None);
                    };
                    if fresh_from.is_some_and(|from| resolve.fetched_on < from) {
                        return Ok(None);
                    }
                    let answer = repo
                        .find_answer(tx, &scope, tenant_id, &resolve.binding_digest)
                        .await?
                        .ok_or_else(|| {
                            DomainError::CorruptRow(
                                "bss_rating__pricing_resolve.binding_digest".to_owned(),
                            )
                        })?;
                    Ok(Some(ResolveOutcome::Answered {
                        resolve: Box::new(resolve),
                        answer: Box::new(answer),
                    }))
                })
            })
            .await
    }

    /// Stores a fetched answer and its question, and returns the question and answer that stand:
    /// the first fetch of a day stands, so a concurrent fetch that lost the question's insert
    /// returns the winner's, never an answer no stored question names.
    async fn persist(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        answer: &StoredAnswer,
        resolve: &StoredResolve,
    ) -> Result<(StoredResolve, StoredAnswer), DomainError> {
        let (repo, scope, answer, resolve) = (
            Arc::clone(&self.repo),
            scope.clone(),
            answer.clone(),
            resolve.clone(),
        );
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    repo.insert_answer(tx, &scope, tenant_id, &answer).await?;
                    let standing = repo.insert_resolve(tx, &scope, tenant_id, &resolve).await?;
                    if standing.binding_digest == answer.binding_digest {
                        return Ok((standing, answer));
                    }
                    let winner = repo
                        .find_answer(tx, &scope, tenant_id, &standing.binding_digest)
                        .await?
                        .ok_or_else(|| {
                            DomainError::CorruptRow(
                                "bss_rating__pricing_resolve.binding_digest".to_owned(),
                            )
                        })?;
                    Ok((standing, winner))
                })
            })
            .await
    }

    fn failure(error: &CanonicalError) -> ResolveOutcome {
        if is_unavailable(error) {
            tracing::warn!(
                target: "bss_rating.pricing",
                dependency = Dependency::Pricing.as_str(),
                error = %error,
                "pricing could not answer a resolve"
            );
            return ResolveOutcome::Unavailable {
                detail: error.detail().to_owned(),
            };
        }
        let code = refusal_code(error);
        if code == "INCOMPLETE_COMMERCIAL_INPUTS" {
            return ResolveOutcome::IncompleteCommercialInputs;
        }
        ResolveOutcome::Refused { code }
    }
}

#[cfg(test)]
#[path = "pricing_tests.rs"]
mod tests;

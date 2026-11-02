//! Products' derived usage declarations (DESIGN 11 §3.2 `DerivedDeclarationStore`; T-D-76, R-31).
//!
//! A usage binding names its derived meter (`products.derived/<code>@<n>`); its declaration —
//! inputs, folds, formula — is Products'. Products offers no typed read yet (R-31), so Rating
//! reads it through its own port, [`DerivedDeclarationRead`], resolved at each call (a double in
//! tests), and keeps the copy it rated with in `bss_rating__derived_declaration`, verbatim with its
//! digest: SHA-256 of Products' canonical bytes. The same meter stored with another digest is an
//! alarm, never an overwrite.
//!
//! An unreadable declaration is not a refusal of the fact: the fact is stored and waits
//! `pending(derived_declaration_unavailable)` (feature 06 §2.1 step 7.1).

use std::sync::Arc;

use async_trait::async_trait;
use bss_products_sdk::derived::{
    self, DerivedUsageDeclaration, Expr, Granularity as ProductsGranularity,
    GranuleFold as ProductsFold, MeterId, RoundMode as ProductsRound,
};
use bss_rating_core::engine::current::input::{
    DerivedDeclaration, DerivedExpr, DerivedInput, Granularity, GranuleFold, RoundMode,
};
use time::OffsetDateTime;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::authz::service_context;
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::ports::{Clock, TenantGrant};

/// The derived-declaration read (R-31).
#[async_trait]
pub trait DerivedDeclarationRead: Send + Sync {
    /// The declaration of `meter` in the caller's tenant, or `None` when Products answers that it
    /// is not declared.
    ///
    /// # Errors
    ///
    /// [`DomainError::DependencyUnavailable`] when Products cannot answer.
    async fn read(
        &self,
        ctx: &SecurityContext,
        meter: &MeterId,
    ) -> Result<Option<DerivedUsageDeclaration>, DomainError>;
}

/// Where the read comes from: `ClientHub` in the gear, a double in tests. Resolved at each call.
pub trait DeclarationSource: Send + Sync {
    /// The read.
    ///
    /// # Errors
    ///
    /// [`DomainError::DependencyUnavailable`] when none is bound.
    fn reader(&self) -> Result<Arc<dyn DerivedDeclarationRead>, DomainError>;
}

/// A declaration as Rating keeps it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDeclaration {
    /// The derived usage type code.
    pub code: String,
    /// Its version.
    pub version: u32,
    /// SHA-256 of Products' canonical declaration bytes.
    pub digest: Sha256Hex,
    /// The declaration, as the core reads it.
    pub declaration: DerivedDeclaration,
    /// When it was first read.
    pub fetched_at: OffsetDateTime,
}

impl StoredDeclaration {
    /// The raw GTS usage types its inputs read.
    #[must_use]
    pub fn input_types(&self) -> Vec<String> {
        self.declaration
            .inputs
            .iter()
            .map(|input| input.usage_type_ref.clone())
            .collect()
    }
}

/// What reading a declaration found.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationOutcome {
    /// Stored, or read and stored now.
    Stored(StoredDeclaration),
    /// Products could not answer, or does not know the meter yet: retried.
    Unavailable {
        /// Why.
        detail: String,
    },
    /// The meter id, or the declaration Products answered, cannot be rated, or it differs from
    /// the stored copy: the fact fails `derived_declaration_conflict`.
    Conflict {
        /// Why.
        detail: String,
    },
}

/// `bss_rating__derived_declaration`: insert-only.
#[async_trait]
pub trait DeclarationRepo: Send + Sync {
    /// The stored copy of `(code, version)`.
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        code: &str,
        version: u32,
    ) -> Result<Option<StoredDeclaration>, DomainError>;

    /// Stores `declaration` unless `(code, version)` is stored; returns the stored copy.
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        declaration: &StoredDeclaration,
    ) -> Result<StoredDeclaration, DomainError>;
}

/// The derived-declaration store.
#[domain_model]
#[derive(Clone)]
pub struct DerivedDeclarationStore {
    db: Db,
    repo: Arc<dyn DeclarationRepo>,
    source: Arc<dyn DeclarationSource>,
    grant: Arc<dyn TenantGrant>,
    clock: Arc<dyn Clock>,
}

impl DerivedDeclarationStore {
    /// A store over `repo`, reading Products through `source`.
    #[must_use]
    pub fn new(
        db: Db,
        repo: Arc<dyn DeclarationRepo>,
        source: Arc<dyn DeclarationSource>,
        grant: Arc<dyn TenantGrant>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            repo,
            source,
            grant,
            clock,
        }
    }

    /// The declaration of `meter` for `tenant_id`: the stored copy, else Products' answer, checked
    /// and stored. Runs outside any caller transaction (a remote read).
    ///
    /// # Errors
    ///
    /// [`DomainError::OutsideGrant`] or [`DomainError::AuthzUnavailable`] from the service grant;
    /// a storage failure. Products being unable to answer is an outcome, not an error.
    pub async fn ensure(
        &self,
        tenant_id: Uuid,
        meter: &str,
    ) -> Result<DeclarationOutcome, DomainError> {
        let meter_id = match MeterId::parse(meter) {
            Ok(meter_id) => meter_id,
            Err(error) => {
                return Ok(DeclarationOutcome::Conflict {
                    detail: format!("`{meter}` is not a derived meter id: {error:?}"),
                });
            }
        };
        let scope = self.grant.scope_for(tenant_id).await?;
        let (code, version) = (meter_id.code().to_owned(), meter_id.version());
        let (repo, scan) = (Arc::clone(&self.repo), scope.clone());
        let stored = {
            let code = code.clone();
            self.db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move { repo.find(tx, &scan, tenant_id, &code, version).await })
                })
                .await?
        };
        if let Some(stored) = stored {
            return Ok(DeclarationOutcome::Stored(stored));
        }
        let read = match self.source.reader() {
            Ok(reader) => {
                let ctx = service_context(tenant_id)?;
                reader.read(&ctx, &meter_id).await
            }
            Err(error) => Err(error),
        };
        let declaration = match read {
            Ok(Some(declaration)) => declaration,
            Ok(None) => {
                return Ok(DeclarationOutcome::Unavailable {
                    detail: format!("products does not declare `{meter}` yet"),
                });
            }
            Err(error) => {
                return Ok(DeclarationOutcome::Unavailable {
                    detail: error.to_string(),
                });
            }
        };
        if let Err(error) = derived::validate(&declaration) {
            return Ok(DeclarationOutcome::Conflict {
                detail: format!("products declares `{meter}` invalidly: {error}"),
            });
        }
        let fresh = StoredDeclaration {
            code,
            version,
            digest: digest(&declaration),
            declaration: to_core(&declaration),
            fetched_at: self.clock.now(),
        };
        let (repo, insert) = (Arc::clone(&self.repo), fresh.clone());
        let stored = self
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { repo.insert(tx, &scope, tenant_id, &insert).await })
            })
            .await?;
        if stored.digest != fresh.digest {
            tracing::error!(
                target: "bss_rating.declarations",
                %tenant_id,
                meter,
                stored = stored.digest.as_str(),
                read = fresh.digest.as_str(),
                "a derived declaration changed under one version"
            );
            return Ok(DeclarationOutcome::Conflict {
                detail: format!("`{meter}` is stored with another digest"),
            });
        }
        Ok(DeclarationOutcome::Stored(stored))
    }
}

/// SHA-256 of Products' canonical declaration bytes.
#[must_use]
pub fn digest(declaration: &DerivedUsageDeclaration) -> Sha256Hex {
    Sha256Hex::of_bytes(&derived::canonical_bytes(declaration))
}

/// Products' declaration as the core reads it.
#[must_use]
pub fn to_core(declaration: &DerivedUsageDeclaration) -> DerivedDeclaration {
    DerivedDeclaration {
        output_unit: declaration.output_unit.clone(),
        granularity: match declaration.granularity {
            ProductsGranularity::Hour => Granularity::Hour,
        },
        inputs: declaration
            .inputs
            .iter()
            .map(|input| DerivedInput {
                name: input.name.clone(),
                usage_type_ref: input.usage_type_ref.clone(),
                granule_fold: match input.granule_fold {
                    ProductsFold::Sum => GranuleFold::Sum,
                    ProductsFold::Peak => GranuleFold::Peak,
                    ProductsFold::TimeWeighted => GranuleFold::TimeWeighted,
                },
                max_hold_seconds: input.max_hold_seconds,
                unit: input.unit.clone(),
            })
            .collect(),
        formula: expr(&declaration.formula),
        output_scale: declaration.output_scale,
        output_round: round(declaration.output_round),
    }
}

const fn round(mode: ProductsRound) -> RoundMode {
    match mode {
        ProductsRound::HalfEven => RoundMode::HalfEven,
        ProductsRound::HalfUp => RoundMode::HalfUp,
        ProductsRound::Up => RoundMode::Up,
        ProductsRound::Down => RoundMode::Down,
    }
}

fn expr(formula: &Expr) -> DerivedExpr {
    let boxed = |operand: &Expr| Box::new(expr(operand));
    match formula {
        Expr::Input(name) => DerivedExpr::Input { name: name.clone() },
        Expr::Const(value) => DerivedExpr::Const { value: *value },
        Expr::Add(left, right) => DerivedExpr::Add {
            left: boxed(left),
            right: boxed(right),
        },
        Expr::Sub(left, right) => DerivedExpr::Sub {
            left: boxed(left),
            right: boxed(right),
        },
        Expr::Mul(left, right) => DerivedExpr::Mul {
            left: boxed(left),
            right: boxed(right),
        },
        Expr::DivConst(operand, divisor) => DerivedExpr::DivConst {
            operand: boxed(operand),
            divisor: *divisor,
        },
        Expr::Max(operands) => DerivedExpr::Max {
            operands: operands.iter().map(expr).collect(),
        },
        Expr::Min(operands) => DerivedExpr::Min {
            operands: operands.iter().map(expr).collect(),
        },
        Expr::Ceil(operand) => DerivedExpr::Ceil {
            operand: boxed(operand),
        },
        Expr::Floor(operand) => DerivedExpr::Floor {
            operand: boxed(operand),
        },
        Expr::Round(operand, scale, mode) => DerivedExpr::Round {
            operand: boxed(operand),
            scale: *scale,
            mode: round(*mode),
        },
    }
}

#[cfg(test)]
#[path = "declaration_tests.rs"]
mod tests;

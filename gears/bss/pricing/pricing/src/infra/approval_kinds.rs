//! The approval kinds pricing records and the subject each one is judged by (spec §6).
//!
//! Every door that acts on a stored unit dispatches on its `kind`: the subject that collects,
//! locks, applies and unlocks it, the domain event its apply announces and the impact its card
//! shows. A kind pricing does not record (promotions are deferred, D-409; migration requests,
//! D-410) is a corrupt row, never judged as another kind.
use super::{
    plan_revisions::{self, KIND_PLAN_REVISION, PlanRevisionSubject},
    prices::{KIND_PRICES, PricesSubject},
    storage::RepoError,
};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};
use serde_json::Value;
use time::Date;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{DbTx, secure::DBRunner};
use uuid::Uuid;

/// A kind of approval unit pricing records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A batch of draft prices of one book.
    Prices,
    /// One draft plan revision.
    PlanRevision,
}
impl Kind {
    /// Every kind, in the order the policy lists them.
    pub const ALL: [Self; 2] = [Self::Prices, Self::PlanRevision];
    /// The stored kind name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prices => KIND_PRICES,
            Self::PlanRevision => KIND_PLAN_REVISION,
        }
    }
    /// A kind by its stored name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
    /// The kind of a stored unit.
    /// # Errors
    /// An unknown kind is a corrupt row: it is never judged as another kind.
    pub fn of(unit: &Unit) -> Result<Self, RepoError> {
        Self::parse(&unit.kind).ok_or_else(|| {
            RepoError::CorruptRow(format!(
                "approval unit {} has unknown kind {}",
                unit.id, unit.kind
            ))
        })
    }
    /// The live impact every read of a unit of this kind shows, recomputed from its items.
    /// # Errors
    /// Storage failures.
    pub async fn impact(
        self,
        tx: &impl DBRunner,
        tenant: Uuid,
        items: &[ItemRef],
    ) -> Result<Value, RepoError> {
        match self {
            Self::Prices => super::prices::live_impact(tx, tenant, items).await,
            Self::PlanRevision => Ok(plan_revisions::impact()),
        }
    }
}

impl Kind {
    /// [`Kind::impact`] from a reading of the plans loaded once for a page of units (D-458).
    #[must_use]
    pub fn impact_from(self, reading: &super::prices::PlansReading, items: &[ItemRef]) -> Value {
        match self {
            Self::Prices => super::prices::impact_from(reading, items),
            Self::PlanRevision => plan_revisions::impact(),
        }
    }
}

/// The subject one unit is judged by, chosen by its kind.
#[derive(Clone)]
pub enum Subject {
    Prices(PricesSubject),
    PlanRevision(PlanRevisionSubject),
}
impl Subject {
    /// The Products refusal that ended the last judgement, if any; the door answers it as is.
    #[must_use]
    pub fn take_refusal(&self) -> Option<CanonicalError> {
        match self {
            Self::Prices(s) => s.take_refusal(),
            Self::PlanRevision(s) => s.take_refusal(),
        }
    }
    /// The kind this subject judges.
    #[must_use]
    pub const fn kind_of(&self) -> Kind {
        match self {
            Self::Prices(_) => Kind::Prices,
            Self::PlanRevision(_) => Kind::PlanRevision,
        }
    }
}

#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for Subject {
    fn kind(&self) -> &'static str {
        match self {
            Self::Prices(s) => ApprovalSubject::<DbTx<'a>>::kind(s),
            Self::PlanRevision(s) => ApprovalSubject::<DbTx<'a>>::kind(s),
        }
    }
    fn ref_type(&self) -> &'static str {
        match self {
            Self::Prices(s) => ApprovalSubject::<DbTx<'a>>::ref_type(s),
            Self::PlanRevision(s) => ApprovalSubject::<DbTx<'a>>::ref_type(s),
        }
    }
    async fn collect(&self, tx: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        match self {
            Self::Prices(s) => s.collect(tx, ids).await,
            Self::PlanRevision(s) => s.collect(tx, ids).await,
        }
    }
    async fn validate_submit(&self, tx: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        match self {
            Self::Prices(s) => s.validate_submit(tx, items).await,
            Self::PlanRevision(s) => s.validate_submit(tx, items).await,
        }
    }
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit_id: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Prices(s) => s.lock(tx, unit_id, items).await,
            Self::PlanRevision(s) => s.lock(tx, unit_id, items).await,
        }
    }
    fn snapshot(&self, items: &[ItemRef], common_effective_date: Option<Date>) -> Value {
        match self {
            Self::Prices(s) => {
                ApprovalSubject::<DbTx<'a>>::snapshot(s, items, common_effective_date)
            }
            Self::PlanRevision(s) => {
                ApprovalSubject::<DbTx<'a>>::snapshot(s, items, common_effective_date)
            }
        }
    }
    async fn apply(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Prices(s) => s.apply(tx, unit, items).await,
            Self::PlanRevision(s) => s.apply(tx, unit, items).await,
        }
    }
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Prices(s) => s.unlock(tx, unit, items, approved).await,
            Self::PlanRevision(s) => s.unlock(tx, unit, items, approved).await,
        }
    }
}

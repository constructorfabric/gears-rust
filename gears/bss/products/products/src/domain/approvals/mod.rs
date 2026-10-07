//! Approval kinds and immutable proposal content shared by the subjects.
// The subjects apply through the repositories, the broker and the outbox.
#![allow(unknown_lints, de0301_no_infra_in_domain)]

use bss_products_sdk::models::{Lifecycle, SkuContent};
/// Publish a draft SKU.
pub const KIND_SKU_PUBLISH: &str = "sku_publish";
/// Change published business content and optionally lifecycle.
pub const KIND_SKU_CHANGE: &str = "sku_change";
/// Retire a fenced SKU.
pub const KIND_SKU_RETIRE: &str = "sku_retire";
/// A kind of approval unit products records (P-D-227). No CHECK holds the stored column: the
/// repository reads it through this set, so a unit of another kind is a corrupt row, never
/// judged or served as one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::enum_variant_names,
    reason = "the variants name the stored kinds sku_publish, sku_change and sku_retire, and the \
              wire set maps from them one to one (`ProductsApprovalKind`)"
)]
pub enum ApprovalKind {
    SkuPublish,
    SkuChange,
    SkuRetire,
}
impl ApprovalKind {
    /// Every kind, in the order the counts name them.
    pub const ALL: [Self; 3] = [Self::SkuPublish, Self::SkuChange, Self::SkuRetire];
    /// The stored kind name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkuPublish => KIND_SKU_PUBLISH,
            Self::SkuChange => KIND_SKU_CHANGE,
            Self::SkuRetire => KIND_SKU_RETIRE,
        }
    }
    /// A kind by its stored name; `None` for a kind products does not record.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}
/// Missing policy rows still require review (P-D-190).
pub const DEFAULT_QUORUM: u32 = 1;
/// The longest submitter's note the submit, change and retire doors take, in characters (Unicode
/// scalar values), as a book's description is counted in pricing (P-D-219).
pub const NOTE_MAX_CHARS: usize = 2000;

/// Records `NOTE_TOO_LONG` on `note` when the submitter's note is longer than [`NOTE_MAX_CHARS`].
/// A note is stored as sent: it is not trimmed, and a blank one is kept.
pub fn check_note(note: Option<&str>, report: &mut crate::domain::validation::ValidationReport) {
    if note.is_some_and(|n| n.chars().count() > NOTE_MAX_CHARS) {
        report.violate(
            "NOTE_TOO_LONG",
            "note",
            format!("a note is at most {NOTE_MAX_CHARS} characters"),
        );
    }
}
/// The content and requested lifecycle that reviewers decide together.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkuProposal {
    pub content: SkuContent,
    pub lifecycle: Option<Lifecycle>,
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod approvals_tests;

pub(crate) mod change;
pub(crate) mod publish;
// Integration probes use the real subject; consumers use the SDK.
#[doc(hidden)]
pub use publish::SkuPublish;
pub(crate) mod retire;

use crate::infra::storage::{RepoError, RepoRefusal, repo};
use bss_approval::ApprovalError;
use toolkit_db::{DbTx, secure::AccessScope};
use uuid::Uuid;

/// Keep contention errors typed all the way to the transaction boundary. A repository refusal an
/// apply can meet is the apply's refusal with its code; the rest cannot follow from an apply's
/// writes and stay a store failure, with the refusal named (RS-16: an exhaustive match, not a
/// string list).
pub(crate) fn store_err(error: RepoError) -> ApprovalError {
    match error {
        RepoError::Driver { source, .. } => ApprovalError::Db(source),
        RepoError::Refused(
            refusal @ (RepoRefusal::SkuNameTaken
            | RepoRefusal::SkuCodeTaken
            | RepoRefusal::CategoryRetired
            | RepoRefusal::CategoryNotFound
            | RepoRefusal::VersionOrder),
        ) => ApprovalError::ApplyRefused {
            code: refusal.code(),
            detail: error.to_string(),
        },
        RepoError::Refused(
            RepoRefusal::CategoryCodeTaken
            | RepoRefusal::CategoryDefaultTaken
            | RepoRefusal::ReferenceExists
            | RepoRefusal::DerivedCodeTaken
            | RepoRefusal::DerivedVersionTaken,
        )
        | RepoError::Db(_)
        | RepoError::CorruptRow(_) => ApprovalError::Store(error.to_string()),
    }
}
/// The refusal of a category a SKU's content names (P-D-196), with the draft doors' answers: a
/// category the tenant does not hold is `CATEGORY_NOT_FOUND` with the id as its detail, which
/// answers 404 (`DomainError::from`); a retired one is `CATEGORY_RETIRED`, 409.
async fn require_category(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(), ApprovalError> {
    repo::category_repo::require_active_category(tx, scope, tenant, id)
        .await
        .map_err(|error| match error {
            RepoError::Refused(RepoRefusal::CategoryNotFound) => ApprovalError::ApplyRefused {
                code: "CATEGORY_NOT_FOUND",
                detail: id.to_string(),
            },
            other => store_err(other),
        })
}
fn invalid(code: &'static str, field: &str, detail: impl Into<String>) -> ApprovalError {
    ApprovalError::InvalidSubmit {
        code,
        field: field.into(),
        detail: detail.into(),
    }
}
fn apply_error(e: ApprovalError) -> ApprovalError {
    match e {
        ApprovalError::InvalidSubmit { code, detail, .. } => {
            ApprovalError::ApplyRefused { code, detail }
        }
        other => other,
    }
}
fn json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, ApprovalError> {
    serde_json::to_value(v).map_err(|e| ApprovalError::Store(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &serde_json::Value) -> Result<T, ApprovalError> {
    serde_json::from_value(v.clone()).map_err(|e| ApprovalError::Store(e.to_string()))
}
async fn sku(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<bss_products_sdk::models::Sku, ApprovalError> {
    repo::find_sku(tx, scope, tenant, id)
        .await
        .map_err(store_err)?
        .ok_or_else(|| invalid("NOT_FOUND", "id", id.to_string()))
}

/// The products kinds share one engine boundary without erasing the transaction runner.
#[toolkit_macros::domain_model]
#[derive(Clone)]
pub(crate) enum Subject {
    Publish(publish::SkuPublish),
    Change(change::SkuChange),
    Retire(retire::SkuRetire),
}
#[async_trait::async_trait]
impl<'a> bss_approval::ApprovalSubject<DbTx<'a>> for Subject {
    fn kind(&self) -> &'static str {
        match self {
            Self::Publish(s) => s.kind(),
            Self::Change(s) => s.kind(),
            Self::Retire(s) => s.kind(),
        }
    }
    fn ref_type(&self) -> &'static str {
        match self {
            Self::Publish(s) => s.ref_type(),
            Self::Change(s) => s.ref_type(),
            Self::Retire(s) => s.ref_type(),
        }
    }
    async fn collect(
        &self,
        tx: &DbTx<'a>,
        ids: &[Uuid],
    ) -> Result<Vec<bss_approval::ItemRef>, ApprovalError> {
        match self {
            Self::Publish(s) => s.collect(tx, ids).await,
            Self::Change(s) => s.collect(tx, ids).await,
            Self::Retire(s) => s.collect(tx, ids).await,
        }
    }
    async fn validate_submit(
        &self,
        tx: &DbTx<'a>,
        items: &[bss_approval::ItemRef],
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Publish(s) => s.validate_submit(tx, items).await,
            Self::Change(s) => s.validate_submit(tx, items).await,
            Self::Retire(s) => s.validate_submit(tx, items).await,
        }
    }
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit: Uuid,
        items: &[bss_approval::ItemRef],
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Publish(s) => s.lock(tx, unit, items).await,
            Self::Change(s) => s.lock(tx, unit, items).await,
            Self::Retire(s) => s.lock(tx, unit, items).await,
        }
    }
    fn snapshot(
        &self,
        items: &[bss_approval::ItemRef],
        date: Option<time::Date>,
    ) -> serde_json::Value {
        match self {
            Self::Publish(s) => s.snapshot(items, date),
            Self::Change(s) => s.snapshot(items, date),
            Self::Retire(s) => s.snapshot(items, date),
        }
    }
    async fn apply(
        &self,
        tx: &DbTx<'a>,
        unit: &bss_approval::Unit,
        items: &[bss_approval::ItemRef],
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Publish(s) => s.apply(tx, unit, items).await,
            Self::Change(s) => s.apply(tx, unit, items).await,
            Self::Retire(s) => s.apply(tx, unit, items).await,
        }
    }
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        unit: &bss_approval::Unit,
        items: &[bss_approval::ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        match self {
            Self::Publish(s) => s.unlock(tx, unit, items, approved).await,
            Self::Change(s) => s.unlock(tx, unit, items, approved).await,
            Self::Retire(s) => s.unlock(tx, unit, items, approved).await,
        }
    }
}

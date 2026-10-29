//! The complete authorized gate report of a refused submit or amendment (DESIGN *Canonical wire
//! contract*, "For an all-failures response, retain the complete authorized report in
//! `context.data`"; D-229). It arrives as the optional
//! [`RefusalDiagnostics::assessment`](crate::errors::RefusalDiagnostics::assessment) member; the
//! primary blocking result is the outer refusal itself.
//!
//! Every field is a public token, never producer results or upstream evidence. `predicate`,
//! `verdict` and `applicability` are open strings: a newer Orders may add a predicate or a token
//! without breaking this decoder. `reason` is the closed [`Reason`] registry (D-229 item 3), so a
//! reason token this SDK does not know fails to decode the whole member.
//!
//! Consumers decode these types; they do not build them. The types are `#[non_exhaustive]` and
//! the constructors exist for the producing gear.
use crate::catalog::Reason;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The complete authorized gate report of a refused submit/amendment (DESIGN L5093; D-229).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GateReport {
    /// The stored gate assessment this report projects.
    pub assessment_id: Uuid,
    /// The versioned predicate → reason/applicability mapping the assessment ran under (D-195).
    pub mapping_version: String,
    /// Every outcome, passed ones included, in declared predicate order.
    pub outcomes: Vec<GateOutcome>,
    /// Every non-passed `(line, reason)` once, in outcome order.
    pub failures: Vec<GateFailure>,
}

impl GateReport {
    /// The report of `assessment_id` under `mapping_version` (the producing gear's constructor).
    #[must_use]
    pub fn new(
        assessment_id: Uuid,
        mapping_version: impl Into<String>,
        outcomes: Vec<GateOutcome>,
        failures: Vec<GateFailure>,
    ) -> Self {
        Self {
            assessment_id,
            mapping_version: mapping_version.into(),
            outcomes,
            failures,
        }
    }
}

/// Whether an outcome is scoped to a catalog selection, and with which key (DESIGN L6527-6538:
/// a selected NULL and the literal `"default"` are different identities).
///
/// On every wire and in the database it is the two members `has_catalog_selection` and
/// `catalog_scope_key`; the enum makes their one invalid combination (no selection but a key)
/// unrepresentable. The variant order is the order of the `(bool, Option<String>)` pair it
/// replaces, `(false, None) < (true, None) < (true, Some(k))`: frozen outcome vectors are sorted
/// by it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogSelection {
    /// The outcome is not scoped to a catalog selection.
    NotSelected,
    /// Scoped to a selection whose catalog scope key is NULL.
    SelectedNull,
    /// Scoped to a selection with this catalog scope key.
    Selected(String),
}

/// A catalog scope key without a catalog selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a catalog scope key requires a catalog selection")]
pub struct SelectionError;

impl CatalogSelection {
    /// The selection the two wire members name.
    ///
    /// # Errors
    /// [`SelectionError`] when `has` is `false` but a key is present.
    pub fn from_parts(has: bool, key: Option<String>) -> Result<Self, SelectionError> {
        match (has, key) {
            (false, None) => Ok(Self::NotSelected),
            (false, Some(_)) => Err(SelectionError),
            (true, None) => Ok(Self::SelectedNull),
            (true, Some(key)) => Ok(Self::Selected(key)),
        }
    }

    /// The `has_catalog_selection` member: whether a selection is present (a selected NULL
    /// counts).
    #[must_use]
    pub const fn has_catalog_selection(&self) -> bool {
        !matches!(self, Self::NotSelected)
    }

    /// The `catalog_scope_key` member: the raw dimension value, `None` for no selection or a
    /// selected NULL.
    #[must_use]
    pub fn catalog_scope_key(&self) -> Option<&str> {
        match self {
            Self::Selected(key) => Some(key),
            Self::NotSelected | Self::SelectedNull => None,
        }
    }

    /// The two wire members, by value.
    #[must_use]
    pub fn into_parts(self) -> (bool, Option<String>) {
        match self {
            Self::NotSelected => (false, None),
            Self::SelectedNull => (true, None),
            Self::Selected(key) => (true, Some(key)),
        }
    }
}

/// One public outcome (the gear's vector row as tokens; never producer evidence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GateOutcomeWire", into = "GateOutcomeWire")]
#[non_exhaustive]
pub struct GateOutcome {
    /// The predicate token, e.g. `catalog.revision-available`.
    pub predicate: String,
    /// The line, absent for an order-scoped predicate.
    pub line_id: Option<Uuid>,
    /// The catalog item, absent when the outcome is not item-scoped.
    pub item_id: Option<Uuid>,
    /// The catalog selection; on the wire the `has_catalog_selection` and
    /// `catalog_scope_key` members (the key is absent for no selection or a selected NULL).
    pub selection: CatalogSelection,
    /// The verdict token: `passed`, `failed` or `unevaluable`.
    pub verdict: String,
    /// The registered reason, present when the outcome did not pass.
    pub reason: Option<Reason>,
    /// The applicability token: `required`, `tcv-only` or `preview-forecast`.
    pub applicability: String,
}

/// The wire form of [`GateOutcome`]: the selection as its two flat members.
#[derive(Serialize, Deserialize)]
struct GateOutcomeWire {
    predicate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_id: Option<Uuid>,
    has_catalog_selection: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog_scope_key: Option<String>,
    verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<Reason>,
    applicability: String,
}

impl TryFrom<GateOutcomeWire> for GateOutcome {
    type Error = SelectionError;
    fn try_from(w: GateOutcomeWire) -> Result<Self, Self::Error> {
        Ok(Self {
            predicate: w.predicate,
            line_id: w.line_id,
            item_id: w.item_id,
            selection: CatalogSelection::from_parts(w.has_catalog_selection, w.catalog_scope_key)?,
            verdict: w.verdict,
            reason: w.reason,
            applicability: w.applicability,
        })
    }
}

impl From<GateOutcome> for GateOutcomeWire {
    fn from(o: GateOutcome) -> Self {
        let (has_catalog_selection, catalog_scope_key) = o.selection.into_parts();
        Self {
            predicate: o.predicate,
            line_id: o.line_id,
            item_id: o.item_id,
            has_catalog_selection,
            catalog_scope_key,
            verdict: o.verdict,
            reason: o.reason,
            applicability: o.applicability,
        }
    }
}

impl GateOutcome {
    /// An order-scoped outcome without a reason (the producing gear's constructor); scope it
    /// with [`Self::with_line`], [`Self::with_item`] and
    /// [`Self::with_selection`], and give a non-passed outcome its reason with
    /// [`Self::with_reason`].
    #[must_use]
    pub fn new(
        predicate: impl Into<String>,
        verdict: impl Into<String>,
        applicability: impl Into<String>,
    ) -> Self {
        Self {
            predicate: predicate.into(),
            line_id: None,
            item_id: None,
            selection: CatalogSelection::NotSelected,
            verdict: verdict.into(),
            reason: None,
            applicability: applicability.into(),
        }
    }

    /// Scope the outcome to a line.
    #[must_use]
    pub const fn with_line(mut self, line_id: Uuid) -> Self {
        self.line_id = Some(line_id);
        self
    }

    /// Scope the outcome to a selected item.
    #[must_use]
    pub const fn with_item(mut self, item_id: Uuid) -> Self {
        self.item_id = Some(item_id);
        self
    }

    /// Record the outcome's catalog selection (selected NULL or a scope key).
    #[must_use]
    pub fn with_selection(mut self, selection: CatalogSelection) -> Self {
        self.selection = selection;
        self
    }

    /// The `has_catalog_selection` wire member (see [`CatalogSelection::has_catalog_selection`]).
    #[must_use]
    pub const fn has_catalog_selection(&self) -> bool {
        self.selection.has_catalog_selection()
    }

    /// The `catalog_scope_key` wire member (see [`CatalogSelection::catalog_scope_key`]).
    #[must_use]
    pub fn catalog_scope_key(&self) -> Option<&str> {
        self.selection.catalog_scope_key()
    }

    /// The outcome's reason.
    #[must_use]
    pub fn with_reason(mut self, reason: Option<Reason>) -> Self {
        self.reason = reason;
        self
    }
}

/// One entry of the failure list: a non-passed `(line, reason)` and the first predicate that
/// reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GateFailure {
    /// The line, absent for an order-level failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_id: Option<Uuid>,
    /// The registered reason.
    pub reason: Reason,
    /// The first predicate token in declared order that reported it.
    pub predicate: String,
}

impl GateFailure {
    /// The failure entry (the producing gear's constructor).
    #[must_use]
    pub fn new(line_id: Option<Uuid>, reason: Reason, predicate: impl Into<String>) -> Self {
        Self {
            line_id,
            reason,
            predicate: predicate.into(),
        }
    }
}

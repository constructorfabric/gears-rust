//! The `$filter` fields of the public Quota list.
//!
//! Each field lowers onto one slot of [`crate::QuotaFilter`], so every field
//! serves `eq` and only `id`, the one multi-valued slot, also serves `in`. The
//! parser and the published `x-odata-filter` both read [`FilterField::published_ops`],
//! so what a caller reads in the contract is what the endpoint accepts.

use toolkit_odata::filter::{FieldKind, FilterField, FilterOp};

/// A filterable field of `GET /quotas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaFilterField {
    /// The Quota id.
    Id,
    /// The owning tenant.
    TenantId,
    /// The subject type; comes with `subject_id`.
    SubjectType,
    /// The subject identifier; comes with `subject_type`.
    SubjectId,
    /// The metric type id.
    Metric,
    /// `active` or `deactivated`.
    Status,
}

impl FilterField for QuotaFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::TenantId,
        Self::SubjectType,
        Self::SubjectId,
        Self::Metric,
        Self::Status,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::TenantId => "tenant_id",
            Self::SubjectType => "subject_type",
            Self::SubjectId => "subject_id",
            Self::Metric => "metric",
            Self::Status => "status",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::TenantId => FieldKind::Uuid,
            Self::SubjectType | Self::SubjectId | Self::Metric | Self::Status => FieldKind::String,
        }
    }

    fn published_ops(&self) -> Option<&'static [FilterOp]> {
        match self {
            Self::Id => Some(&[FilterOp::Eq, FilterOp::In]),
            _ => Some(&[FilterOp::Eq]),
        }
    }
}

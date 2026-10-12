//! Refund counter errors retain business cap routing and transaction conflicts.
use crate::domain::{error::DomainError, model::RepoError};

/// Preserve per-invoice cap routing; every other failure goes through the one
/// adjustment mapper (a conflict retries the complete operation).
pub(super) fn map_refund_cap_err(e: RepoError) -> DomainError {
    match e {
        RepoError::MoneyOutCapExceeded(m) => {
            // The per-invoice cap is the `payment_allocation_refund` row
            // (`chk_par_refunded_le_allocated`); its repo context stamps the
            // "allocation_refund" marker. Everything else is a settlement cap.
            if m.contains("allocation_refund") || m.contains("chk_par_") {
                DomainError::RefundExceedsAllocated(m)
            } else {
                DomainError::RefundExceedsSettled(m)
            }
        }
        other => super::map_adjustment_repo_err("refund cap sidecar", other),
    }
}

#[cfg(test)]
mod tests {
    use super::{DomainError, RepoError, map_refund_cap_err};
    #[test]
    fn conflicts_and_caps_keep_their_meaning() {
        assert!(matches!(
            map_refund_cap_err(RepoError::Conflict("stale".into())),
            DomainError::ConcurrentModification(_)
        ));
        assert!(matches!(
            map_refund_cap_err(RepoError::MoneyOutCapExceeded(
                "allocation_refund cap".into()
            )),
            DomainError::RefundExceedsAllocated(_)
        ));
        assert!(matches!(
            map_refund_cap_err(RepoError::MoneyOutCapExceeded(
                "payment_settlement cap".into()
            )),
            DomainError::RefundExceedsSettled(_)
        ));
        assert!(matches!(
            map_refund_cap_err(RepoError::InvalidStoredMoney("database is locked".into())),
            DomainError::Internal(_)
        ));
    }
}

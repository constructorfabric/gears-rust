//! Two variants sharing the same `(error_domain, error_code)` pair must fail —
//! otherwise wire-error reconstruction would be ambiguous (unreachable arm).

use toolkit_contract::ContractError;

#[derive(Debug, ContractError)]
#[error_domain("billing.v1")]
pub enum BillingError {
    #[error_code("DUP")]
    #[canonical(FailedPrecondition)]
    One { a: u32 },

    #[error_code("DUP")]
    #[canonical(FailedPrecondition)]
    Two { b: u32 },
}

fn main() {}

//! The trailing major-version marker on the trait name must agree with the
//! declared `version` (ADR-0007). Here the name says v2 while the contract
//! declares v1 — the descriptor/IR a consumer resolves would contradict the
//! type name, so this must be rejected at macro-expansion time.

use toolkit_contract::contract;

#[contract(gear = "billing", version = "v1")]
pub trait PaymentApiV2: Send + Sync {
    async fn charge(&self, amount: i64) -> Result<u64, std::io::Error>;
}

fn main() {}

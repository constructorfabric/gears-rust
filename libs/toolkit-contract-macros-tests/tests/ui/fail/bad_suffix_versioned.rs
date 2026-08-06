//! Stripping the trailing major-version marker must NOT weaken the suffix rule:
//! `PaymentServiceV2` reduces to `PaymentService`, which still carries no
//! contract-type suffix (`Api` / `Embedded` / `Backend` / `Extension`) and must
//! still be rejected.

use toolkit_contract::contract;

#[contract(gear = "billing", version = "v2")]
pub trait PaymentServiceV2: Send + Sync {
    async fn charge(&self, amount: i64) -> Result<u64, std::io::Error>;
}

fn main() {}

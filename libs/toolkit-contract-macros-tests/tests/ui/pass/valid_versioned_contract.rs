//! ADR-0007: a breaking change ships as a **parallel versioned trait**. A
//! trailing major-version marker must not defeat the contract-type suffix rule
//! (DESIGN D6) — `PaymentApiV2` classifies as `Api` exactly like `PaymentApi`,
//! and its REST projection is the ordinary `{Base}Rest` = `PaymentApiV2Rest`.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "billing", version = "v2")]
pub trait PaymentApiV2: Send + Sync {
    async fn charge(&self, ctx: SecurityContext, amount: i64) -> Result<u64, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v2")]
pub trait PaymentApiV2Rest: PaymentApiV2 {
    #[post("/payments/charge")]
    async fn charge(&self, ctx: SecurityContext, amount: i64) -> Result<u64, std::io::Error>;
}

// A versioned `Backend` stays remote-capable too (the gRPC/REST capability check
// also ignores the version marker).
#[contract(gear = "billing", version = "v3")]
pub trait NotificationBackendV3: Send + Sync {
    async fn deliver(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/billing/v3")]
pub trait NotificationBackendV3Rest: NotificationBackendV3 {
    #[post("/deliver")]
    async fn deliver(&self, ctx: SecurityContext, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}

//! gRPC projection trait name must be `<Base>Grpc` per PRD #1536 D1.

use toolkit_contract::{contract, grpc_contract};

#[contract(gear = "billing", version = "v1")]
pub trait PaymentApi: Send + Sync {
    async fn charge(&self, body: String) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "billing.payment.v1",
    service = "PaymentApi",
    stubs_module = "crate::stubs"
)]
pub trait PaymentApiOverGrpc: PaymentApi {
    #[rpc(name = "Charge")]
    async fn charge(&self, body: String) -> Result<u32, std::io::Error>;
}

fn main() {}

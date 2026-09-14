//! The framing is a single bare name, not a key/value pair (#4734 C3).
//! `#[streaming(sse = true)]` must be rejected with the same wording as every
//! other malformed argument, rather than with `syn`'s raw "unexpected token".

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/ticks")]
    #[streaming(sse = true)]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

fn main() {}

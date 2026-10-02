//! `#[streaming(open = fallible)]` on a non-`async fn` is a compile error
//! (#4734): the fallible open is an awaited operation, so the selector and the
//! `async` keyword must agree. The base declares the same method `async` with
//! `open = fallible`; the projection dropping `async` is the mismatch under
//! test.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = fallible)]
    async fn frames(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/frames")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    fn frames(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

fn main() {}

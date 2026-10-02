//! Two framing selectors on one `#[streaming(...)]` must be a compile error
//! (#4740) rather than last-one-wins, which would silently pick a wire format.

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
    #[streaming(sse, multipart_mixed)]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

fn main() {}

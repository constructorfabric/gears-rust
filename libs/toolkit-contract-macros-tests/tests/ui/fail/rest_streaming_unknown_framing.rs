//! An unrecognised framing name must be a compile error, not a silent fall
//! back to SSE (#4734 C3) — a typo'd framing would otherwise put the wrong
//! media type on the wire with no diagnostic.

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
    #[streaming(bogus)]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

fn main() {}

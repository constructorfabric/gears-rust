//! A second `#[streaming]` attribute on one REST projection method must be a
//! compile error (#4740) rather than last-one-wins. The two attributes below
//! disagree on both framing (`multipart_mixed` vs `sse`) and open shape
//! (`fallible` vs `immediate`); silently keeping the last would change the
//! emitted signature with no diagnostic.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[streaming(open = fallible)]
    async fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/ticks")]
    #[streaming(multipart_mixed, open = fallible)]
    #[streaming(sse, open = immediate)]
    #[server_manual]
    async fn ticks(&self, ctx: SecurityContext) -> Result<u64, std::io::Error>;
}

fn main() {}

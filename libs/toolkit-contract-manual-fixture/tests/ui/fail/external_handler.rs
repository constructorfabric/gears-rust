//! A consumer crate cannot implement an SDK's sealed handler trait.

use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::manual::ManualCallContext;
use toolkit_security::SecurityContext;
use toolkit_contract_manual_fixture::{Echo, StatefulDemoApiRestManual};

struct Mine;

#[async_trait::async_trait]
impl StatefulDemoApiRestManual for Mine {
    async fn count(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
    ) -> Result<Echo, CanonicalError> {
        unimplemented!()
    }
}

fn main() {}

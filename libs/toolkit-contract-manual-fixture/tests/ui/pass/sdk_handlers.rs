//! Consumers use the SDK's handlers: concretely, as `dyn`, or through the
//! default type parameter.

use std::sync::Arc;

use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract_manual_fixture::handlers::{DemoHandler, StatefulHandler};
use toolkit_contract_manual_fixture::{
    ManualDemoApi, ManualDemoApiRest, ManualDemoApiRestClient, ManualDemoApiRestManual,
    StatefulDemoApi, StatefulDemoApiRestClient,
};

// Compile-only: building a client needs a Tokio runtime.
fn main() {}

#[allow(dead_code)]
fn usage() {
    // The bare type name is the default handler; `new` keeps its signature.
    let default: ManualDemoApiRestClient =
        ManualDemoApiRestClient::new(ClientConfig::new("http://x")).unwrap();
    let _: Arc<dyn ManualDemoApi> = Arc::new(default);

    let concrete = ManualDemoApiRestClient::with_manual(
        ClientConfig::new("http://x"),
        Arc::new(DemoHandler),
    )
    .unwrap();
    let _: Arc<dyn ManualDemoApiRest> = Arc::new(concrete);

    let dynamic: Arc<dyn ManualDemoApiRestManual> = Arc::new(DemoHandler);
    let client: ManualDemoApiRestClient<dyn ManualDemoApiRestManual> =
        ManualDemoApiRestClient::with_manual(ClientConfig::new("http://x"), dynamic).unwrap();
    let _: Arc<dyn ManualDemoApi> = Arc::new(client);

    // No default handler: `with_manual` with a non-`Clone` stateful handler.
    let stateful = StatefulDemoApiRestClient::with_manual(
        ClientConfig::new("http://x"),
        Arc::new(StatefulHandler::default()),
    )
    .unwrap();
    let _: Arc<dyn StatefulDemoApi> = Arc::new(stateful);
}

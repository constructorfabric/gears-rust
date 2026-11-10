//! Without `manual_handler` there is no `new`: a handler must be supplied.

use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract_manual_fixture::StatefulDemoApiRestClient;

fn main() {
    let _ = StatefulDemoApiRestClient::new(ClientConfig::new("http://x"));
}

//! Projection trait without a base supertrait — must fail (PRD D1).

use toolkit_contract::rest_contract;

#[rest_contract(base_path = "/api/foo/v1")]
pub trait FooApiRest {
    #[post("/ping")]
    async fn ping(&self) -> Result<String, std::io::Error>;
}

fn main() {}

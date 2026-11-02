//! Publishing through a registry is not consuming it: a gear may also
//! consume the same registry contract for its own business.
use std::sync::Arc;

pub mod adapter {
    use std::sync::Arc;

    pub trait RegistryApi: Send + Sync {}
    pub type Registry = dyn RegistryApi;
    pub const PROVIDER: &str = "registry";

    pub struct ResolvingClient;
    impl ResolvingClient {
        pub fn new(
            _: Arc<dyn toolkit::discovery::EndpointResolver>,
            _: &'static str,
            _: toolkit::contract_support::wiring::ClientTuning,
        ) -> Self {
            Self
        }
    }
    impl RegistryApi for ResolvingClient {}
    pub type RegistryApiRestResolvingClient = ResolvingClient;

    #[derive(Debug)]
    pub struct Never;
    impl std::fmt::Display for Never {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            Ok(())
        }
    }
    impl std::error::Error for Never {}

    pub async fn publish(
        _: &Registry,
        _: &str,
        _: &str,
        _: &[(String, serde_json::Value)],
        _: &tokio_util::sync::CancellationToken,
        _: impl FnMut(&str) + Send,
    ) -> Result<(), Never> {
        Ok(())
    }
}

pub fn gts_declarations() -> Vec<(String, String)> {
    Vec::new()
}

#[toolkit::consumes(contract = adapter::RegistryApi, from = "registry")]
#[toolkit::gear(name = "x", gts(crates = [crate], publisher = adapter))]
#[derive(Default)]
pub struct X;

#[async_trait::async_trait]
impl toolkit::Gear for X {
    async fn init(&self, _: &toolkit::GearCtx) -> anyhow::Result<()> {
        let _ = Arc::new(());
        Ok(())
    }
}

fn main() {}

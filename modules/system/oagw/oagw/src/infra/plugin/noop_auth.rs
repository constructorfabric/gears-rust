use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};
use async_trait::async_trait;

/// Auth plugin that does nothing — used for upstreams requiring no authentication.
pub struct NoopAuthPlugin;

#[async_trait]
impl AuthPlugin for NoopAuthPlugin {
    async fn authenticate(&self, _ctx: &mut AuthContext) -> Result<(), PluginError> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "noop_auth_tests.rs"]
mod noop_auth_tests;

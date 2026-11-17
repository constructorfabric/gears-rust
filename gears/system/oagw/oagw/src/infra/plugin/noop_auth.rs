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
mod tests {
    use std::collections::HashMap;

    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn noop_sets_no_headers() {
        let plugin = NoopAuthPlugin;
        let mut ctx = AuthContext::new(
            HashMap::new(),
            SecurityContext::builder()
                .subject_tenant_id(Uuid::nil())
                .subject_id(Uuid::nil())
                .build()
                .unwrap(),
        );

        plugin.authenticate(&mut ctx).await.unwrap();

        assert!(ctx.set_headers.is_empty());
    }
}

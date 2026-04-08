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

    use modkit_security::SecurityContext;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn noop_leaves_headers_unchanged() {
        let plugin = NoopAuthPlugin;
        let mut headers = HashMap::new();
        headers.insert("x-existing".to_string(), "value".to_string());

        let mut ctx = AuthContext {
            headers: headers.clone(),
            config: HashMap::new(),
            security_context: SecurityContext::builder()
                .subject_tenant_id(Uuid::nil())
                .subject_id(Uuid::nil())
                .build()
                .unwrap(),
        };

        plugin.authenticate(&mut ctx).await.unwrap();

        // Headers should be identical.
        assert_eq!(ctx.headers.len(), 1);
        assert_eq!(ctx.headers.get("x-existing").unwrap(), "value");
    }
}

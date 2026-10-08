use std::sync::Arc;
use std::time::Duration;

use toolkit::client_hub::ClientHub;

use crate::config::ModelConfig;
use crate::domain::model_client::ModelClient;

pub mod chat_completions;
#[cfg(test)]
mod chat_completions_test;

/// @cpt-dod:cpt-cf-construct-dod-model-client-config:p1
#[must_use]
pub fn model_client(config: &ModelConfig, hub: Arc<ClientHub>) -> Arc<dyn ModelClient> {
    match config {
        ModelConfig::ChatCompletions {
            upstream_alias,
            model,
            timeout_ms,
        } => Arc::new(chat_completions::ChatCompletionsModel::new(
            hub,
            upstream_alias.clone(),
            model.clone(),
            Duration::from_millis(*timeout_ms),
        )),
    }
}

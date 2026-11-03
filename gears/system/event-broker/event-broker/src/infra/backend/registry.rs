use std::collections::BTreeMap;
use std::sync::Arc;

use event_broker_sdk::backend::{Backend, BuildContext, Provider, Role};
use event_broker_sdk::gts::TopicV1;
use toolkit_gts::GtsSchema;

use crate::config::checks::{self, Deployment};
use crate::domain::backend::{self, BackendResolver, Routing};
use crate::domain::model::Topic;

/// Every registry entry, built, and the routing that maps a topic onto one.
///
/// Each entry is built once, so every topic routed to it shares the one
/// backend and its connection pool (AC-4).
pub struct Registry {
    backends: BTreeMap<backend::Name, Arc<dyn Backend>>,
    routing: Routing,
    /// The instance-less route's backend, kept apart so resolution has an
    /// answer for every topic without a lookup that could miss.
    base: Arc<dyn Backend>,
}

/// A registry entry that could not be built: C1 and C2 of feature 0005 §2.4.
///
/// Names the entry and its type, both validated, and never a settings value.
/// A provider's own refusal is reproduced as it stands: the storage backend
/// API requires it to name the setting and the rule it broke without quoting
/// the value (feature 0007 §2.7).
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// C1.
    #[error(
        "backends: entry '{name}' names backend type '{backend_type}', which no provider linked into this build serves; it links {linked:?}; every entry's type is served by a linked provider (feature 0005 section 2.4, C1)"
    )]
    UnservedType {
        name: backend::Name,
        backend_type: String,
        linked: Vec<String>,
    },

    /// C2.
    #[error(
        "backends: entry '{name}' (type '{backend_type}') was refused by its provider: {reason}; every entry's settings are accepted by its provider (feature 0005 section 2.4, C2)"
    )]
    RefusedSettings {
        name: backend::Name,
        backend_type: String,
        reason: String,
    },

    /// A deployment that did not come through [`checks::check`], whose
    /// instance-less route names no built entry.
    #[error(transparent)]
    Check(#[from] checks::Error),
}

impl Registry {
    /// Builds every entry of `deployment` through the provider serving its
    /// type. Building validates settings and connects lazily, so an
    /// unreachable backend fails its topics' operations, never this.
    ///
    /// # Errors
    /// [`BuildError`] for the first entry whose type no provider serves or
    /// whose settings its provider refuses.
    pub async fn build(
        deployment: Deployment,
        providers: &[Box<dyn Provider>],
        role: Role,
    ) -> Result<Self, BuildError> {
        let mut backends = BTreeMap::new();
        for (name, entry) in deployment.backends {
            let backend_type = entry.r#type.as_ref().to_owned();
            let Some(provider) = providers
                .iter()
                .find(|provider| provider.backend_type().as_ref() == backend_type)
            else {
                return Err(BuildError::UnservedType {
                    name,
                    backend_type,
                    linked: providers
                        .iter()
                        .map(|provider| provider.backend_type().to_string())
                        .collect(),
                });
            };
            let built = provider
                .build(&BuildContext::new(&entry.settings, role))
                .await
                .map_err(|err| BuildError::RefusedSettings {
                    name: name.clone(),
                    backend_type: backend_type.clone(),
                    reason: err.to_string(),
                })?;
            tracing::info!(
                backend = %name,
                %backend_type,
                dedup = ?built.dedup(),
                "storage backend built"
            );
            backends.insert(name, built);
        }
        Self::from_built(backends, deployment.routing)
    }

    /// Every built backend, in registry-name order: the process starts each
    /// before its workers and stops each after.
    #[must_use]
    pub fn backends(&self) -> Vec<Arc<dyn Backend>> {
        self.backends.values().cloned().collect()
    }

    /// Assembles a registry from backends already built. The checks prove the
    /// instance-less route names an entry, so the base is always present for
    /// a deployment that came through them.
    fn from_built(
        backends: BTreeMap<backend::Name, Arc<dyn Backend>>,
        routing: Routing,
    ) -> Result<Self, BuildError> {
        let base =
            backends
                .get(routing.base())
                .cloned()
                .ok_or_else(|| checks::Error::UnknownBackend {
                    key: TopicV1::TYPE_ID.to_owned(),
                })?;
        Ok(Self {
            backends,
            routing,
            base,
        })
    }
}

impl BackendResolver for Registry {
    /// The topic's resolved route names its entry. The routing is consulted
    /// only if that name is not in this registry - a record resolved under
    /// another configuration - and the instance-less route answers last.
    fn resolve(&self, topic: &Topic) -> Arc<dyn Backend> {
        self.backends
            .get(topic.settings.backend().value())
            .or_else(|| self.backends.get(self.routing.resolve(&topic.id).value()))
            .unwrap_or(&self.base)
            .clone()
    }
}

use std::future::Future;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::Mutex;

use crate::gts::BaseModkitPluginV1;

/// A resettable, allocation-friendly selector for GTS plugin instance IDs.
///
/// Uses a single-flight pattern to ensure that the resolve function is called
/// at most once even under concurrent callers. The selected instance ID is
/// cached as `Arc<str>` to avoid allocations on the happy path.
pub struct GtsPluginSelector {
    /// Cached selected instance ID (sync lock for fast access and sync reset).
    cached: RwLock<Option<Arc<str>>>,
    /// Mutex to ensure single-flight resolution.
    resolve_lock: Mutex<()>,
}

impl Default for GtsPluginSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl GtsPluginSelector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cached: RwLock::new(None),
            resolve_lock: Mutex::new(()),
        }
    }

    /// Create a selector with `value` already cached, skipping resolution entirely.
    ///
    /// Useful in tests to pre-warm the selector with a known instance ID or
    /// an empty-string sentinel (meaning "no plugin configured").
    #[must_use]
    pub fn pre_cached(value: String) -> Self {
        Self {
            cached: RwLock::new(Some(Arc::from(value))),
            resolve_lock: Mutex::new(()),
        }
    }

    /// Returns the cached instance ID, or resolves it using the provided function.
    ///
    /// Uses a single-flight pattern: even under concurrent callers, the resolve
    /// function is called at most once. Returns `Arc<str>` to avoid allocations
    /// on the happy path.
    /// # Errors
    ///
    /// Returns `Err(E)` if the provided `resolve` future fails.
    pub async fn get_or_init<F, Fut, E>(&self, resolve: F) -> Result<Arc<str>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, E>>,
    {
        // Fast path: check if already cached (sync lock, no await)
        {
            let guard = self.cached.read();
            if let Some(ref id) = *guard {
                return Ok(Arc::clone(id));
            }
        }

        // Slow path: acquire resolve lock for single-flight
        let _resolve_guard = self.resolve_lock.lock().await;

        // Re-check after acquiring resolve lock (another caller may have resolved)
        {
            let guard = self.cached.read();
            if let Some(ref id) = *guard {
                return Ok(Arc::clone(id));
            }
        }

        // Resolve and cache
        let id_string = resolve().await?;
        let id: Arc<str> = id_string.into();

        {
            let mut guard = self.cached.write();
            *guard = Some(Arc::clone(&id));
        }

        Ok(id)
    }

    /// Clears the cached selected instance ID.
    ///
    /// Returns `true` if there was a cached value, `false` otherwise.
    pub async fn reset(&self) -> bool {
        let _resolve_guard = self.resolve_lock.lock().await;
        let mut guard = self.cached.write();
        guard.take().is_some()
    }
}

/// Error returned by [`choose_plugin_instance`].
#[derive(Debug, thiserror::Error)]
pub enum ChoosePluginError {
    /// Failed to deserialize a plugin instance's content.
    #[error("invalid plugin instance content for '{gts_id}': {reason}")]
    InvalidPluginInstance {
        /// GTS ID of the malformed instance.
        gts_id: String,
        /// Human-readable reason.
        reason: String,
    },

    /// No plugin instance matched the requested vendor.
    #[error("no plugin instances found for schema '{schema_id}', vendor '{vendor}'")]
    PluginNotFound {
        /// GTS schema ID of the plugin type being resolved.
        schema_id: String,
        /// The vendor that was requested.
        vendor: String,
    },
}

/// Selects the best plugin instance for the given vendor.
///
/// Accepts an iterator of `(gts_id, content)` pairs — typically
/// produced from `types_registry_sdk::GtsEntity`:
///
/// ```ignore
/// choose_plugin_instance::<MyPluginSpecV1>(
///     &self.vendor,
///     instances.iter().map(|e| (e.gts_id.as_str(), &e.content)),
/// )
/// ```
///
/// Deserializes each entry as `BaseModkitPluginV1<P>`, filters by
/// `vendor`, and returns the `gts_id` of the instance with the
/// **lowest** priority value.
///
/// # Type Parameters
///
/// - `P` — The plugin-specific properties struct (e.g.
///   `AuthNResolverPluginSpecV1`). Must be `DeserializeOwned`.
///
/// # Errors
///
/// - [`ChoosePluginError::InvalidPluginInstance`] if deserialization fails
///   or the `content.id` doesn't match `gts_id`.
/// - [`ChoosePluginError::PluginNotFound`] if no instance matches the vendor.
pub fn choose_plugin_instance<'a, P>(
    vendor: &str,
    instances: impl IntoIterator<Item = (&'a str, &'a serde_json::Value)>,
) -> Result<String, ChoosePluginError>
where
    P: for<'de> gts::GtsDeserialize<'de> + gts::GtsSchema,
{
    let mut best: Option<(&str, i16)> = None;
    let mut count: usize = 0;

    for (gts_id, content_val) in instances {
        count += 1;
        let content: BaseModkitPluginV1<P> =
            serde_json::from_value(content_val.clone()).map_err(|e| {
                tracing::error!(
                    gts_id = %gts_id,
                    error = %e,
                    "Failed to deserialize plugin instance content"
                );
                ChoosePluginError::InvalidPluginInstance {
                    gts_id: gts_id.to_owned(),
                    reason: e.to_string(),
                }
            })?;

        if content.id != gts_id {
            return Err(ChoosePluginError::InvalidPluginInstance {
                gts_id: gts_id.to_owned(),
                reason: format!(
                    "content.id mismatch: expected {:?}, got {:?}",
                    gts_id, content.id
                ),
            });
        }

        if content.vendor != vendor {
            continue;
        }

        match &best {
            None => best = Some((gts_id, content.priority)),
            Some((_, cur_priority)) => {
                if content.priority < *cur_priority {
                    best = Some((gts_id, content.priority));
                }
            }
        }
    }

    tracing::debug!(vendor, instance_count = count, "choose_plugin_instance");

    best.map(|(gts_id, _)| gts_id.to_owned())
        .ok_or_else(|| ChoosePluginError::PluginNotFound {
            schema_id: P::SCHEMA_ID.to_owned(),
            vendor: vendor.to_owned(),
        })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn resolve_called_once_returns_same_str() {
        let selector = GtsPluginSelector::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_a = calls.clone();
        let id_a = selector
            .get_or_init(|| async move {
                calls_a.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(
                    "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1"
                        .to_owned(),
                )
            })
            .await
            .unwrap();

        let calls_b = calls.clone();
        let id_b = selector
            .get_or_init(|| async move {
                calls_b.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(
                    "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1"
                        .to_owned(),
                )
            })
            .await
            .unwrap();

        assert_eq!(id_a, id_b);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reset_triggers_reselection() {
        let selector = GtsPluginSelector::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_a = calls.clone();
        let id_a = selector
            .get_or_init(|| async move {
                calls_a.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(
                    "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1"
                        .to_owned(),
                )
            })
            .await;
        assert_eq!(
            &*id_a.unwrap(),
            "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~a.test._.plugin.v1"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(selector.reset().await);

        let calls_b = calls.clone();
        let id_b = selector
            .get_or_init(|| async move {
                calls_b.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(
                    "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1"
                        .to_owned(),
                )
            })
            .await;
        assert_eq!(
            &*id_b.unwrap(),
            "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~b.test._.plugin.v1"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_get_or_init_resolves_once() {
        let selector = Arc::new(GtsPluginSelector::new());
        let calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let selector = Arc::clone(&selector);
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                selector
                    .get_or_init(|| async {
                        // Small delay to increase chance of concurrent access
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, std::convert::Infallible>(
                            "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~concurrent.test._.plugin.v1"
                                .to_owned(),
                        )
                    })
                    .await
            }));
        }

        // Await each handle in a loop (no futures_util dependency)
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap().unwrap());
        }

        // All results should be the same
        for id in &results {
            assert_eq!(
                &**id,
                "gts.x.core.modkit.plugin.v1~x.core.test.plugin.v1~concurrent.test._.plugin.v1"
            );
        }

        // Resolve should have been called exactly once
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

use std::future::Future;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::Mutex;

use crate::client_hub::{ClientHub, ClientScope};
use crate::gts::PluginV1;

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
    #[error("no plugin instances found for type '{type_id}', vendor '{vendor}'")]
    PluginNotFound {
        /// GTS Type Identifier of the plugin type being resolved.
        type_id: String,
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
/// Deserializes each entry as `PluginV1<P>`, filters by
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
        let content: PluginV1<P> = serde_json::from_value(content_val.clone()).map_err(|e| {
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
            type_id: P::TYPE_ID.to_owned(),
            vendor: vendor.to_owned(),
        })
}

/// [`choose_plugin_instance`] restricted to candidates with a scoped `C` client
/// in `hub` under [`ClientScope::gts_id`]. This checks registration, not health.
///
/// Accepts `(gts_id, content)` pairs. Unregistered entries are skipped without
/// validation, allowing stale catalogue entries with absent or malformed content.
/// Registered entries retain listing order; the first wins a priority tie.
///
/// Callers must pair `C` with the listing's `P` and the later scoped lookup;
/// the types do not enforce this. Each registered entry must have content that
/// decodes as `PluginV1<P>` with `id == gts_id`.
///
/// # Errors
///
/// - [`ChoosePluginError::InvalidPluginInstance`] for missing registered content
///   (checked for all candidates before decoding) or as [`choose_plugin_instance`] reports.
/// - [`ChoosePluginError::PluginNotFound`] when no registered instance matches the vendor.
pub fn choose_registered_plugin_instance<'a, P, C>(
    hub: &ClientHub,
    vendor: &str,
    instances: impl IntoIterator<Item = (&'a str, Option<&'a serde_json::Value>)>,
) -> Result<String, ChoosePluginError>
where
    P: for<'de> gts::GtsDeserialize<'de> + gts::GtsSchema,
    C: ?Sized + Send + Sync + 'static,
{
    let registered = instances
        .into_iter()
        .filter(|(gts_id, _)| {
            hub.try_get_scoped::<C>(&ClientScope::gts_id(gts_id))
                .is_some()
        })
        .map(|(gts_id, content)| {
            content.map(|content| (gts_id, content)).ok_or_else(|| {
                ChoosePluginError::InvalidPluginInstance {
                    gts_id: gts_id.to_owned(),
                    reason: "listed without content".to_owned(),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    choose_plugin_instance::<P>(vendor, registered)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use toolkit_gts::gts_id;

    const PLUGIN_A: &str =
        gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.test.plugin.v1~vendor.a.test.plugin.v1");
    const PLUGIN_B: &str =
        gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.test.plugin.v1~vendor.b.test.plugin.v1");
    const CONCURRENT_PLUGIN: &str = gts_id!(
        "cf.toolkit.plugins.plugin.v1~cf.core.test.plugin.v1~vendor.concurrent.test.plugin.v1"
    );

    #[tokio::test]
    async fn resolve_called_once_returns_same_str() {
        let selector = GtsPluginSelector::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_a = calls.clone();
        let id_a = selector
            .get_or_init(|| async move {
                calls_a.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(PLUGIN_A.to_owned())
            })
            .await
            .unwrap();

        let calls_b = calls.clone();
        let id_b = selector
            .get_or_init(|| async move {
                calls_b.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(PLUGIN_B.to_owned())
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
                Ok::<_, std::convert::Infallible>(PLUGIN_A.to_owned())
            })
            .await;
        assert_eq!(&*id_a.unwrap(), PLUGIN_A);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(selector.reset().await);

        let calls_b = calls.clone();
        let id_b = selector
            .get_or_init(|| async move {
                calls_b.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::convert::Infallible>(PLUGIN_B.to_owned())
            })
            .await;
        assert_eq!(&*id_b.unwrap(), PLUGIN_B);
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
                        Ok::<_, std::convert::Infallible>(CONCURRENT_PLUGIN.to_owned())
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
            assert_eq!(&**id, CONCURRENT_PLUGIN);
        }

        // Resolve should have been called exactly once
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    mod choose_registered {
        use super::*;
        use serde_json::{Value, json};
        use toolkit_gts::gts_type_schema;

        #[derive(Default)]
        #[gts_type_schema(
            dir_path = "schemas",
            base = PluginV1,
            type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.test.plugin.v1~"),
            description = "Test plugin specification",
            properties = "",
        )]
        struct TestPluginSpecV1;

        trait TestClient: Send + Sync {}
        trait OtherClient: Send + Sync {}
        struct Client;
        impl TestClient for Client {}
        impl OtherClient for Client {}

        /// A valid instance of `name`; its id and content.
        fn instance(name: &str, vendor: &str, priority: i16) -> (String, Value) {
            let (id, content) = PluginV1::<TestPluginSpecV1>::build_registration(
                &format!("vendor.x.{name}.plugin.v1"),
                vendor,
                priority,
            )
            .expect("serializes");
            (id.to_string(), content)
        }

        fn register(hub: &ClientHub, gts_id: &str) {
            hub.register_scoped::<dyn TestClient>(ClientScope::gts_id(gts_id), Arc::new(Client));
        }

        fn choose(
            hub: &ClientHub,
            listed: &[(String, Option<Value>)],
        ) -> Result<String, ChoosePluginError> {
            choose_registered_plugin_instance::<TestPluginSpecV1, dyn TestClient>(
                hub,
                "acme",
                listed
                    .iter()
                    .map(|(id, content)| (id.as_str(), content.as_ref())),
            )
        }

        #[test]
        fn unregistered_entries_are_skipped_unexamined() {
            let hub = ClientHub::new();
            let (missing, _) = instance("missing", "acme", 0);
            let (malformed, _) = instance("malformed", "acme", 0);
            let (preferred, preferred_content) = instance("preferred", "acme", -10);
            let (running, running_content) = instance("running", "acme", 5);
            let (elsewhere, elsewhere_content) = instance("elsewhere", "acme", -20);
            register(&hub, &running);
            // Registered for another client type: not a candidate for this one.
            hub.register_scoped::<dyn OtherClient>(
                ClientScope::gts_id(&elsewhere),
                Arc::new(Client),
            );

            let chosen = choose(
                &hub,
                &[
                    (missing, None),
                    (malformed, Some(json!({ "bogus": true }))),
                    (preferred, Some(preferred_content)),
                    (running.clone(), Some(running_content)),
                    (elsewhere, Some(elsewhere_content)),
                ],
            )
            .expect("chooses");
            assert_eq!(chosen, running);
        }

        #[test]
        fn the_lowest_registered_priority_of_the_vendor_wins_and_the_first_on_a_tie() {
            let hub = ClientHub::new();
            let listed: Vec<_> = [
                instance("low", "acme", 5),
                instance("other_vendor", "other", -5),
                instance("tie_first", "acme", 1),
                instance("tie_second", "acme", 1),
            ]
            .into_iter()
            .map(|(id, content)| {
                register(&hub, &id);
                (id, Some(content))
            })
            .collect();

            assert_eq!(choose(&hub, &listed).expect("chooses"), listed[2].0);
        }

        #[test]
        fn a_registered_entry_without_content_is_invalid_before_any_is_read() {
            let hub = ClientHub::new();
            let (malformed, _) = instance("malformed", "acme", 0);
            let (missing, _) = instance("missing", "acme", 0);
            register(&hub, &malformed);
            register(&hub, &missing);

            let error = choose(
                &hub,
                &[
                    (malformed, Some(json!({ "bogus": true }))),
                    (missing.clone(), None),
                ],
            )
            .expect_err("refused");
            assert!(
                matches!(&error, ChoosePluginError::InvalidPluginInstance { gts_id, reason }
                    if *gts_id == missing && reason == "listed without content"),
                "{error:?}"
            );
        }

        #[test]
        fn a_malformed_or_misnamed_registered_entry_is_invalid() {
            let hub = ClientHub::new();
            let (malformed, _) = instance("malformed", "other", 0);
            let (good, good_content) = instance("good", "acme", 0);
            let (misnamed, _) = instance("misnamed", "acme", 0);
            for id in [&malformed, &good, &misnamed] {
                register(&hub, id);
            }

            let error = choose(
                &hub,
                &[
                    (good, Some(good_content.clone())),
                    (malformed.clone(), Some(json!({ "bogus": true }))),
                ],
            )
            .expect_err("refused");
            assert!(
                matches!(&error, ChoosePluginError::InvalidPluginInstance { gts_id, .. } if *gts_id == malformed),
                "{error:?}"
            );

            let error =
                choose(&hub, &[(misnamed.clone(), Some(good_content))]).expect_err("refused");
            assert!(
                matches!(&error, ChoosePluginError::InvalidPluginInstance { gts_id, .. } if *gts_id == misnamed),
                "{error:?}"
            );
        }

        #[test]
        fn nothing_registered_is_not_found() {
            let (id, content) = instance("listed", "acme", 0);
            let error = choose(&ClientHub::new(), &[(id, Some(content))]).expect_err("refused");
            assert!(
                matches!(error, ChoosePluginError::PluginNotFound { .. }),
                "{error:?}"
            );
        }
    }
}

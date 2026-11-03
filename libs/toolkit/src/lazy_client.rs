//! Lazy, cached retrieval of a consumed client from the [`ClientHub`].
//!
//! A consumer binds the hub once (typically in `init`) and resolves the client
//! on first use, when the provider may have registered since. Only a
//! successful lookup is cached; a miss is returned to the caller and the next
//! [`LazyClient::get`] looks the hub up again.

use std::sync::{Arc, OnceLock};

use crate::client_hub::{ClientHub, ClientHubError};

/// Why [`LazyClient::get`] produced no client.
#[derive(Debug, thiserror::Error)]
pub enum LazyClientError {
    /// [`LazyClient::bind`] was never called.
    #[error("lazy client is not bound to a ClientHub")]
    NotBound,
    /// The hub lookup failed, usually because no client is registered (yet).
    #[error(transparent)]
    Resolve(#[from] ClientHubError),
}

/// A client of interface `T` (e.g. `dyn my_sdk::Api`), looked up in the bound
/// [`ClientHub`] on first [`get`](Self::get) and cached once found.
pub struct LazyClient<T: ?Sized> {
    hub: OnceLock<Arc<ClientHub>>,
    client: OnceLock<Arc<T>>,
}

impl<T: ?Sized> Default for LazyClient<T> {
    fn default() -> Self {
        Self {
            hub: OnceLock::new(),
            client: OnceLock::new(),
        }
    }
}

impl<T: ?Sized + Send + Sync + 'static> LazyClient<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the hub the client is resolved from. The first bind wins; later
    /// calls are ignored.
    pub fn bind(&self, hub: Arc<ClientHub>) {
        drop(self.hub.set(hub));
    }

    /// The cached client, or a fresh hub lookup cached on success.
    ///
    /// Concurrent first lookups all return the one `Arc` that won the cache.
    ///
    /// # Errors
    /// [`LazyClientError::NotBound`] before [`bind`](Self::bind);
    /// [`LazyClientError::Resolve`] when the hub lookup fails, usually because
    /// no client for `T` is registered yet.
    pub fn get(&self) -> Result<Arc<T>, LazyClientError> {
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        let hub = self.hub.get().ok_or(LazyClientError::NotBound)?;
        let found = hub.get::<T>()?;
        Ok(Arc::clone(self.client.get_or_init(|| found)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Api: Send + Sync {
        fn id(&self) -> u32;
    }
    struct Impl(u32);
    impl Api for Impl {
        fn id(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn get_before_bind_is_not_bound() {
        let lazy = LazyClient::<dyn Api>::new();
        assert!(matches!(lazy.get(), Err(LazyClientError::NotBound)));
    }

    #[test]
    fn a_miss_is_not_cached_and_a_late_provider_is_found() {
        let hub = Arc::new(ClientHub::new());
        let lazy = LazyClient::<dyn Api>::new();
        lazy.bind(Arc::clone(&hub));

        assert!(matches!(lazy.get(), Err(LazyClientError::Resolve(_))));
        assert!(matches!(lazy.get(), Err(LazyClientError::Resolve(_))));

        hub.register::<dyn Api>(Arc::new(Impl(1)));
        let first = lazy.get().expect("resolved after registration");
        let again = lazy.get().expect("cached");
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(first.id(), 1);
    }

    #[test]
    fn the_cached_client_is_kept_after_the_hub_changes() {
        let hub = Arc::new(ClientHub::new());
        let lazy = LazyClient::<dyn Api>::new();
        lazy.bind(Arc::clone(&hub));
        hub.register::<dyn Api>(Arc::new(Impl(1)));
        let first = lazy.get().unwrap();

        hub.register::<dyn Api>(Arc::new(Impl(2)));
        hub.remove::<dyn Api>();
        assert!(Arc::ptr_eq(&first, &lazy.get().unwrap()));
    }

    #[test]
    fn the_first_bind_wins() {
        let bound = Arc::new(ClientHub::new());
        bound.register::<dyn Api>(Arc::new(Impl(1)));
        let lazy = LazyClient::<dyn Api>::new();
        lazy.bind(bound);
        lazy.bind(Arc::new(ClientHub::new()));
        assert_eq!(lazy.get().unwrap().id(), 1);
    }

    #[test]
    fn concurrent_first_lookups_share_the_winning_client() {
        let hub = Arc::new(ClientHub::new());
        let lazy = Arc::new(LazyClient::<dyn Api>::new());
        lazy.bind(Arc::clone(&hub));
        hub.register::<dyn Api>(Arc::new(Impl(1)));
        let barrier = Arc::new(std::sync::Barrier::new(8));

        // Spawn all threads before joining any: they meet at the barrier.
        let mut threads = Vec::new();
        for i in 0..8u32 {
            let (hub, lazy, barrier) = (Arc::clone(&hub), Arc::clone(&lazy), Arc::clone(&barrier));
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                // Replace the registration while others resolve.
                hub.register::<dyn Api>(Arc::new(Impl(i + 10)));
                lazy.get().unwrap()
            }));
        }
        let clients: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        let cached = lazy.get().unwrap();
        assert!(clients.iter().all(|c| Arc::ptr_eq(c, &cached)));
    }
}

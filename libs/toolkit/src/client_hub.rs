//! Minimalistic, type-safe `ClientHub`.
//!
//! Design goals:
//! - Providers register an implementation once (local or remote).
//! - Consumers fetch by *interface type* (trait object): `get::<dyn my::Api>()`.
//! - For plugin-like scenarios, multiple implementations of the same interface can coexist
//!   under different scopes (e.g. selected by GTS instance ID).
//! - Remote proxies wired by `#[toolkit::consumes]` belong to the consuming gear: each gear
//!   reads the hub through its own view ([`ClientHub::for_gear`]) and gets its own proxy.
//!
//! Implementation details:
//! - Key = type name. We use `type_name::<T>()`, which works for `T = dyn Trait`.
//! - Value = `Arc<T>` stored as `Box<dyn Any + Send + Sync>` (downcast on read).
//! - Sync hot path: `get()` is non-async; no hidden per-entry cells or lazy slots.
//!
//! Notes:
//! - Re-registering overwrites the previous value atomically; existing Arcs held by consumers remain valid.
//! - For testing, just register a mock under the same trait type.

use parking_lot::RwLock;
use std::{any::Any, collections::HashMap, fmt, sync::Arc};

/// Stable type key for trait objects — uses fully-qualified `type_name::<T>()`.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct TypeKey(&'static str);

impl TypeKey {
    #[inline]
    fn of<T: ?Sized + 'static>() -> Self {
        TypeKey(std::any::type_name::<T>())
    }
}

impl fmt::Debug for TypeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A scope for resolving multiple implementations of the same interface type.
///
/// This is intentionally opaque: the scope semantics are defined by the caller.
/// One common scope is a full GTS entity/instance ID.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ClientScope(Arc<str>);

impl ClientScope {
    /// Create a new scope from an arbitrary string.
    #[inline]
    #[must_use]
    pub fn new(scope: impl Into<Arc<str>>) -> Self {
        Self(scope.into())
    }

    /// Create a scope derived from a GTS identifier.
    ///
    /// Internally we prefix the scope to avoid accidental collisions with other scope kinds.
    #[must_use]
    pub fn gts_id(gts_id: &str) -> Self {
        let mut s = String::with_capacity("gts:".len() + gts_id.len());
        s.push_str("gts:");
        s.push_str(gts_id);
        Self(Arc::<str>::from(s))
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct ScopedKey {
    type_key: TypeKey,
    scope: ClientScope,
}

impl fmt::Debug for ScopedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedKey")
            .field("type_key", &self.type_key)
            .field("scope", &self.scope)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientHubError {
    #[error("client not found: type={type_key:?}")]
    NotFound { type_key: TypeKey },

    #[error("type mismatch in hub for type={type_key:?}")]
    TypeMismatch { type_key: TypeKey },

    #[error("scoped client not found: type={type_key:?} scope={scope:?}")]
    ScopedNotFound {
        type_key: TypeKey,
        scope: ClientScope,
    },

    #[error("type mismatch in hub for type={type_key:?} scope={scope:?}")]
    ScopedTypeMismatch {
        type_key: TypeKey,
        scope: ClientScope,
    },
}

type Boxed = Box<dyn Any + Send + Sync>;

/// Internal map type for the client hub.
type ClientMap = HashMap<TypeKey, Boxed>;

/// Internal map type for the scoped client hub.
type ScopedClientMap = HashMap<ScopedKey, Boxed>;

/// Key of a consumer-wiring proxy: the contract type and the gear that owns
/// the proxy. `owner = None` is a process-wide proxy.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
struct ProxyKey {
    type_key: TypeKey,
    owner: Option<Arc<str>>,
}

/// Registrations shared by every view of one hub.
#[derive(Default)]
struct Storage {
    map: RwLock<ClientMap>,
    scoped_map: RwLock<ScopedClientMap>,
    /// Discovery-resolving proxies registered by consumer wiring, kept apart
    /// from `map` so a proxy is never mistaken for an in-process impl.
    ///
    /// Keyed by owner because each consumer wires the contract with its own
    /// `consumer_wiring.<dep>` settings (endpoint, timeout, retry, TLS). One
    /// proxy per contract would hand every consumer the settings of whichever
    /// wired first.
    proxies: RwLock<HashMap<ProxyKey, Boxed>>,
}

/// Type-safe registry of clients keyed by interface type.
///
/// A hub is shared storage plus an optional owner gear. [`Self::for_gear`]
/// returns a view of the same storage owned by a gear; `GearCtx::client_hub`
/// hands each gear its own view. Plain and scoped registrations are shared by
/// all views. Remote proxies registered through a view belong to its owner.
#[derive(Default)]
pub struct ClientHub {
    storage: Arc<Storage>,
    owner: Option<Arc<str>>,
}

impl ClientHub {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A view of this hub's storage owned by `gear`.
    ///
    /// Local and scoped registrations are shared with every other view. A
    /// remote proxy registered through the view is seen only by this view and
    /// does not replace another gear's proxy for the same contract.
    #[must_use]
    pub fn for_gear(&self, gear: impl Into<Arc<str>>) -> Arc<Self> {
        Arc::new(Self {
            storage: Arc::clone(&self.storage),
            owner: Some(gear.into()),
        })
    }

    /// The gear that owns this view, or `None` for the process-wide hub.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    fn proxy_key<T: ?Sized + 'static>(&self) -> ProxyKey {
        ProxyKey {
            type_key: TypeKey::of::<T>(),
            owner: self.owner.clone(),
        }
    }

    /// Local impl first, then this owner's proxy, then the process-wide proxy.
    fn lookup<T>(&self) -> Result<Arc<T>, ClientHubError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let type_key = TypeKey::of::<T>();
        let downcast = |boxed: &Boxed| {
            boxed
                .downcast_ref::<Arc<T>>()
                .cloned()
                .ok_or_else(|| ClientHubError::TypeMismatch {
                    type_key: type_key.clone(),
                })
        };
        if let Some(boxed) = self.storage.map.read().get(&type_key) {
            return downcast(boxed);
        }
        let proxies = self.storage.proxies.read();
        let mut key = ProxyKey {
            type_key: type_key.clone(),
            owner: self.owner.clone(),
        };
        if key.owner.is_some()
            && let Some(boxed) = proxies.get(&key)
        {
            return downcast(boxed);
        }
        key.owner = None;
        match proxies.get(&key) {
            Some(boxed) => downcast(boxed),
            None => Err(ClientHubError::NotFound { type_key }),
        }
    }
}

impl ClientHub {
    /// Register a client under the interface type `T`.
    /// `T` can be a trait object like `dyn my_gear::api::MyClient`.
    ///
    /// The registration is shared by every view and wins over any remote
    /// proxy for `T` in [`Self::get`], also when it is registered after one.
    pub fn register<T>(&self, client: Arc<T>)
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let mut w = self.storage.map.write();
        w.insert(TypeKey::of::<T>(), Box::new(client));
    }

    /// Register a scoped client under the interface type `T`.
    ///
    /// This enables multiple implementations of the same interface to coexist,
    /// distinguished by a caller-defined `ClientScope` (e.g., a GTS instance ID).
    pub fn register_scoped<T>(&self, scope: ClientScope, client: Arc<T>)
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let key = ScopedKey {
            type_key: TypeKey::of::<T>(),
            scope,
        };
        let mut w = self.storage.scoped_map.write();
        w.insert(key, Box::new(client));
    }

    /// Fetch a client by interface type `T`.
    ///
    /// Returns, in order: a local registration, this view's remote proxy, a
    /// process-wide remote proxy.
    ///
    /// # Errors
    /// Returns `ClientHubError::NotFound` if no client is registered for the type.
    /// Returns `ClientHubError::TypeMismatch` if the stored type doesn't match.
    pub fn get<T>(&self) -> Result<Arc<T>, ClientHubError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.lookup::<T>()
    }

    /// Try to fetch a client by interface type `T`.
    ///
    /// Same lookup order as [`Self::get`]; returns `None` if nothing is
    /// registered or the stored type doesn't match. Use [`Self::try_get_local`]
    /// when the answer needs to mean "satisfied in-process".
    #[must_use]
    pub fn try_get<T>(&self) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.lookup::<T>().ok()
    }

    /// Register a client that is a discovery-resolving proxy to a *remote*
    /// provider, not an in-process implementation.
    ///
    /// The proxy belongs to this view's owner, or is process-wide when the hub
    /// has no owner. It never hides a local registration of `T`.
    pub fn register_remote_proxy<T>(&self, client: Arc<T>)
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let key = self.proxy_key::<T>();
        self.storage.proxies.write().insert(key, Box::new(client));
    }

    /// Try to fetch a client by interface type `T`, ignoring remote proxies.
    ///
    /// This is the short-circuit probe for consumer wiring: a compile-time
    /// (local) registration takes priority over a discovery-based proxy, so the
    /// wiring closure asks whether a *local* implementation exists before
    /// registering one of its own. Seeing a proxy here would let a consumer
    /// report its dependency as local and turn `/readyz` green before the
    /// provider exists.
    #[must_use]
    pub fn try_get_local<T>(&self) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let r = self.storage.map.read();
        let boxed = r.get(&TypeKey::of::<T>())?;
        boxed.downcast_ref::<Arc<T>>().cloned()
    }

    /// Whether this view already holds a remote proxy for interface type `T`:
    /// its owner's proxy, or the process-wide one for a hub without an owner.
    ///
    /// Another gear's proxy does not count, so each consumer builds a proxy
    /// from its own wiring settings.
    #[must_use]
    pub fn has_remote_proxy<T>(&self) -> bool
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.storage
            .proxies
            .read()
            .contains_key(&self.proxy_key::<T>())
    }
    /// Fetch a scoped client by interface type `T` and scope.
    ///
    /// # Errors
    /// Returns `ClientHubError::ScopedNotFound` if no client is registered for the `(type, scope)` pair.
    /// Returns `ClientHubError::ScopedTypeMismatch` if the stored type doesn't match.
    pub fn get_scoped<T>(&self, scope: &ClientScope) -> Result<Arc<T>, ClientHubError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let key = ScopedKey {
            type_key: TypeKey::of::<T>(),
            scope: scope.clone(),
        };
        let r = self.storage.scoped_map.read();

        let boxed = r.get(&key).ok_or_else(|| ClientHubError::ScopedNotFound {
            type_key: key.type_key.clone(),
            scope: key.scope.clone(),
        })?;

        if let Some(arc_t) = boxed.downcast_ref::<Arc<T>>() {
            return Ok(arc_t.clone());
        }
        Err(ClientHubError::ScopedTypeMismatch {
            type_key: key.type_key,
            scope: key.scope,
        })
    }

    /// Try to fetch a scoped client by interface type `T` and scope.
    ///
    /// Returns `None` if not found or if the stored type doesn't match.
    #[must_use]
    pub fn try_get_scoped<T>(&self, scope: &ClientScope) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let key = ScopedKey {
            type_key: TypeKey::of::<T>(),
            scope: scope.clone(),
        };
        let r = self.storage.scoped_map.read();
        let boxed = r.get(&key)?;

        boxed.downcast_ref::<Arc<T>>().cloned()
    }

    /// Remove a local client by interface type; returns the removed client if
    /// it was present. Remote proxies are not touched.
    #[expect(
        clippy::must_use_candidate,
        reason = "callers remove for the side effect and may drop the client"
    )]
    pub fn remove<T>(&self) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let type_key = TypeKey::of::<T>();
        let mut w = self.storage.map.write();
        let boxed = w.remove(&type_key)?;
        boxed.downcast::<Arc<T>>().ok().map(|b| *b)
    }

    /// Remove a scoped client by interface type + scope; returns the removed client if it was present.
    #[expect(
        clippy::must_use_candidate,
        reason = "callers remove for the side effect and may drop the client"
    )]
    pub fn remove_scoped<T>(&self, scope: &ClientScope) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let key = ScopedKey {
            type_key: TypeKey::of::<T>(),
            scope: scope.clone(),
        };
        let mut w = self.storage.scoped_map.write();
        let boxed = w.remove(&key)?;
        boxed.downcast::<Arc<T>>().ok().map(|b| *b)
    }

    /// Clear everything (useful in tests).
    pub fn clear(&self) {
        self.storage.map.write().clear();
        self.storage.scoped_map.write().clear();
        self.storage.proxies.write().clear();
    }

    /// Introspection: (total entries).
    #[must_use]
    pub fn len(&self) -> usize {
        self.storage.map.read().len()
            + self.storage.scoped_map.read().len()
            + self.storage.proxies.read().len()
    }

    /// Check if the hub is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.storage.map.read().is_empty()
            && self.storage.scoped_map.read().is_empty()
            && self.storage.proxies.read().is_empty()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_gts::gts_id;

    #[async_trait::async_trait]
    trait TestApi: Send + Sync {
        async fn id(&self) -> usize;
    }

    struct ImplA(usize);
    #[async_trait::async_trait]
    impl TestApi for ImplA {
        async fn id(&self) -> usize {
            self.0
        }
    }

    #[tokio::test]
    async fn register_and_get_dyn_trait() {
        let hub = ClientHub::new();
        let api: Arc<dyn TestApi> = Arc::new(ImplA(7));
        hub.register::<dyn TestApi>(api.clone());

        let got = hub.get::<dyn TestApi>().unwrap();
        assert_eq!(got.id().await, 7);
        assert_eq!(Arc::as_ptr(&api), Arc::as_ptr(&got));
    }

    #[tokio::test]
    async fn remove_works() {
        let hub = ClientHub::new();
        let api: Arc<dyn TestApi> = Arc::new(ImplA(42));
        hub.register::<dyn TestApi>(api);

        assert!(hub.get::<dyn TestApi>().is_ok());

        let removed = hub.remove::<dyn TestApi>();
        assert!(removed.is_some());
        assert!(hub.get::<dyn TestApi>().is_err());
    }

    #[tokio::test]
    async fn try_get_local_ignores_a_remote_proxy() {
        let hub = ClientHub::new();
        let proxy: Arc<dyn TestApi> = Arc::new(ImplA(1));
        hub.register_remote_proxy::<dyn TestApi>(proxy);

        // Reachable for business calls...
        assert!(hub.try_get::<dyn TestApi>().is_some());
        assert!(hub.get::<dyn TestApi>().is_ok());
        // ...but not a local implementation. This is what stops a second
        // consumer of the same contract from reporting `WireOutcome::Local`
        // and marking the dependency readiness-resolved while the provider is
        // still absent.
        assert!(hub.try_get_local::<dyn TestApi>().is_none());
        assert!(hub.has_remote_proxy::<dyn TestApi>());
    }

    #[tokio::test]
    async fn try_get_local_sees_a_plain_registration() {
        let hub = ClientHub::new();
        let local: Arc<dyn TestApi> = Arc::new(ImplA(2));
        hub.register::<dyn TestApi>(local);

        assert!(hub.try_get_local::<dyn TestApi>().is_some());
        assert!(!hub.has_remote_proxy::<dyn TestApi>());
    }

    #[tokio::test]
    async fn a_late_local_registration_wins_over_the_proxy() {
        // A consumer wires a directory proxy under the contract first...
        let hub = ClientHub::new();
        let view = hub.for_gear("orders");
        view.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(1)));
        assert!(view.try_get_local::<dyn TestApi>().is_none());

        // ...then the real in-process provider registers its local impl. It must
        // win in every view, the proxy owner's included.
        hub.register::<dyn TestApi>(Arc::new(ImplA(2)));
        let local = view
            .try_get_local::<dyn TestApi>()
            .expect("the late local registration must be visible as local");
        assert_eq!(local.id().await, 2);
        assert_eq!(view.get::<dyn TestApi>().unwrap().id().await, 2);
    }

    #[tokio::test]
    async fn each_gear_gets_its_own_proxy() {
        let hub = ClientHub::new();
        let a = hub.for_gear("a");
        let b = hub.for_gear("b");
        a.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(1)));

        assert!(a.has_remote_proxy::<dyn TestApi>());
        assert!(!b.has_remote_proxy::<dyn TestApi>());
        assert!(b.try_get::<dyn TestApi>().is_none());
        assert!(hub.try_get::<dyn TestApi>().is_none());

        b.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(2)));
        assert_eq!(a.get::<dyn TestApi>().unwrap().id().await, 1);
        assert_eq!(b.get::<dyn TestApi>().unwrap().id().await, 2);
    }

    #[tokio::test]
    async fn a_view_falls_back_to_the_process_wide_proxy() {
        let hub = ClientHub::new();
        hub.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(1)));
        let view = hub.for_gear("orders");

        assert_eq!(view.get::<dyn TestApi>().unwrap().id().await, 1);
        // It is not this gear's proxy, so wiring builds one of its own.
        assert!(!view.has_remote_proxy::<dyn TestApi>());

        view.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(2)));
        assert_eq!(view.get::<dyn TestApi>().unwrap().id().await, 2);
        assert_eq!(hub.get::<dyn TestApi>().unwrap().id().await, 1);
    }

    #[tokio::test]
    async fn removing_the_local_impl_leaves_the_proxy() {
        let hub = ClientHub::new();
        hub.register_remote_proxy::<dyn TestApi>(Arc::new(ImplA(1)));
        hub.register::<dyn TestApi>(Arc::new(ImplA(2)));

        assert!(hub.remove::<dyn TestApi>().is_some());
        assert!(hub.try_get_local::<dyn TestApi>().is_none());
        assert!(hub.has_remote_proxy::<dyn TestApi>());
        assert_eq!(hub.get::<dyn TestApi>().unwrap().id().await, 1);
    }

    #[test]
    fn views_share_local_and_scoped_registrations() {
        let hub = ClientHub::new();
        let view = hub.for_gear("orders");
        let scope = ClientScope::new("pricing");
        view.register::<str>(Arc::from("local"));
        view.register_scoped::<str>(scope.clone(), Arc::from("scoped"));

        assert_eq!(view.owner(), Some("orders"));
        assert_eq!(hub.owner(), None);
        assert_eq!(&*hub.get::<str>().unwrap(), "local");
        assert_eq!(&*hub.get_scoped::<str>(&scope).unwrap(), "scoped");
        assert_eq!(&*hub.for_gear("billing").get::<str>().unwrap(), "local");
    }

    #[tokio::test]
    async fn overwrite_replaces_atomically() {
        let hub = ClientHub::new();
        hub.register::<dyn TestApi>(Arc::new(ImplA(1)));

        let old = hub.get::<dyn TestApi>().unwrap();
        assert_eq!(old.id().await, 1);

        hub.register::<dyn TestApi>(Arc::new(ImplA(2)));

        let new = hub.get::<dyn TestApi>().unwrap();
        assert_eq!(new.id().await, 2);

        // Old Arc is still valid
        assert_eq!(old.id().await, 1);
    }

    #[tokio::test]
    async fn scoped_register_and_get_dyn_trait() {
        let hub = ClientHub::new();
        let scope_a = ClientScope::gts_id(gts_id!(
            "cf.core.toolkit.plugins.v1~cf.core.tenant_resolver.plugin.v1~contoso.app._.plugin.v1.0"
        ));
        let scope_b = ClientScope::gts_id(gts_id!(
            "cf.core.toolkit.plugins.v1~cf.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0"
        ));

        let api_a: Arc<dyn TestApi> = Arc::new(ImplA(1));
        let api_b: Arc<dyn TestApi> = Arc::new(ImplA(2));

        hub.register_scoped::<dyn TestApi>(scope_a.clone(), api_a.clone());
        hub.register_scoped::<dyn TestApi>(scope_b.clone(), api_b.clone());

        assert_eq!(
            hub.get_scoped::<dyn TestApi>(&scope_a).unwrap().id().await,
            1
        );
        assert_eq!(
            hub.get_scoped::<dyn TestApi>(&scope_b).unwrap().id().await,
            2
        );
    }

    #[test]
    fn scoped_get_is_independent_from_global_get() {
        let hub = ClientHub::new();
        let scope = ClientScope::gts_id(gts_id!(
            "cf.core.toolkit.plugins.v1~cf.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0"
        ));
        hub.register::<str>(Arc::from("global"));
        hub.register_scoped::<str>(scope.clone(), Arc::from("scoped"));

        assert_eq!(&*hub.get::<str>().unwrap(), "global");
        assert_eq!(&*hub.get_scoped::<str>(&scope).unwrap(), "scoped");
    }

    #[test]
    fn try_get_scoped_returns_some_on_hit() {
        let hub = ClientHub::new();
        let scope = ClientScope::gts_id(gts_id!(
            "cf.core.toolkit.plugins.v1~cf.core.tenant_resolver.plugin.v1~contoso.app._.plugin.v1.0"
        ));
        hub.register_scoped::<str>(scope.clone(), Arc::from("scoped"));

        let got = hub.try_get_scoped::<str>(&scope);
        assert_eq!(got.as_deref(), Some("scoped"));
    }

    #[test]
    fn try_get_scoped_returns_none_on_miss() {
        let hub = ClientHub::new();
        let scope = ClientScope::gts_id(gts_id!(
            "cf.core.toolkit.plugins.v1~cf.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0"
        ));

        let got = hub.try_get_scoped::<str>(&scope);
        assert!(got.is_none());
    }
}

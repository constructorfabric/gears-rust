use super::*;

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
    let scope_a = ClientScope::gts_id(
        "gts.x.core.modkit.plugins.v1~x.core.tenant_resolver.plugin.v1~contoso.app._.plugin.v1.0",
    );
    let scope_b = ClientScope::gts_id(
        "gts.x.core.modkit.plugins.v1~x.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0",
    );

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
    let scope = ClientScope::gts_id(
        "gts.x.core.modkit.plugins.v1~x.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0",
    );
    hub.register::<str>(Arc::from("global"));
    hub.register_scoped::<str>(scope.clone(), Arc::from("scoped"));

    assert_eq!(&*hub.get::<str>().unwrap(), "global");
    assert_eq!(&*hub.get_scoped::<str>(&scope).unwrap(), "scoped");
}

#[test]
fn try_get_scoped_returns_some_on_hit() {
    let hub = ClientHub::new();
    let scope = ClientScope::gts_id(
        "gts.x.core.modkit.plugins.v1~x.core.tenant_resolver.plugin.v1~contoso.app._.plugin.v1.0",
    );
    hub.register_scoped::<str>(scope.clone(), Arc::from("scoped"));

    let got = hub.try_get_scoped::<str>(&scope);
    assert_eq!(got.as_deref(), Some("scoped"));
}

#[test]
fn try_get_scoped_returns_none_on_miss() {
    let hub = ClientHub::new();
    let scope = ClientScope::gts_id(
        "gts.x.core.modkit.plugins.v1~x.core.tenant_resolver.plugin.v1~fabrikam.app._.plugin.v1.0",
    );

    let got = hub.try_get_scoped::<str>(&scope);
    assert!(got.is_none());
}

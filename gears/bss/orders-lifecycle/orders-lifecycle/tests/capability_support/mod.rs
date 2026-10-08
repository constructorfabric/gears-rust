pub mod capabilities;
pub mod entity;
pub mod flows;
pub mod history;
pub mod migration;
pub mod pg_container;
pub mod provider;
use sea_orm::{EntityTrait, IntoActiveModel, Set};
use testcontainers_modules::{postgres::Postgres, testcontainers::ContainerAsync};
use toolkit_db::secure::{SecureEntityExt, secure_insert};
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

pub struct Pg {
    pub db: Db,
    pub port: u16,
    _container: ContainerAsync<Postgres>,
}
impl Pg {
    pub async fn new() -> anyhow::Result<Self> {
        let (container, port) = pg_container::start_postgres().await?;
        let db = connect_db(
            &format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
            ConnectOpts::default(),
        )
        .await?;
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            vec![Box::new(migration::Probe)],
        )
        .await?;
        Ok(Self {
            db,
            port,
            _container: container,
        })
    }
    pub async fn role(&self, role: &str) -> anyhow::Result<Db> {
        Ok(connect_db(
            &format!("postgres://{role}:fixture@127.0.0.1:{}/postgres", self.port),
            ConnectOpts::default(),
        )
        .await?)
    }
}
pub fn row(id: u128, resource: u128, seller: u128, payer: u128) -> entity::Model {
    entity::Model {
        order_id: Uuid::from_u128(id),
        resource_tenant_id: Uuid::from_u128(resource),
        seller_tenant_id: Uuid::from_u128(seller),
        payer_tenant_id: Uuid::from_u128(payer),
        version: 1,
        state: "draft".into(),
    }
}
pub async fn exact_scope(row: &entity::Model) -> AccessScope {
    provider::scope(vec![provider::path(vec![
        provider::eq("id", row.order_id),
        provider::eq("resource_tenant_id", row.resource_tenant_id),
        provider::eq("seller_tenant_id", row.seller_tenant_id),
        provider::eq("payer_tenant_id", row.payer_tenant_id),
    ])])
    .await
}
pub async fn insert(db: &Db, row: &entity::Model) -> anyhow::Result<()> {
    secure_insert::<entity::Entity>(
        row.clone().into_active_model(),
        &exact_scope(row).await,
        &db.conn()?,
    )
    .await?;
    Ok(())
}
pub async fn rows(db: &Db, scope: &AccessScope) -> anyhow::Result<Vec<entity::Model>> {
    Ok(entity::Entity::find()
        .secure()
        .scope_with(scope)
        .all(&db.conn()?)
        .await?)
}
pub async fn history_insert(db: &Db, row: &entity::Model) -> anyhow::Result<()> {
    let scope = provider::scope(vec![provider::path(vec![provider::eq("id", row.order_id)])]).await;
    secure_insert::<history::Entity>(
        history::ActiveModel {
            order_id: Set(row.order_id),
            version: Set(row.version),
            old_payer_id: Set(row.payer_tenant_id),
        },
        &scope,
        &db.conn()?,
    )
    .await?;
    Ok(())
}
pub mod evidence;

use std::sync::Arc;

use futures_util::StreamExt;
use toolkit_macros::domain_model;
use toolkit_sdk::odata::{QueryBuilder, items_stream};
use toolkit_security::SecurityContext;
use users_info_sdk::odata::CitySchema;
use users_info_sdk::{CitiesStreamingClientV1, City, UsersInfoError, UsersInfoStream};

use crate::gear::ConcreteAppServices;

#[domain_model]
pub(crate) struct LocalCitiesStreamingClient {
    services: Arc<ConcreteAppServices>,
}

impl LocalCitiesStreamingClient {
    #[must_use]
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

impl CitiesStreamingClientV1 for LocalCitiesStreamingClient {
    fn stream(
        &self,
        ctx: SecurityContext,
        query: QueryBuilder<CitySchema>,
    ) -> UsersInfoStream<City> {
        let services = Arc::clone(&self.services);
        let stream = items_stream(query, move |q| {
            let services = Arc::clone(&services);
            let ctx = ctx.clone();
            async move {
                services
                    .cities
                    .list_cities_page(&ctx, &q)
                    .await
                    .map_err(UsersInfoError::from)
            }
        });
        Box::pin(stream.map(|res| res.map_err(super::pager_to_users_info)))
    }
}

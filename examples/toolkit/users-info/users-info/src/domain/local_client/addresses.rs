use std::sync::Arc;

use futures_util::StreamExt;
use toolkit_macros::domain_model;
use toolkit_sdk::odata::{QueryBuilder, items_stream};
use toolkit_security::SecurityContext;
use users_info_sdk::odata::AddressSchema;
use users_info_sdk::{Address, AddressesStreamingClientV1, UsersInfoError, UsersInfoStream};

use crate::gear::ConcreteAppServices;

#[domain_model]
pub(crate) struct LocalAddressesStreamingClient {
    services: Arc<ConcreteAppServices>,
}

impl LocalAddressesStreamingClient {
    #[must_use]
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

impl AddressesStreamingClientV1 for LocalAddressesStreamingClient {
    fn stream(
        &self,
        ctx: SecurityContext,
        query: QueryBuilder<AddressSchema>,
    ) -> UsersInfoStream<Address> {
        let services = Arc::clone(&self.services);
        let stream = items_stream(query, move |q| {
            let services = Arc::clone(&services);
            let ctx = ctx.clone();
            async move {
                services
                    .addresses
                    .list_addresses_page(&ctx, &q)
                    .await
                    .map_err(UsersInfoError::from)
            }
        });
        Box::pin(stream.map(|res| res.map_err(super::pager_to_users_info)))
    }
}

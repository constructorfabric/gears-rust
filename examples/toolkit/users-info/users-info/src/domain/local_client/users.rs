use std::sync::Arc;

use futures_util::StreamExt;
use toolkit_macros::domain_model;
use toolkit_sdk::odata::{QueryBuilder, items_stream};
use toolkit_security::SecurityContext;
use users_info_sdk::odata::UserSchema;
use users_info_sdk::{User, UsersInfoError, UsersInfoStream, UsersStreamingClientV1};

use crate::gear::ConcreteAppServices;

#[domain_model]
pub(crate) struct LocalUsersStreamingClient {
    services: Arc<ConcreteAppServices>,
}

impl LocalUsersStreamingClient {
    #[must_use]
    pub fn new(services: Arc<ConcreteAppServices>) -> Self {
        Self { services }
    }
}

impl UsersStreamingClientV1 for LocalUsersStreamingClient {
    fn stream(
        &self,
        ctx: SecurityContext,
        query: QueryBuilder<UserSchema>,
    ) -> UsersInfoStream<User> {
        let services = Arc::clone(&self.services);
        let stream = items_stream(query, move |q| {
            let services = Arc::clone(&services);
            let ctx = ctx.clone();
            async move {
                services
                    .users
                    .list_users_page(&ctx, &q)
                    .await
                    .map_err(UsersInfoError::from)
            }
        });
        Box::pin(stream.map(|res| res.map_err(super::pager_to_users_info)))
    }
}

//! The hub's service dispatcher: a [`Routes`] snapshot that later batches
//! extend on the running listener.
//!
//! A request clones the current snapshot and releases the lock before it is
//! served, so a stream in flight keeps the snapshot it started with and an
//! installation never waits for one. An installation builds the next snapshot
//! from the current one plus the new installers only: installers already run
//! are never run again.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use parking_lot::RwLock;
use tonic::service::{Routes, RoutesBuilder};
use toolkit::runtime::GearInstallers;
use tower::ServiceExt as _;

/// See the [module](self).
#[derive(Clone)]
pub struct DynamicRoutes {
    current: Arc<RwLock<Routes>>,
}

impl DynamicRoutes {
    #[must_use]
    pub fn new(routes: Routes) -> Self {
        Self {
            current: Arc::new(RwLock::new(routes)),
        }
    }

    /// Add the services of `gears` to the served routes.
    pub fn install(&self, gears: &[GearInstallers]) {
        let mut builder = RoutesBuilder::from(self.current.read().clone());
        for gear in gears {
            for installer in &gear.installers {
                (installer.register)(&mut builder);
            }
        }
        *self.current.write() = builder.routes();
    }
}

impl tower::Service<http::Request<tonic::body::Body>> for DynamicRoutes {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        // Cloned under the lock, served without it.
        let routes = self.current.read().clone();
        Box::pin(async move { routes.oneshot(request).await })
    }
}

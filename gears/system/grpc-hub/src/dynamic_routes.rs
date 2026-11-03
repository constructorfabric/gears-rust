//! Extensible gRPC route snapshots on one running listener.
//!
//! Requests clone a snapshot and serve without the lock, so installs never
//! wait for in-flight streams. Each install extends the current snapshot
//! using only new installers; earlier installers are not rerun.

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

    /// Add `gears` under one write lock to prevent lost concurrent installs.
    pub fn install(&self, gears: &[GearInstallers]) {
        let mut current = self.current.write();
        let mut builder = RoutesBuilder::from(current.clone());
        for gear in gears {
            for installer in &gear.installers {
                (installer.register)(&mut builder);
            }
        }
        *current = builder.routes();
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

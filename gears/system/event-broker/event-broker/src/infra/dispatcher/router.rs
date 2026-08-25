//! Delivery-routing handler: cache-based lookup, new-group CAS placement,
//! SD-gated failover cache invalidation (`DESIGN.md:1216-1391`). Shell
//! only - the routing algorithm lands with #4345.

#[derive(Debug, Default)]
pub struct DeliveryRouter;

impl DeliveryRouter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

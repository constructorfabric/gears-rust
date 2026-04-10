use crate::descriptor::ContractDescriptor;
use crate::ir::contract::ContractIr;

/// Connects a `dyn Trait` type to its contract descriptor and IR.
pub trait Contract: Send + Sync + 'static {
    /// Returns the static descriptor for fast runtime lookups.
    fn descriptor() -> &'static ContractDescriptor;

    /// Builds the full Contract IR.
    fn contract_ir() -> ContractIr;
}

pub use Contract as ServiceContract;

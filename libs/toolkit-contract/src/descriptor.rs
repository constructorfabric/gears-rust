use serde::{Deserialize, Serialize};

use crate::ir::contract::{Idempotency, MethodKind};

/// Operational classification of a contract trait.
///
/// Encoded in the trait name suffix per PRD #1536 D2/D6:
/// - `Api` — module **provides** the contract; remote-capable.
/// - `Embedded` — module **provides** the contract; always in-process.
/// - `Backend` — module **requires** the contract; remote-capable.
/// - `Extension` — module **requires** the contract; always in-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContractKind {
    /// Provided contract, remote-capable.
    Api,
    /// Provided contract, always local.
    Embedded,
    /// Required contract (consumed by the module), remote-capable.
    Backend,
    /// Required contract (consumed by the module), always local.
    Extension,
}

impl ContractKind {
    /// Whether this kind permits a transport projection (`*Rest`, `*Grpc`).
    #[must_use]
    pub const fn is_remote_capable(self) -> bool {
        matches!(self, ContractKind::Api | ContractKind::Backend)
    }

    /// Human-readable name suitable for diagnostics and trait-suffix matching.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            ContractKind::Api => "Api",
            ContractKind::Embedded => "Embedded",
            ContractKind::Backend => "Backend",
            ContractKind::Extension => "Extension",
        }
    }
}

/// Compile-time static metadata for a contract.
pub struct ContractDescriptor {
    /// Gear name.
    pub gear: &'static str,
    /// Contract name, usually the SDK trait name.
    pub contract: &'static str,
    /// Compatibility service name for old service-hub call sites.
    pub service: &'static str,
    /// API version.
    pub version: &'static str,
    /// Operational classification of this contract.
    pub kind: ContractKind,
    /// Method descriptors for all methods in this contract.
    pub methods: &'static [MethodDescriptor],
}

impl ContractDescriptor {
    /// Compatibility accessor for old service-oriented call sites.
    #[must_use]
    pub const fn service(&self) -> &'static str {
        self.service
    }

    /// Whether the contract permits remote dispatch.
    #[must_use]
    pub const fn is_remote_capable(&self) -> bool {
        self.kind.is_remote_capable()
    }
}

/// Static metadata for a single method within a contract.
pub struct MethodDescriptor {
    /// Method name.
    pub name: &'static str,
    /// Unary or streaming.
    pub kind: MethodKind,
    /// Idempotency classification for retry decisions.
    pub idempotency: Idempotency,
    /// Input type name for diagnostics and logging.
    pub input_type: &'static str,
    /// Output type name for diagnostics and logging.
    pub output_type: &'static str,
}

pub type ServiceDescriptor = ContractDescriptor;

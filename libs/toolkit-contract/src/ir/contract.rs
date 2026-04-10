use serde::{Deserialize, Serialize};

/// Intermediate representation of a complete contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractIr {
    /// Contract name, usually the SDK trait name.
    pub name: String,
    /// Gear that provides this contract.
    pub gear: String,
    /// API version.
    pub version: String,
    /// Methods exposed by this contract.
    pub methods: Vec<MethodIr>,
}

/// Intermediate representation of a single contract method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodIr {
    /// Method name.
    pub name: String,
    /// Whether this method is unary or streaming.
    pub kind: MethodKind,
    /// Input parameters.
    pub input: InputShape,
    /// Output type reference.
    pub output: TypeRef,
    /// Error type reference, if the method is fallible.
    pub error: Option<TypeRef>,
    /// Idempotency classification for retry decisions.
    pub idempotency: Idempotency,
    /// `true` when the trait declares a default body — peers MAY omit
    /// this method (carried as `x-optional` extension in `OpenAPI`).
    #[serde(default)]
    pub optional: bool,
}

/// Whether a method returns a single value or a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MethodKind {
    /// Request -> Response.
    Unary,
    /// Request -> Stream of responses.
    ServerStreaming,
}

/// Shape of a method's input parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputShape {
    /// Ordered list of input fields.
    pub fields: Vec<FieldIr>,
}

/// A single field in an input shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldIr {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: TypeRef,
    /// Whether this field is optional.
    pub optional: bool,
    /// Semantic role of the field. `#[serde(default)]` keeps deserialization
    /// backward-compatible with IR persisted before this field existed.
    #[serde(default)]
    pub role: FieldRole,
}

/// Semantic role of a contract input field. Most fields are `Wire` (sent
/// across the transport boundary). `SecurityContext` is server-injected and
/// must NOT appear in proto wire schemas or in `OpenAPI` request bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FieldRole {
    #[default]
    Wire,
    SecurityContext,
}

/// Reference to a type used in method signatures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TypeRef {
    /// A primitive scalar type.
    Primitive(PrimitiveType),
    /// A named domain type.
    Named(String),
    /// An optional wrapper.
    Optional(Box<TypeRef>),
    /// A list/vector.
    List(Box<TypeRef>),
    /// A key-value map.
    Map(Box<TypeRef>, Box<TypeRef>),
}

/// Primitive scalar types supported in contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrimitiveType {
    /// UTF-8 string.
    String,
    /// 32-bit signed integer.
    I32,
    /// 64-bit signed integer.
    I64,
    /// 64-bit unsigned integer.
    U64,
    /// 64-bit floating point.
    F64,
    /// Boolean.
    Bool,
    /// UUID.
    Uuid,
    /// Raw bytes.
    Bytes,
}

/// Idempotency classification for retry policy decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Idempotency {
    /// Safe read operation — always retriable.
    SafeRead,
    /// Idempotent write — retriable.
    IdempotentWrite,
    /// Non-idempotent write — not retriable without explicit strategy.
    NonIdempotentWrite,
}

pub type ServiceIr = ContractIr;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_role_default_is_wire() {
        assert_eq!(FieldRole::default(), FieldRole::Wire);
    }

    #[test]
    fn field_ir_deserializes_without_role_defaults_to_wire() {
        let json = r#"{
            "name": "amount",
            "ty": { "Primitive": "I64" },
            "optional": false
        }"#;
        let f: FieldIr = serde_json::from_str(json).expect("deserialize FieldIr");
        assert_eq!(f.role, FieldRole::Wire);
        assert_eq!(f.name, "amount");
    }

    #[test]
    fn field_ir_deserializes_with_explicit_role() {
        let json = r#"{
            "name": "ctx",
            "ty": { "Named": "SecurityContext" },
            "optional": false,
            "role": "SecurityContext"
        }"#;
        let f: FieldIr = serde_json::from_str(json).expect("deserialize FieldIr");
        assert_eq!(f.role, FieldRole::SecurityContext);
    }
}

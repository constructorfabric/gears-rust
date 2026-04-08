// simulated_dir=/hyperspot/modules/some_module/contract/
#![allow(dead_code)]

use uuid::Uuid;

// Should trigger DE1303 - transparent alias of primitive
pub type TenantId = Uuid;

// Should trigger DE1303 - transparent alias of primitive
pub type UserId = Uuid;

// Should trigger DE1303 - transparent alias of String
pub type GtsId = String;

// Should trigger DE1303 - transparent alias of primitive
pub type Port = u16;

// Should trigger DE1303 - transparent alias (qualified path, last segment matches)
pub type CorrelationId = uuid::Uuid;

// Should trigger DE1303 - transparent alias (i32 backing type)
pub type Count = i32;

fn main() {}

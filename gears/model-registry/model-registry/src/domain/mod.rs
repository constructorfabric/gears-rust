// The repositories take `toolkit_db::secure::DBRunner` and `DomainError`
// classifies `sea_orm::DbErr`, which DE0301 flags. This is the convention
// across gears, not a deviation pending repair. `unknown_lints` covers plain
// rustc runs, where the dylint lint is not registered.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod access_guard;
pub mod clock;
pub mod credential_check;
pub mod error;
pub mod gts_validator;
pub mod local_client;
pub mod precondition;
pub mod service;
pub mod validation;
pub mod visibility;

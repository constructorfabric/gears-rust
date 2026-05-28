//! Sea-ORM-backed repository implementations.
//!
//! Each module here exposes a `*Repo` trait and a Sea-ORM impl. Services
//! depend on the trait (object-safe `Arc<dyn …>`) so unit tests can swap in
//! in-memory mocks without touching a database.
//
// @cpt-cf-chat-engine-infra-repo-root:p3

pub mod message_repo;
pub mod plugin_config_repo;
pub mod reaction_repo;
pub mod session_repo;
pub mod session_type_repo;
pub mod variant_repo;

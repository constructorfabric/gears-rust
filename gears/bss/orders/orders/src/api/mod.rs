//! The Orders adapters: the REST boundary ([`rest`]) and the in-process SDK client
//! ([`local_client`]), both over the same services and caller rules.
pub mod local_client;
pub mod rest;

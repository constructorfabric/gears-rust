//! Infrastructure layer: persistence, cluster coordination, and GTS
//! provisioning (`DESIGN.md` §1.3 Architecture Layers).

pub mod cluster;
pub mod dispatcher;
pub mod storage;
pub mod type_provisioning;
pub mod workers;

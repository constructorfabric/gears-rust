//! Domain layer of the policy-engine gear: the content model, the management
//! service, the decision path and the ports it reaches its dependencies through.

pub mod decision;
pub mod engine_plugin;
pub mod eval;
pub mod evaluator;
pub mod management;
pub mod model;
pub mod ports;
pub mod repos;
pub mod validation;

pub mod config;
pub mod gear;
pub mod service;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

pub use gear::StaticMiniChatModelPolicyPlugin;

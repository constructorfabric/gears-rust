use std::collections::HashMap;
use uuid::Uuid;

use toolkit::runtime::InstanceState;

use crate::domain::model::{DeploymentMode, GearInfo, InstanceInfo};

/// Deployment mode of a gear
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub enum DeploymentModeDto {
    /// Gear is compiled into the host binary
    CompiledIn,
    /// Gear runs as a separate process
    OutOfProcess,
}

/// Response DTO for a single registered gear
#[toolkit_macros::api_dto(response)]
pub struct GearDto {
    /// Gear name
    pub name: String,
    /// Gear version (if reported by a running instance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Declared capabilities (e.g., "rest", "grpc", "system", "db")
    pub capabilities: Vec<String>,
    /// Gear dependencies (other gear names)
    pub dependencies: Vec<String>,
    /// Whether the gear is compiled-in or out-of-process
    pub deployment_mode: DeploymentModeDto,
    /// Running instances of this gear
    pub instances: Vec<GearInstanceDto>,
    /// Plugins provided by this gear (reserved for follow-up implementation)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginDto>,
}

/// Response DTO for a running gear instance
#[toolkit_macros::api_dto(response)]
pub struct GearInstanceDto {
    /// Unique instance ID
    pub instance_id: Uuid,
    /// Gear version (if reported during registration)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Current instance state (e.g., "registered", "healthy", "quarantined")
    pub state: String,
    /// gRPC services provided by this instance (service name -> endpoint URI)
    pub grpc_services: HashMap<String, String>,
}

/// Response DTO for a plugin (reserved for follow-up implementation)
#[toolkit_macros::api_dto(response)]
pub struct PluginDto {
    /// Plugin GTS identifier
    pub gts_id: String,
    /// Plugin version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl From<&GearInfo> for GearDto {
    fn from(gear: &GearInfo) -> Self {
        // Derive gear-level version from the first instance that reports one
        let version = gear.instances.iter().find_map(|inst| inst.version.clone());

        Self {
            name: gear.name.clone(),
            version,
            capabilities: gear.capabilities.clone(),
            dependencies: gear.dependencies.clone(),
            deployment_mode: match gear.deployment_mode {
                DeploymentMode::CompiledIn => DeploymentModeDto::CompiledIn,
                DeploymentMode::OutOfProcess => DeploymentModeDto::OutOfProcess,
            },
            instances: gear.instances.iter().map(GearInstanceDto::from).collect(),
            plugins: vec![],
        }
    }
}

impl From<&InstanceInfo> for GearInstanceDto {
    fn from(instance: &InstanceInfo) -> Self {
        Self {
            instance_id: instance.instance_id,
            version: instance.version.clone(),
            state: match instance.state {
                InstanceState::Registered => "registered",
                InstanceState::Ready => "ready",
                InstanceState::Healthy => "healthy",
                InstanceState::Quarantined => "quarantined",
                InstanceState::Draining => "draining",
            }
            .to_owned(),
            grpc_services: instance.grpc_services.clone(),
        }
    }
}

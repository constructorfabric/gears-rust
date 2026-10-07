use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistrationState {
    Active,
    Retired,
    Stopping,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnregisterMode {
    Retain,
    CancelAndRelease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnregisterOptions {
    pub mode: UnregisterMode,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub name: String,
    pub generation: u64,
    pub revision: u64,
    pub state: RegistrationState,
}

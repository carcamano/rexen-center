use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{Host, Vm, VmOperations, VmPowerState, de};
use crate::Ref;

/// Subset of the `pool` record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PoolRecord {
    pub uuid: String,
    pub name_label: String,
    pub name_description: String,
    pub master: Ref<Host>,
    pub other_config: HashMap<String, String>,
}

/// Subset of the `host` record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostRecord {
    pub uuid: String,
    pub name_label: String,
    pub name_description: String,
    pub hostname: String,
    pub address: String,
    pub enabled: bool,
    #[serde(rename = "API_version_major", deserialize_with = "de::int")]
    pub api_version_major: i64,
    #[serde(rename = "API_version_minor", deserialize_with = "de::int")]
    pub api_version_minor: i64,
    pub software_version: HashMap<String, String>,
    #[serde(rename = "resident_VMs")]
    pub resident_vms: Vec<Ref<Vm>>,
    pub other_config: HashMap<String, String>,
}

/// Subset of the `VM` record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmRecord {
    pub uuid: String,
    pub name_label: String,
    pub name_description: String,
    pub power_state: VmPowerState,
    pub allowed_operations: Vec<VmOperations>,
    pub current_operations: HashMap<String, VmOperations>,
    pub is_a_template: bool,
    pub is_default_template: bool,
    pub is_a_snapshot: bool,
    pub is_control_domain: bool,
    pub resident_on: Ref<Host>,
    pub affinity: Ref<Host>,
    pub snapshot_of: Ref<Vm>,
    pub other_config: HashMap<String, String>,
}

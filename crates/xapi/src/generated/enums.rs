xapi_enum! {
    /// `vm_power_state`.
    pub enum VmPowerState {
        Halted => "Halted",
        Paused => "Paused",
        Running => "Running",
        Suspended => "Suspended",
    }
}

impl Default for VmPowerState {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

xapi_enum! {
    /// `vm_operations`: values found in `allowed_operations` / `current_operations`.
    pub enum VmOperations {
        Snapshot => "snapshot",
        Clone => "clone",
        Copy => "copy",
        CreateTemplate => "create_template",
        Revert => "revert",
        Checkpoint => "checkpoint",
        SnapshotWithQuiesce => "snapshot_with_quiesce",
        Provision => "provision",
        Start => "start",
        StartOn => "start_on",
        Pause => "pause",
        Unpause => "unpause",
        CleanShutdown => "clean_shutdown",
        CleanReboot => "clean_reboot",
        HardShutdown => "hard_shutdown",
        PowerStateReset => "power_state_reset",
        HardReboot => "hard_reboot",
        Suspend => "suspend",
        Csvm => "csvm",
        Resume => "resume",
        ResumeOn => "resume_on",
        PoolMigrate => "pool_migrate",
        MigrateSend => "migrate_send",
        GetBootRecord => "get_boot_record",
        SendSysrq => "send_sysrq",
        SendTrigger => "send_trigger",
        QueryServices => "query_services",
        Shutdown => "shutdown",
        CallPlugin => "call_plugin",
        ChangingMemoryLive => "changing_memory_live",
        AwaitingMemoryLive => "awaiting_memory_live",
        ChangingDynamicRange => "changing_dynamic_range",
        ChangingStaticRange => "changing_static_range",
        ChangingMemoryLimits => "changing_memory_limits",
        ChangingShadowMemory => "changing_shadow_memory",
        ChangingShadowMemoryLive => "changing_shadow_memory_live",
        ChangingVcpus => "changing_VCPUs",
        ChangingVcpusLive => "changing_VCPUs_live",
        ChangingNvram => "changing_NVRAM",
        AssertOperationValid => "assert_operation_valid",
        DataSourceOp => "data_source_op",
        UpdateAllowedOperations => "update_allowed_operations",
        MakeIntoTemplate => "make_into_template",
        Import => "import",
        Export => "export",
        MetadataExport => "metadata_export",
        Reverting => "reverting",
        Destroy => "destroy",
        CreateVtpm => "create_vtpm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_are_tolerated() {
        let s: VmPowerState = serde_json::from_str("\"Running\"").unwrap();
        assert_eq!(s, VmPowerState::Running);
        let s: VmPowerState = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(s, VmPowerState::Running);
        let s: VmPowerState = serde_json::from_str("\"Crashed\"").unwrap();
        assert_eq!(s, VmPowerState::Unknown("Crashed".into()));
        assert_eq!(
            serde_json::to_string(&VmOperations::ChangingVcpus).unwrap(),
            "\"changing_VCPUs\""
        );
    }
}

//! Titles and descriptions of XAPI messages (alerts), from the original
//! console's `XenModel/FriendlyNames.resx` (`Message.name-*` and
//! `Message.body-*`; XenAdmin 25.04, BSD-2-Clause). Keys are the message
//! names in lowercase; `{0}`, `{1}` stand for the object's and the pool's
//! names.
//!
//! Generated: sorted by key (byte order) for binary search; do not edit by
//! hand.

/// `(message name, title)`, sorted by name.
pub static MESSAGE_NAMES: &[(&str, &str)] = &[
    (
        "SRUnavailable",
        "Cannot reclaim freed space because the SR is already busy",
    ),
    (
        "UnknownTrimException",
        "An unknown error occurred when reclaiming freed space",
    ),
    (
        "UnsupportedSRForTrim",
        "Reclaiming freed space not supported on this SR",
    ),
    ("alarm", "Unrecognized alarm"),
    ("bond_status_changed", "Bond status has changed"),
    (
        "cluster_host_enable_failed",
        "Server failed to join the cluster",
    ),
    ("cluster_host_fencing", "Server has been fenced"),
    (
        "extauth_in_pool_is_non_homogeneous",
        "Authentication method is not consistent across the Pool",
    ),
    (
        "extauth_init_in_host_failed",
        "Active Directory authorization is not available",
    ),
    ("gfs2_capacity", "Storage repository low on space"),
    ("grace_license", "Grace license issued."),
    (
        "ha_heartbeat_approaching_timeout",
        "HA heartbeat approaching timeout",
    ),
    ("ha_host_failed", "Server failed"),
    ("ha_host_was_fenced", "Server recovered"),
    ("ha_network_bonding_error", "HA network bonding error"),
    (
        "ha_pool_drop_in_plan_exists_for",
        "Drop in HA failover tolerance",
    ),
    ("ha_pool_overcommitted", "Drop in HA failover tolerance"),
    (
        "ha_protected_vm_restart_failed",
        "HA-protected VM failed to restart",
    ),
    (
        "ha_statefile_approaching_timeout",
        "Heartbeat SR approaching timeout",
    ),
    ("ha_statefile_lost", "Heartbeat SR lost"),
    (
        "ha_xapi_healthcheck_approaching_timeout",
        "Server healthcheck approaching timeout",
    ),
    ("host_clock_skew_detected", "Server clock not synchronized"),
    (
        "host_clock_went_backwards",
        "Server clock has gone backwards",
    ),
    ("host_cpu_features_down", "Host CPU features reduced"),
    ("host_cpu_features_up", "Host CPU features increased"),
    (
        "host_sync_data_failed",
        "Server statistics synchronization failed",
    ),
    (
        "ip_configured_pif_can_unplug",
        "Network configuration issue",
    ),
    (
        "license_does_not_support_pooling",
        "License does not support pooling",
    ),
    ("license_expired", "License Expired."),
    ("license_expires_soon", "License expires soon"),
    ("license_not_available", "A license was not available."),
    (
        "license_server_connected",
        "License server connection successful.",
    ),
    (
        "license_server_unavailable",
        "Server disconnected from license server.",
    ),
    (
        "license_server_unreachable",
        "The license server could not be reached.",
    ),
    (
        "license_server_version_obsolete",
        "The license server is an out-of-date version.",
    ),
    ("metadata_lun_broken", "Lost access to HA metadata disk"),
    (
        "metadata_lun_healthy",
        "Restored access to HA metadata disk",
    ),
    ("multipath_periodic_alert", "Multipath Alert"),
    (
        "pbd_plug_failed_on_server_start",
        "Failed to attach storage on server start",
    ),
    ("pool_cpu_features_down", "Pool CPU features reduced"),
    ("pool_cpu_features_up", "Pool CPU features increased"),
    ("pool_master_transition", "Pool coordinator changed"),
    (
        "pvs_proxy_no_cache_sr_available",
        "No cache storage available for PVS site",
    ),
    ("pvs_proxy_setup_failed", "PVS proxy setup failed"),
    (
        "tls_verification_emergency_disabled",
        "Certificate verification is disabled.",
    ),
    ("vbd_qos_failed", "VM Disk QoS Failed"),
    ("vcpu_qos_failed", "VM vCPU QoS Failed"),
    (
        "vdi_cbt_metadata_inconsistent",
        "Changed block tracking disabled due to inconsistent metadata",
    ),
    (
        "vdi_cbt_resize_failed",
        "Changed block tracking disabled due to a virtual disk resizing failure",
    ),
    (
        "vdi_cbt_snapshot_failed",
        "Changed block tracking disabled due to a snapshot failure",
    ),
    ("vif_qos_failed", "VM Network Interface QoS Failed"),
    (
        "vm_checkpointed",
        "Snapshot of disks and memory for virtual machine has been created",
    ),
    ("vm_cloned", "VM Cloned"),
    ("vm_crashed", "VM Crashed"),
    ("vm_migrated", "Virtual machine has been migrated"),
    ("vm_paused", "Virtual machine has been paused"),
    ("vm_rebooted", "VM Rebooted"),
    ("vm_resumed", "VM Resumed"),
    (
        "vm_secure_boot_failed",
        "VM Secure Boot verification failed",
    ),
    ("vm_shutdown", "VM Shut down"),
    (
        "vm_snapshot_reverted",
        "Virtual machine has been reverted to snapshot",
    ),
    (
        "vm_snapshotted",
        "Snapshot of disks for virtual machine has been created",
    ),
    ("vm_started", "VM Started"),
    ("vm_suspended", "VM Suspended"),
    ("vm_uncooperative", "VM Uncooperative"),
    ("vm_unpaused", "Virtual machine has been unpaused"),
    (
        "vmss_license_error",
        "Snapshot schedule failed: insufficient license for Scheduled snapshots",
    ),
    ("vmss_snapshot_failed", "Snapshot schedule failed"),
    ("vmss_snapshot_lock_failed", "Snapshot schedule failed"),
    ("vmss_snapshot_missed_event", "Snapshot schedule failed"),
    ("vmss_snapshot_succeeded", "Snapshot schedule succeeded"),
    (
        "vmss_xapi_logon_failure",
        "Snapshot schedule XAPI logon failure",
    ),
    ("wlb_consultation_failed", "WLB consultation has failed"),
    ("wlb_host_power_off", "WLB Host Power Off"),
    ("wlb_host_power_on", "WLB Host Power On"),
    ("wlb_optimization_alert", "WLB optimization alert"),
    ("wlb_server_time_discrepancy", "WLB Server Time Discrepancy"),
    ("wlb_vm_relocation", "WLB VM Relocation"),
];

/// `(message name, description template)`, sorted by name.
pub static MESSAGE_BODIES: &[(&str, &str)] = &[
    ("alarm", "Alarm '{0}' was not recognized."),
    (
        "cluster_host_enable_failed",
        "Server '{0}' failed to join the cluster.",
    ),
    ("cluster_host_fencing", "Server '{0}' has been fenced."),
    (
        "extauth_in_pool_is_non_homogeneous",
        "The external authentication service used within '{0}' is not consistent.",
    ),
    (
        "extauth_init_in_host_failed",
        "Server '{0}' has failed to connect to the Active Directory server.\n\n{1}",
    ),
    (
        "gfs2_capacity",
        "Space utilization on SR '{0}' has exceeded 80%.",
    ),
    (
        "grace_license",
        "A grace license was issued for {0}. This was because the license server could not be reached, it is an out-of-date version, or because the host has recently been upgraded.",
    ),
    (
        "ha_heartbeat_approaching_timeout",
        "Server '{0}' is taking too long to respond to the HA networking heartbeat and may become fenced.",
    ),
    ("ha_host_failed", "Server '{0}' has failed."),
    (
        "ha_host_was_fenced",
        "Server '{0}' was fenced, and has now successfully restarted and rejoined the pool.",
    ),
    (
        "ha_network_bonding_error",
        "One of the paths in network bond '{0}' on server '{1}' has failed. HA protection is at risk without it.",
    ),
    (
        "ha_pool_drop_in_plan_exists_for-0",
        "The failover tolerance for pool '{0}' has dropped to 0, and HA is no longer guaranteed.",
    ),
    (
        "ha_pool_drop_in_plan_exists_for-1",
        "The failover tolerance for pool '{0}' has dropped. HA is now only guaranteed for 1 host failure.",
    ),
    (
        "ha_pool_drop_in_plan_exists_for-n",
        "The failover tolerance for pool '{0}' has dropped. HA is now only guaranteed for {1} host failures.",
    ),
    (
        "ha_pool_overcommitted",
        "The maximum number of server failures tolerable in pool '{0}' has fallen below the requested value.",
    ),
    (
        "ha_protected_vm_restart_failed",
        "Virtual machine '{0}', which is protected under HA, has failed to start.",
    ),
    (
        "ha_statefile_approaching_timeout",
        "Server '{0}' is taking too long to write to the heartbeat storage and may become fenced.",
    ),
    (
        "ha_statefile_lost",
        "The servers in pool '{0}' cannot access the heartbeat SR. HA protection is severely impaired without it.",
    ),
    (
        "ha_xapi_healthcheck_approaching_timeout",
        "Server '{0}' is taking too long to respond and may become fenced.",
    ),
    (
        "host_clock_skew_detected",
        "The clock on server '{0}' is not synchronized with the other servers in pool '{1}'. This can lead to errors when migrating virtual machines, and also affect any performance data collected in the future.",
    ),
    (
        "host_clock_went_backwards",
        "The server clock on '{0}' has gone backwards.",
    ),
    (
        "host_cpu_features_down",
        "The CPU feature level on host '{0}' has been reduced.",
    ),
    (
        "host_cpu_features_up",
        "The CPU feature level on host '{0}' has been increased.",
    ),
    (
        "host_sync_data_failed",
        "There was a temporary failure synchronizing performance statistics across the pool, probably because one or more servers were offline. Another synchronization attempt will be made later.",
    ),
    (
        "license_does_not_support_pooling",
        "The license on server '{0}' has expired or been changed, pooling is no longer supported.",
    ),
    ("license_expired", "The license for {0} has expired."),
    (
        "license_expires_soon",
        "The license on host '{0}' will expire within 30 days.",
    ),
    (
        "license_not_available",
        "The requested license is not available for {0}.",
    ),
    (
        "license_server_connected",
        "The server {0} has successfully connected to the license server.",
    ),
    (
        "license_server_unavailable",
        "The server {0} has disconnected from the license server.",
    ),
    (
        "license_server_unreachable",
        "A license for {0} could not be checked-out because the license server could not be reached at the given address and port. Please check the connection details, and verify that the license server is running.",
    ),
    (
        "license_server_version_obsolete",
        "The license server for {0} is an out-of-date version.",
    ),
    (
        "metadata_lun_broken",
        "Pool '{0}' is unable to access the HA metadata disk. Until access is restored changes to the pool metadata may be lost in the event of an HA failover.",
    ),
    (
        "metadata_lun_healthy",
        "Pool '{0}' successfully accessed the HA metadata disk after a previous failure and can perform a successful recovery in the event of an HA failover.",
    ),
    (
        "multipath_periodic_alert",
        "Server '{0}' has experienced a change in multipath status. Currently {1} out of {2} paths are active. See the application logs for the full details.",
    ),
    (
        "multipath_periodic_alert_healthy",
        "Servers in pool '{0}' have experienced a change in multipath status. Currently all paths are active. See the application logs for the full details.",
    ),
    (
        "multipath_periodic_alert_healthy_standalone",
        "Server '{0}' has experienced a change in multipath status. Currently all paths are active. See the application logs for the full details.",
    ),
    (
        "multipath_periodic_alert_summary",
        "Servers in pool '{0}' have experienced a change in multipath status. Currently the following servers have inactive paths: {1}. See the application logs for the full details.",
    ),
    (
        "pbd_plug_failed_on_server_start",
        "A storage repository could not be attached when server '{0}' started.",
    ),
    (
        "pool_cpu_features_down",
        "The CPU feature level on pool '{0}' has been reduced.",
    ),
    (
        "pool_cpu_features_up",
        "The CPU feature level on pool '{0}' has been increased.",
    ),
    (
        "pool_master_transition",
        "Server '{0}' is now the coordinator of pool '{1}'.",
    ),
    (
        "pvs_proxy_no_cache_sr_available",
        "Unable to setup PVS-Accelerator on VM '{0}'. No cache storage found on site '{1}'.",
    ),
    (
        "tls_verification_emergency_disabled",
        "Certificate verification is enabled on the pool, but overridden on server '{0}'.",
    ),
    (
        "vbd_qos_failed",
        "Quality of Service settings for disk '{0}' on virtual machine '{1}' could not be obeyed.",
    ),
    (
        "vcpu_qos_failed",
        "Quality of Service settings for VCPU '{0}' on virtual machine '{1}' could not be obeyed.",
    ),
    (
        "vdi_cbt_metadata_inconsistent",
        "Changed block tracking has been disabled for virtual disk {0} due to inconsistent metadata. To continue using changed block tracking with this virtual disk, you will need to do the backup setup again, including re-enabling changed block tracking, taking a snapshot and backing it up.",
    ),
    (
        "vdi_cbt_resize_failed",
        "Changed block tracking has been disabled for virtual disk {0} due to a resizing failure. To continue using changed block tracking with this virtual disk, you will need to do the backup setup again, including re-enabling changed block tracking, taking a snapshot and backing it up.",
    ),
    (
        "vdi_cbt_snapshot_failed",
        "Changed block tracking has been disabled for virtual disk {0} due to a snapshot failure. To continue using changed block tracking with this virtual disk, you will need to do the backup setup again, including re-enabling changed block tracking, taking a snapshot and backing it up.",
    ),
    (
        "vif_qos_failed",
        "Quality of Service settings for network interface '{0}' on virtual machine '{1}' could not be obeyed.",
    ),
    ("vm_cloned", "Virtual machine '{0}' has been cloned."),
    ("vm_crashed", "Virtual machine '{0}' has crashed"),
    ("vm_rebooted", "Virtual machine '{0}' has been rebooted."),
    ("vm_resumed", "Virtual machine '{0}' has been resumed."),
    (
        "vm_secure_boot_failed",
        "Virtual machine '{0}' failed to pass Secure Boot verification.",
    ),
    ("vm_shutdown", "Virtual machine '{0}' has been shut down."),
    ("vm_started", "Virtual machine '{0}' has been started."),
    ("vm_suspended", "Virtual machine '{0}' has been suspended."),
    (
        "vm_uncooperative",
        "Virtual machine '{0}' did not reduce its memory usage when requested.",
    ),
    (
        "vmss_license_error",
        "The server license you are using does not allow you to use the Scheduled Snapshots feature.",
    ),
    (
        "vmss_snapshot_failed",
        "The snapshot schedule '{0}' failed because a snapshot could not be created.",
    ),
    (
        "vmss_snapshot_lock_failed",
        "Unable to create snapshots at this time because a scheduled snapshot operation is already in progress in this pool. Only one scheduled snapshot operation can run at a time in the same pool. Please check that the snapshot schedules in this pool do not clash.",
    ),
    (
        "vmss_snapshot_missed_event",
        "Unable to create snapshots at this time because the scheduled snapshot operation for this policy is already in progress. Please try again later.",
    ),
    ("vmss_snapshot_succeeded", "Snapshot schedule succeeded"),
    (
        "vmss_xapi_logon_failure",
        "Snapshot schedule XAPI logon failure",
    ),
    (
        "wlb_consultation_failed",
        "Pool '{0}' failed to retrieve placement recommendations from WLB for '{1}'.",
    ),
    (
        "wlb_optimization_alert",
        "WLB reports that the pool '{0}' is in need of optimization. The pool optimization mode is set to '{1}' and the severity of the alert is '{2}'.",
    ),
];

//! User-interface strings of the Performance tab, the Alerts view and the
//! performance alert settings.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's (`Messages.resx`,
//! `FriendlyNames.resx`, `PerformancePage.resx`, `AlertSummaryPage.resx`,
//! `PerfmonAlertEditPage.resx`). Templates use `{name}` placeholders
//! filled by [`crate::i18n::fill`].

macro_rules! perf_strings {
    ($( $(#[$doc:meta])* $field:ident = $en:literal, )*) => {
        /// One language's strings.
        pub struct PerfStrings {
            $( $(#[$doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: PerfStrings = PerfStrings {
            $( $field: $en, )*
        };
    };
}

perf_strings! {
    tab_performance = "Performance",
    hdr_performance = "Performance Graphs",
    zoom = "Zoom",
    zoom_10_minutes = "10 Minutes",
    zoom_hour = "1 Hour",
    zoom_2_hours = "2 Hours",
    zoom_day = "1 Day",
    zoom_week = "1 Week",
    zoom_month = "1 Month",
    zoom_year = "1 Year",
    /// Shown next to the zoom button: the range drawn.
    showing = "Last {range}",
    graph_cpu = "CPU Performance",
    graph_memory = "Memory Performance",
    graph_network = "Network Performance",
    graph_disk = "Disk Performance",
    graph_loading = "Loading Data...",
    graph_no_data = "No data available",
    graph_needs_tools = "To view memory data for this VM, you need to install XCP-ng VM Tools",
    perf_not_running = "This VM is not running: the graphs show the data recorded until it stopped.",
    perf_disconnected = "The server is not connected.",
    perf_failed = "Could not read the performance data ({error}); trying again...",
    perf_gone = "This object no longer exists.",
    events_label = "VM Lifecycle Events",
    alert_settings = "Alert Settings...",

    ds_cpu = "CPU {0}",
    ds_cpu_avg = "Average CPU",
    ds_cpu_usage = "CPU Usage",
    ds_used_memory = "Used Memory",
    ds_total_memory = "Total Memory",
    ds_nic_rx = "NIC {0} Receive",
    ds_nic_tx = "NIC {0} Send",
    ds_vif_rx = "{0} Receive",
    ds_vif_tx = "{0} Send",
    ds_vbd_read = "Disk {0} Read",
    ds_vbd_write = "Disk {0} Write",
    ds_loadavg = "Control Domain Load",

    alerts_title = "Alerts",
    alerts_unread = "Alerts ({count})",
    alerts_empty = "There are no alerts.",
    alerts_col_severity = "Severity",
    alerts_col_message = "Message",
    alerts_col_location = "Server / Pool",
    alerts_col_date = "Date",
    alerts_col_actions = "Actions",
    alerts_dismiss = "Dismiss",
    alerts_dismiss_all = "Dismiss All",
    alerts_dismiss_selected = "Dismiss Selected",
    alerts_alarm_action = "Alarm Settings",
    alerts_confirm_title = "Dismiss Alerts",
    alerts_confirm_all = "This operation will remove permanently all alerts from every connected server. Do you want to continue?",
    alerts_confirm_selected = "This operation will remove the selected alerts from the servers permanently. Do you want to continue?",
    alerts_confirm_yes_all = "Yes, Dismiss all",
    alerts_confirm_yes = "Yes",
    alerts_confirm_no = "No",
    prio_critical = "Critical",
    prio_major = "Major",
    prio_warning = "Warning",
    prio_minor = "Minor",
    prio_information = "Information",
    prio_unknown = "Unknown",
    op_dismiss_one = "Dismissing alert '{title}'",
    op_dismiss_many = "Dismissing {count} alerts",
    op_save_alerts = "Saving alerts for '{object}'",

    alarm_cpu = "CPU Usage Alarm",
    alarm_cpu_desc = "CPU usage on {object} has been on average {value} for the last {period}.",
    alarm_network = "Network Usage Alarm",
    alarm_network_desc = "Network usage on {object} has been on average {value} for the last {period}.",
    alarm_disk = "Disk Usage Alarm",
    alarm_disk_desc = "Disk usage on {object} has been on average {value} for the last {period}.",
    alarm_memory = "Memory Usage Alarm",
    alarm_memory_desc = "Free memory on {object} has been {value} for the last {period}.",
    alarm_storage = "Storage Throughput Alarm",
    alarm_storage_desc = "The total read and write throughput of {object} on storage repository '{sr}' has been {value} for the last {period}.",
    alarm_fs = "File System on Control Domain Full",
    alarm_fs_desc = "Disk usage for the Control Domain on {object} has reached {value}. XCP-ng's performance will be critically affected if this disk becomes full. Log files or other non-essential (user created) files should be removed.",
    alarm_dom0_memory = "Control Domain Memory Usage",
    alarm_dom0_memory_desc = "The memory required by the control domain on {object} is {value} of its allocated memory. Occasional performance degradation can be expected when memory swapping is forced to happen.",
    alarm_log_fs = "File System on Logs Partition Full",
    alarm_log_fs_desc = "The disk usage for the Logs Partition on {object} has reached {value}. Logging will be critically affected if this disk becomes full. Old log files should be removed.",

    failed_login_title = "There has been a number of failed attempts to login to '{pool}' within the past hour.",
    failed_login_all_unknown = "All {count} attempts originated from unknown sources.",
    failed_login_total = "There has been a total of {count} failed login attempts.",
    failed_login_unknown = "{count} attempts originated from unknown sources.",
    failed_login_common = "The most common attempts originated from the following sources:",
    failed_login_ip = "IP address: {0}",
    failed_login_number = "Number of attempts: {0}",
    failed_login_date = "Last attempt on: {0}",
    failed_login_username = "Username: {0}",
    failed_login_originator = "Originator: {0}",
    failed_login_useragent = "User agent: {0}",

    pm_title = "Alerts - {object}",
    pm_rubric = "The server can notify you when your resource usage crosses certain thresholds. These notifications are generated as system alerts and can be forwarded to an email server the same as any other System Alert.",
    pm_cpu_check = "Generate CPU usage alerts",
    pm_cpu_level = "When CPU usage exceeds:",
    pm_net_check = "Generate network usage alerts",
    pm_net_level = "When network usage exceeds:",
    pm_disk_check = "Generate disk usage alerts",
    pm_disk_level = "When disk usage exceeds:",
    pm_memory_check = "Generate memory usage alerts",
    pm_memory_level = "When free memory falls below:",
    pm_sr_check = "Generate storage throughput alerts",
    pm_sr_level = "When SR throughput for any host exceeds:",
    pm_longer = "For longer than:",
    pm_minutes = "minutes",
    pm_percent = "%",
    pm_kbs = "KB/s",
    pm_mb = "MB",
    pm_interval = "Alert repeat interval:",
    pm_interval_multiple = "Interval must be a multiple of 5",
    pm_ok = "OK",
    pm_cancel = "Cancel",
    pm_none = "None defined",
    pm_sub_cpu = "When CPU usage exceeds {level}% for {minutes} min(s)",
    pm_sub_net = "When network usage exceeds {level} for {minutes} min(s)",
    pm_sub_disk = "When disk usage exceeds {level} for {minutes} min(s)",
    pm_sub_memory = "When free memory falls below {level} for {minutes} min(s)",
    pm_sub_sr = "When storage throughput exceeds {level} for {minutes} min(s)",
}

/// The strings of the current language.
pub fn perf_strings() -> &'static PerfStrings {
    &EN
}

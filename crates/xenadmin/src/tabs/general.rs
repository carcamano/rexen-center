//! The General tab (`GeneralTabPage.cs`): sections of label/value rows.
//!
//! Sections appear in the original's top-to-bottom order. Left out on
//! purpose: Custom Fields (XenCenter's XML in `gui_config`), Read Caching,
//! Docker, licensing and update sections (empty or Citrix-only in 25.04).

use xapi::{
    CertificateType, Host, HostRecord, PoolRecord, Ref, SrRecord, VbdType, Vm, VmPowerState,
    VmRecord,
};
use xen_core::format::{datetime_utc, disk_size, memory_size};
use xen_core::objects::{self, VmKind};

use super::{Ctx, Row, Section, pool_display_name, pretty_duration, vm_display_name, yes_no};
use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// Drops empty sections.
fn non_empty(sections: Vec<Section>) -> Vec<Section> {
    sections
        .into_iter()
        .filter(|s| !s.rows.is_empty())
        .collect()
}

fn none_if_empty(value: String) -> String {
    if value.is_empty() {
        tab_strings().none.to_owned()
    } else {
        value
    }
}

fn tags_value(tags: &[String]) -> String {
    let mut tags = tags.to_vec();
    tags.sort();
    none_if_empty(tags.join(", "))
}

/// `other_config["folder"]` as a path ("Production › Web").
fn folder_value(other_config: &std::collections::HashMap<String, String>) -> String {
    let path = other_config.get("folder").map(String::as_str).unwrap_or("");
    none_if_empty(
        path.split('/')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" › "),
    )
}

/// Name, description, tags and folder: the start of every General section.
fn common_rows(
    name: String,
    description: Option<&str>,
    tags: &[String],
    other_config: &std::collections::HashMap<String, String>,
) -> Vec<Row> {
    let t = tab_strings();
    let mut rows = vec![Row::new(t.name, name)];
    if let Some(description) = description {
        rows.push(Row::new(t.description, description));
    }
    rows.push(Row::new(t.tags, tags_value(tags)));
    rows.push(Row::new(t.folder, folder_value(other_config)));
    rows
}

/// A `boot_time`-style `other_config` value (Unix seconds as a float).
fn unix_float(value: Option<&String>) -> Option<i64> {
    value?.trim().parse::<f64>().ok().map(|v| v as i64)
}

/// A `software_version["date"]` value: ISO 8601 or `yyyy-MM-dd`.
fn build_date(raw: &str) -> String {
    if let Some(secs) = xapi::DateTime(raw.to_owned())
        .unix_seconds()
        .filter(|_| raw.contains('T'))
    {
        return datetime_utc(secs, true);
    }
    let parts: Vec<&str> = raw.split(['-', '.']).collect();
    if let [y, m, d] = parts.as_slice()
        && let (Ok(y), Ok(m), Ok(d)) = (y.parse(), m.parse(), d.parse())
        && let Some(date) = xen_core::format::date(y, m, d)
    {
        return date;
    }
    raw.to_owned()
}

/// "XCP-ng 8.3" from a host's `software_version`.
pub fn host_version(host: &HostRecord) -> String {
    let sv = &host.software_version;
    let brand = sv.get("product_brand").map(String::as_str).unwrap_or("");
    let version = sv
        .get("product_version_text")
        .or_else(|| sv.get("product_version"))
        .map(String::as_str)
        .unwrap_or("");
    format!("{brand} {version}").trim().to_owned()
}

// ---- VM --------------------------------------------------------------------

/// General tab of a VM, template or snapshot.
pub fn vm_sections(ctx: Ctx<'_>, vm: &VmRecord) -> Vec<Section> {
    non_empty(vec![
        Section {
            title: tab_strings().sec_general,
            rows: vm_general_rows(ctx, vm),
        },
        Section {
            title: tab_strings().sec_boot,
            rows: vm_boot_rows(ctx, vm),
        },
        Section {
            title: tab_strings().sec_ha,
            rows: vm_ha_rows(ctx, vm),
        },
        Section {
            title: tab_strings().sec_cpus,
            rows: vm_cpu_rows(vm),
        },
        Section {
            title: tab_strings().sec_device_security,
            rows: vm_device_security_rows(vm),
        },
    ])
}

fn vm_general_rows(ctx: Ctx<'_>, vm: &VmRecord) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let kind = objects::vm_kind(vm);
    let default_template = kind == VmKind::DefaultTemplate;
    let description = (!default_template || !vm.name_description.is_empty())
        .then_some(vm.name_description.as_str());
    let mut rows = common_rows(vm_display_name(vm), description, &vm.tags, &vm.other_config);

    rows.push(Row::new(
        t.os,
        objects::vm_os_name(m, vm).unwrap_or_else(|| t.unknown.to_owned()),
    ));
    rows.push(Row::new(
        t.virt_mode,
        if objects::vm_is_hvm(vm) {
            t.virt_hvm
        } else {
            t.virt_pv
        },
    ));
    if !default_template {
        let bios = &vm.bios_strings;
        let xen = bios.get("bios-vendor").map(String::as_str) == Some("Xen")
            && bios.get("system-manufacturer").map(String::as_str) == Some("Xen");
        rows.push(Row::new(t.bios_copied, yes_no(!bios.is_empty() && !xen)));
    }
    if let Some(appliance) = m.get(&vm.appliance) {
        rows.push(Row::new(t.vapp, appliance.name_label.clone()));
    }
    if vm.is_a_snapshot {
        rows.push(Row::new(
            t.snapshot_of,
            m.get(&vm.snapshot_of)
                .map(|p| p.name_label.clone())
                .unwrap_or_default(),
        ));
        if let Some(secs) = vm.snapshot_time.unix_seconds().filter(|s| *s > 0) {
            rows.push(Row::new(t.creation_time, datetime_utc(secs, true)));
        }
    }
    if !vm.is_a_template {
        if let Some(row) = virtualization_state(ctx, vm) {
            rows.push(row);
        }
        let up = matches!(
            vm.power_state,
            VmPowerState::Running | VmPowerState::Paused | VmPowerState::Suspended
        );
        if up
            && let Some(start) = m
                .get(&vm.metrics)
                .and_then(|metrics| metrics.start_time.unix_seconds())
                .filter(|s| *s > 0)
        {
            rows.push(Row::new(t.vm_uptime, pretty_duration(ctx.now - start)));
        }
        if vm_on_shared_storage(ctx, vm) {
            rows.push(Row::new(
                t.home_server,
                m.get(&vm.affinity)
                    .map(|h| h.name_label.clone())
                    .unwrap_or_else(|| t.none.to_owned()),
            ));
        }
    }
    rows.push(Row::new(t.uuid, vm.uuid.clone()));
    rows
}

/// Every disk on shared storage, or no disks and no local CD: the VM can
/// start anywhere, so its home server matters.
fn vm_on_shared_storage(ctx: Ctx<'_>, vm: &VmRecord) -> bool {
    let m = ctx.model;
    let mut has_disk = false;
    let mut local_cd = false;
    for vbd in vm.vbds.iter().filter_map(|r| m.get(r)) {
        let sr = m.get(&vbd.vdi).and_then(|vdi| m.get(&vdi.sr));
        if vbd.r#type == VbdType::Cd {
            local_cd |= sr.is_some_and(|sr| objects::is_local_sr(sr));
            continue;
        }
        has_disk = true;
        if !sr.is_some_and(|sr| sr.shared) {
            return false;
        }
    }
    has_disk || !local_cd
}

/// Windows guests get the "new" multi-line state (`VM.HasNewVirtualisationStates`).
fn is_windows(ctx: Ctx<'_>, vm: &VmRecord) -> bool {
    if !objects::vm_is_hvm(vm) || vm.platform.get("viridian").map(String::as_str) != Some("true") {
        return false;
    }
    let Some(gm) = ctx.model.get(&vm.guest_metrics) else {
        return true;
    };
    let distro = gm.os_version.get("distro").map(|s| s.to_lowercase());
    let linux = [
        "debian",
        "rhel",
        "fedora",
        "centos",
        "scientific",
        "oracle",
        "sles",
        "lsb",
        "boot2docker",
        "freebsd",
        "ubuntu",
        "suse",
        "gentoo",
        "almalinux",
        "rocky",
    ];
    if distro.is_some_and(|d| linux.iter().any(|l| d.contains(l))) {
        return false;
    }
    !gm.os_version
        .get("uname")
        .is_some_and(|u| u.to_lowercase().contains("netscaler"))
}

fn virtualization_state(ctx: Ctx<'_>, vm: &VmRecord) -> Option<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let running = vm.power_state == VmPowerState::Running;
    let gm = m.get(&vm.guest_metrics);
    if is_windows(ctx, vm) {
        let mut lines = Vec::new();
        if running {
            match gm {
                None => lines.push(t.vs_unknown.to_owned()),
                Some(gm) => {
                    lines.push(
                        if gm.pv_drivers_detected {
                            t.vs_io_optimized
                        } else {
                            t.vs_io_not_optimized
                        }
                        .to_owned(),
                    );
                    let agent = gm
                        .other
                        .get("feature-static-ip-setting")
                        .is_some_and(|v| v != "0");
                    lines.push(
                        if agent {
                            t.vs_agent_installed
                        } else {
                            t.vs_agent_not_installed
                        }
                        .to_owned(),
                    );
                }
            }
        }
        lines.push(
            if vm.has_vendor_device {
                t.vs_wu_yes
            } else {
                t.vs_wu_no
            }
            .to_owned(),
        );
        return Some(Row::new(t.virt_state, lines.join("\n")));
    }
    if !running {
        return None;
    }
    let Some(gm) = gm else {
        return Some(Row::new(t.virt_state, t.vs_unknown));
    };
    let major = gm.pv_drivers_version.get("major");
    let minor = gm.pv_drivers_version.get("minor");
    let (Some(major), Some(minor)) = (major, minor) else {
        let started = m
            .get(&vm.metrics)
            .and_then(|metrics| metrics.start_time.unix_seconds())
            .unwrap_or(0);
        if ctx.now - started < 120 {
            return Some(Row::new(t.virt_state, t.vs_unknown));
        }
        return Some(Row::warn(t.virt_state, t.vs_tools_missing));
    };
    let version = format!("{major}.{minor}");
    Some(if gm.pv_drivers_up_to_date {
        Row::new(t.virt_state, fill(t.vs_optimized, &[("version", &version)]))
    } else {
        Row::warn(t.virt_state, fill(t.vs_tools_old, &[("version", &version)]))
    })
}

fn vm_boot_rows(ctx: Ctx<'_>, vm: &VmRecord) -> Vec<Row> {
    let t = tab_strings();
    let mut rows = vec![Row::new(
        t.auto_boot,
        yes_no(vm.other_config.get("auto_poweron").map(String::as_str) == Some("true")),
    )];
    if objects::vm_is_hvm(vm) {
        let mut order: Vec<char> = vm
            .hvm_boot_params
            .get("order")
            .map(|o| o.to_uppercase())
            .unwrap_or_else(|| "CD".to_owned())
            .chars()
            .collect();
        for c in ['C', 'D', 'N'] {
            if !order.contains(&c) {
                order.push(c);
            }
        }
        let mut seen = Vec::new();
        let lines: Vec<&str> = order
            .into_iter()
            .filter(|c| {
                let new = !seen.contains(c);
                seen.push(*c);
                new
            })
            .map(|c| match c {
                'C' => t.boot_hdd,
                'D' => t.boot_dvd,
                _ => t.boot_network,
            })
            .collect();
        rows.push(Row::new(t.boot_order, lines.join("\n")));
        let uefi = vm
            .hvm_boot_params
            .get("firmware")
            .is_some_and(|f| f.eq_ignore_ascii_case("uefi"));
        if uefi {
            let secure = vm.platform.get("secureboot").map(String::as_str);
            let certificates = ctx
                .model
                .pool()
                .is_some_and(|(_, p)| !p.uefi_certificates.is_empty());
            rows.push(match secure {
                Some("true") if !certificates => Row::warn(t.boot_mode, t.boot_uefi_secure_missing),
                Some("true") => Row::new(t.boot_mode, t.boot_uefi_secure),
                Some("auto") if certificates => Row::new(t.boot_mode, t.boot_uefi_secure),
                _ => Row::new(t.boot_mode, t.boot_uefi),
            });
        } else {
            rows.push(Row::new(t.boot_mode, t.boot_bios));
        }
    } else {
        rows.push(Row::new(t.os_boot_params, vm.pv_args.clone()));
    }
    rows
}

fn vm_ha_rows(ctx: Ctx<'_>, vm: &VmRecord) -> Vec<Row> {
    let t = tab_strings();
    if !ctx.model.pool().is_some_and(|(_, p)| p.ha_enabled) || vm.is_a_template {
        return Vec::new();
    }
    let value = match vm.ha_restart_priority.as_str() {
        "best-effort" => t.ha_best_effort,
        "" => t.ha_do_not_restart,
        "0" => t.ha_restart_first,
        _ => t.ha_restart,
    };
    vec![Row::new(t.ha_priority, value)]
}

/// "2 sockets with 2 cores per socket".
pub fn topology(vm: &VmRecord) -> String {
    let t = tab_strings();
    let cores: i64 = vm
        .platform
        .get("cores-per-socket")
        .and_then(|c| c.parse().ok())
        .filter(|c| *c > 0)
        .unwrap_or(1);
    let sockets = if vm.vcpus_max % cores == 0 && vm.vcpus_max / cores <= 16 {
        vm.vcpus_max / cores
    } else {
        0
    };
    let (s, c) = (sockets.to_string(), cores.to_string());
    let args = [("sockets", s.as_str()), ("cores", c.as_str())];
    match (sockets, cores) {
        (0, 1) => t.topology_invalid_1c.to_owned(),
        (0, _) => fill(t.topology_invalid_nc, &args),
        (1, 1) => t.topology_1s_1c.to_owned(),
        (1, _) => fill(t.topology_1s_nc, &args),
        (_, 1) => fill(t.topology_ns_1c, &args),
        _ => fill(t.topology_ns_nc, &args),
    }
}

fn vm_cpu_rows(vm: &VmRecord) -> Vec<Row> {
    let t = tab_strings();
    let mut rows = vec![Row::new(t.vcpus, vm.vcpus_at_startup.to_string())];
    if vm.vcpus_max != vm.vcpus_at_startup {
        rows.push(Row::new(t.vcpus_max, vm.vcpus_max.to_string()));
    }
    rows.push(Row::new(t.topology, topology(vm)));
    rows
}

fn vm_device_security_rows(vm: &VmRecord) -> Vec<Row> {
    let t = tab_strings();
    match vm.vtpms.len() {
        0 => Vec::new(),
        1 => vec![Row::new(t.vtpm, t.vtpm_one)],
        n => vec![Row::new(
            t.vtpm,
            fill(t.vtpm_many, &[("count", &n.to_string())]),
        )],
    }
}

// ---- host ------------------------------------------------------------------

/// General tab of a host.
pub fn host_sections(ctx: Ctx<'_>, host_ref: &Ref<Host>, host: &HostRecord) -> Vec<Section> {
    let t = tab_strings();
    non_empty(vec![
        Section {
            title: t.sec_general,
            rows: host_general_rows(ctx, host_ref, host),
        },
        Section {
            title: t.sec_certificates,
            rows: host_certificate_rows(ctx, host),
        },
        Section {
            title: t.sec_version,
            rows: host_version_rows(host),
        },
        Section {
            title: t.sec_management,
            rows: management_rows(ctx, &[(host_ref, host)], false),
        },
        Section {
            title: t.sec_memory,
            rows: host_memory_rows(ctx, host),
        },
        Section {
            title: t.sec_cpus,
            rows: host_cpu_rows(ctx, host),
        },
    ])
}

fn autoboot_row(ctx: Ctx<'_>) -> Row {
    let on = ctx.model.pool().is_some_and(|(_, p)| {
        p.other_config.get("auto_poweron").map(String::as_str) == Some("true")
    });
    Row::new(tab_strings().autoboot, yes_no(on))
}

fn host_general_rows(ctx: Ctx<'_>, host_ref: &Ref<Host>, host: &HostRecord) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let mut rows = common_rows(
        host.name_label.clone(),
        Some(&host.name_description),
        &host.tags,
        &host.other_config,
    );
    if objects::pool_is_visible(m) {
        rows.push(Row::new(
            t.coordinator,
            yes_no(objects::is_coordinator(m, host_ref)),
        ));
    }
    rows.push(if !objects::host_is_live(m, host) {
        Row::warn(t.enabled, t.host_unreachable)
    } else if !host.enabled {
        if objects::host_maintenance_flag(host) {
            Row::warn(t.enabled, t.host_maintenance)
        } else {
            Row::warn(t.enabled, t.host_disabled)
        }
    } else {
        Row::new(t.enabled, t.yes)
    });
    rows.push(autoboot_row(ctx));
    let pool_tls = m.pool().is_some_and(|(_, p)| p.tls_verification_enabled);
    rows.push(if host.tls_verification_enabled {
        Row::new(t.cert_verification, t.cert_verification_enabled)
    } else if pool_tls {
        Row::warn(t.cert_verification, t.cert_verification_pool_only)
    } else {
        Row::warn(t.cert_verification, t.cert_verification_disabled)
    });
    let iqn = if host.iscsi_iqn.is_empty() {
        host.other_config
            .get("iscsi_iqn")
            .cloned()
            .unwrap_or_default()
    } else {
        host.iscsi_iqn.clone()
    };
    rows.push(Row::new(t.iscsi_iqn, iqn));
    rows.push(Row::new(
        t.log_destination,
        match host.logging.get("syslog_destination") {
            Some(d) if !d.is_empty() => fill(t.log_remote, &[("destination", d)]),
            _ => t.log_local.to_owned(),
        },
    ));
    let since = |key: &str| {
        unix_float(host.other_config.get(key))
            .map(|start| pretty_duration(ctx.now - start))
            .unwrap_or_default()
    };
    rows.push(Row::new(t.host_uptime, since("boot_time")));
    rows.push(Row::new(t.agent_uptime, since("agent_start_time")));
    if host.external_auth_type == "AD" {
        rows.push(Row::new(t.domain, host.external_auth_service_name.clone()));
    }
    rows.push(Row::new(t.uuid, host.uuid.clone()));
    rows
}

fn certificate_value(cert: &xapi::CertificateRecord) -> String {
    let t = tab_strings();
    let date = |d: &xapi::DateTime| {
        d.unix_seconds()
            .map(|s| datetime_utc(s, false))
            .unwrap_or_else(|| d.to_string())
    };
    let thumbprint = if cert.fingerprint_sha256.is_empty() {
        &cert.fingerprint
    } else {
        &cert.fingerprint_sha256
    };
    fill(
        t.cert_value,
        &[
            ("from", &date(&cert.not_before)),
            ("to", &date(&cert.not_after)),
            ("thumbprint", thumbprint),
        ],
    )
}

fn certificate_type_label(kind: &CertificateType) -> &'static str {
    let t = tab_strings();
    match kind {
        CertificateType::Ca => t.cert_ca,
        CertificateType::HostInternal => t.cert_host_internal,
        _ => t.cert_host,
    }
}

fn host_certificate_rows(ctx: Ctx<'_>, host: &HostRecord) -> Vec<Row> {
    let mut certs: Vec<&xapi::CertificateRecord> = host
        .certificates
        .iter()
        .filter_map(|c| ctx.model.get(c))
        .map(|c| &**c)
        .collect();
    certs.sort_by_key(|c| c.r#type.as_str().to_owned());
    certs
        .into_iter()
        .map(|c| Row::new(certificate_type_label(&c.r#type), certificate_value(c)))
        .collect()
}

fn host_version_rows(host: &HostRecord) -> Vec<Row> {
    let t = tab_strings();
    let sv = &host.software_version;
    if sv.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    if let Some(date) = sv.get("date") {
        rows.push(Row::new(t.build_date, build_date(date)));
    }
    rows.push(Row::new(t.version, host_version(host)));
    if let Some(dbv) = sv.get("dbv") {
        rows.push(Row::new(t.dbv, dbv.clone()));
    }
    rows
}

/// "Management Interfaces" of one host or, on a pool page, of every host
/// (labels then say which host).
fn management_rows(ctx: Ctx<'_>, hosts: &[(&Ref<Host>, &HostRecord)], on_pool: bool) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let mut rows = Vec::new();
    for (host_ref, host) in hosts {
        let label = |l: &str| {
            if on_pool {
                fill(t.on_host, &[("label", l), ("host", &host.name_label)])
            } else {
                l.to_owned()
            }
        };
        if !host.hostname.is_empty() {
            rows.push(Row::new(label(t.dns_hostname), host.hostname.clone()));
        }
        let mut pifs: Vec<&xapi::PifRecord> = m
            .pifs
            .values()
            .filter(|p| &p.host == *host_ref)
            .map(|p| &**p)
            .collect();
        pifs.sort_by(|a, b| {
            b.management
                .cmp(&a.management)
                .then_with(|| objects::natural_cmp(&a.device, &b.device))
        });
        for pif in pifs {
            let ip = objects::pif_friendly_ip(pif).unwrap_or_else(|| t.unknown.to_owned());
            if pif.management {
                rows.push(Row::new(label(t.management_interface), ip));
            } else if objects::pif_is_secondary_management(m, pif, ctx.show_hidden) {
                let purpose: String = pif
                    .other_config
                    .get("management_purpose")
                    .map(|p| p.chars().take(30).collect())
                    .unwrap_or_default();
                rows.push(Row::new(label(&purpose), ip));
            }
        }
    }
    rows
}

fn host_memory_rows(ctx: Ctx<'_>, host: &HostRecord) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let Some(mem) = objects::host_memory(m, host) else {
        return vec![
            Row::new(t.mem_server, t.not_found),
            Row::new(t.mem_vms, t.not_found),
            Row::new(t.mem_xen, t.not_found),
        ];
    };
    let mut vms: Vec<(&Ref<Vm>, &VmRecord)> = host
        .resident_vms
        .iter()
        .filter_map(|r| m.vms.get_key_value(r))
        .map(|(r, v)| (r, &**v))
        .filter(|(_, v)| !v.is_control_domain)
        .collect();
    vms.sort_by(|a, b| objects::vm_cmp(*a, *b));
    let lines: Vec<String> = vms
        .iter()
        .filter_map(|(_, vm)| {
            objects::vm_memory_actual(m, vm).map(|actual| {
                fill(
                    t.mem_vm_line,
                    &[
                        ("vm", &vm.name_label),
                        ("memory", &memory_size(actual, true)),
                    ],
                )
            })
        })
        .collect();
    vec![
        Row::new(
            t.mem_server,
            fill(
                t.mem_server_value,
                &[
                    ("available", &memory_size(mem.available, true)),
                    ("total", &memory_size(mem.total, true)),
                ],
            ),
        ),
        Row::new(t.mem_vms, lines.join("\n")),
        Row::new(t.mem_xen, memory_size(mem.xen, true)),
    ]
}

fn host_cpu_rows(ctx: Ctx<'_>, host: &HostRecord) -> Vec<Row> {
    let t = tab_strings();
    let mut cpus: Vec<&xapi::HostCpuRecord> = host
        .host_cpus
        .iter()
        .filter_map(|c| ctx.model.get(c))
        .map(|c| &**c)
        .collect();
    cpus.sort_by_key(|c| c.number);
    let value = |c: &xapi::HostCpuRecord| {
        fill(
            t.cpu_value,
            &[
                ("vendor", &c.vendor),
                ("model", &c.modelname),
                ("speed", &c.speed.to_string()),
            ],
        )
    };
    let Some(first) = cpus.first() else {
        return Vec::new();
    };
    if cpus.iter().all(|c| value(c) == value(first)) {
        let label = if cpus.len() == 1 {
            fill(t.cpu_one, &[("number", "0")])
        } else {
            fill(t.cpu_range, &[("last", &(cpus.len() - 1).to_string())])
        };
        return vec![Row::new(label, value(first))];
    }
    cpus.iter()
        .map(|c| {
            Row::new(
                fill(t.cpu_one, &[("number", &c.number.to_string())]),
                value(c),
            )
        })
        .collect()
}

// ---- pool ------------------------------------------------------------------

/// General tab of a pool.
pub fn pool_sections(ctx: Ctx<'_>, pool: &PoolRecord) -> Vec<Section> {
    let t = tab_strings();
    let m = ctx.model;
    let mut general = common_rows(
        pool_display_name(m, pool),
        Some(&pool.name_description),
        &pool.tags,
        &pool.other_config,
    );
    let sockets: i64 = m
        .hosts
        .values()
        .map(|h| {
            h.cpu_info
                .get("socket_count")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
        })
        .sum();
    general.push(Row::new(t.sockets, sockets.to_string()));
    general.push(autoboot_row(ctx));
    let hosts = objects::sorted_hosts(m);
    if pool.tls_verification_enabled {
        let disabled: Vec<&str> = hosts
            .iter()
            .filter(|(_, h)| !h.tls_verification_enabled)
            .map(|(_, h)| h.name_label.as_str())
            .collect();
        general.push(if disabled.is_empty() {
            Row::new(t.cert_verification, t.cert_verification_enabled)
        } else {
            Row::warn(
                t.cert_verification,
                format!(
                    "{}\n{}",
                    t.cert_verification_some_hosts,
                    disabled.join("\n")
                ),
            )
        });
    } else {
        general.push(Row::warn(t.cert_verification, t.cert_verification_disabled));
    }
    if let Some(coordinator) = m.coordinator() {
        general.push(Row::new(t.version, host_version(coordinator)));
    }
    general.push(Row::new(t.uuid, pool.uuid.clone()));

    let mut cas: Vec<&xapi::CertificateRecord> = m
        .certificates
        .values()
        .filter(|c| c.r#type == CertificateType::Ca)
        .map(|c| &**c)
        .collect();
    cas.sort_by(|a, b| objects::natural_cmp(&a.name, &b.name));
    let certificates = cas
        .into_iter()
        .map(|c| {
            Row::new(
                c.name.clone(),
                format!(
                    "{}\n{}",
                    certificate_type_label(&c.r#type),
                    certificate_value(c)
                ),
            )
        })
        .collect();
    let host_list: Vec<(&Ref<Host>, &HostRecord)> =
        hosts.iter().map(|(r, h)| (*r, &***h)).collect();
    non_empty(vec![
        Section {
            title: t.sec_general,
            rows: general,
        },
        Section {
            title: t.sec_certificates,
            rows: certificates,
        },
        Section {
            title: t.sec_management,
            rows: management_rows(ctx, &host_list, true),
        },
    ])
}

// ---- SR --------------------------------------------------------------------

/// `SR.FriendlyTypeName()`.
pub fn sr_type_name(ctx: Ctx<'_>, sr: &SrRecord) -> String {
    let name = match sr.r#type.as_str() {
        "ext" => "Ext3",
        "ext4" => "Ext4",
        "file" => "File",
        "xfs" => "XFS",
        "lvm" => "LVM",
        "lvmoiscsi" => "LVM over iSCSI",
        "lvmohba" | "lvmofc" => "Hardware HBA",
        "lvmofcoe" => "FCoE",
        "iscsi" => "VDI-per-LUN iSCSI",
        "nfs" => "NFS",
        "smb" => "SMB",
        "iso" => "ISO",
        "udev" => "udev",
        "gfs2" => "GFS2",
        "rawhba" => "LUN-per-VDI",
        "local" => "Local",
        "shm" => "Local Performance Monitoring",
        "cslg" => "StorageLink",
        "netapp" => "NetApp",
        "equal" => "Dell EqualLogic",
        "egenera" => "Egenera Virtual Storage",
        "egeneracd" => "Egenera Virtual DVD Drive",
        "ebs" => "Elastic Block Storage",
        "nutanix" => "Nutanix",
        "nutanixiso" => "Nutanix ISO",
        "dummy" | "tmpfs" => tab_strings().unknown,
        other => {
            // A storage driver of its own (XCP-ng's zfs, linstor, …): its
            // SM name, else the raw type.
            return ctx
                .model
                .sms
                .values()
                .find(|sm| sm.r#type == other)
                .map(|sm| sm.name_label.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| other.to_owned());
        }
    };
    name.to_owned()
}

/// "{used} used of {total} total ({allocated} allocated)".
pub fn sr_size_text(sr: &SrRecord) -> String {
    fill(
        tab_strings().sr_size_value,
        &[
            ("used", &disk_size(sr.physical_utilisation)),
            ("total", &disk_size(sr.physical_size)),
            ("allocated", &disk_size(sr.virtual_allocation)),
        ],
    )
}

/// General tab of an SR.
pub fn sr_sections(ctx: Ctx<'_>, sr: &SrRecord) -> Vec<Section> {
    let t = tab_strings();
    let m = ctx.model;
    let mut general = common_rows(
        sr.name_label.clone(),
        Some(&sr.name_description),
        &sr.tags,
        &sr.other_config,
    );
    general.push(Row::new(t.sr_type, sr_type_name(ctx, sr)));
    if sr.content_type != "iso" && sr.r#type != "udev" {
        general.push(Row::new(t.sr_size, sr_size_text(sr)));
    }
    let scsi = sr
        .pbds
        .iter()
        .filter_map(|p| m.get(p))
        .find_map(|p| p.device_config.get("SCSIid").cloned())
        .or_else(|| {
            sr.sm_config.get("devserial").map(|s| {
                s.strip_prefix("scsi-")
                    .unwrap_or(s)
                    .trim_end_matches(',')
                    .to_owned()
            })
        });
    if let Some(scsi) = scsi {
        general.push(Row::new(t.scsi_id, scsi));
    }
    general.push(Row::new(t.uuid, sr.uuid.clone()));
    non_empty(vec![
        Section {
            title: t.sec_general,
            rows: general,
        },
        Section {
            title: t.sec_status,
            rows: sr_status_rows(ctx, sr),
        },
        Section {
            title: t.sec_multipathing,
            rows: sr_multipath_rows(ctx, sr),
        },
    ])
}

fn sr_status_rows(ctx: Ctx<'_>, sr: &SrRecord) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let state = if objects::sr_is_detached(m, sr) {
        Row::warn(t.sr_state, t.sr_detached)
    } else if objects::sr_is_broken(m, sr) {
        Row::warn(t.sr_state, t.sr_broken)
    } else if !objects::sr_multipath_ok(m, sr) {
        Row::warn(t.sr_state, t.sr_multipath_failure)
    } else {
        Row::new(t.sr_state, t.sr_ok)
    };
    let mut rows = vec![state];
    for (host_ref, host) in objects::sorted_hosts(m) {
        let label: String = host.name_label.chars().take(30).collect();
        let pbd = sr
            .pbds
            .iter()
            .filter_map(|p| m.get(p))
            .find(|p| &p.host == host_ref);
        match pbd {
            None if sr.shared => rows.push(Row::warn(label, t.pbd_missing)),
            None => {}
            Some(pbd) if !pbd.currently_attached => rows.push(Row::warn(label, t.pbd_unplugged)),
            Some(_) if !objects::host_is_live(m, host) => {
                rows.push(Row::warn(label, t.host_unreachable));
            }
            Some(_) => rows.push(Row::new(label, t.pbd_connected)),
        }
    }
    rows
}

fn sr_multipath_rows(ctx: Ctx<'_>, sr: &SrRecord) -> Vec<Row> {
    let t = tab_strings();
    let m = ctx.model;
    let capable = sr.sm_config.get("multipathable").map(String::as_str) == Some("true")
        || m.sms.values().any(|sm| {
            sm.r#type == sr.r#type
                && (sm.capabilities.iter().any(|c| c == "MULTIPATH")
                    || sm.features.contains_key("MULTIPATH"))
        });
    if !capable {
        return vec![Row::new(t.multipath_capable, t.no)];
    }
    let mut rows = Vec::new();
    for (host_ref, host) in objects::sorted_hosts(m) {
        let Some(pbd) = sr
            .pbds
            .iter()
            .filter_map(|p| m.get(p))
            .find(|p| &p.host == host_ref)
        else {
            continue;
        };
        let label: String = host.name_label.chars().take(30).collect();
        let paths = pbd
            .other_config
            .iter()
            .filter(|(k, _)| k.starts_with("mpath-"))
            .find_map(|(_, v)| objects::mpath_paths(v));
        let row = match paths {
            Some((current, max)) => {
                let mut text = fill(
                    t.paths_active,
                    &[("current", &current.to_string()), ("max", &max.to_string())],
                );
                let sessions: Option<i64> = pbd
                    .other_config
                    .get("iscsi_sessions")
                    .and_then(|s| s.parse().ok());
                if let Some(n) = sessions {
                    text.push_str(&fill(t.iscsi_sessions, &[("count", &n.to_string())]));
                }
                if current < max || sessions.is_some_and(|n| max < n) {
                    Row::warn(label, text)
                } else {
                    Row::new(label, text)
                }
            }
            None => {
                let active =
                    pbd.other_config.get("multipathed").map(String::as_str) == Some("true");
                Row::new(
                    label,
                    if active {
                        t.multipath_active
                    } else {
                        t.multipath_not_active
                    },
                )
            }
        };
        rows.push(row);
    }
    rows
}

/// The section titled `title`, if present.
#[cfg(test)]
pub fn section<'a>(sections: &'a [Section], title: &str) -> Option<&'a Section> {
    sections.iter().find(|s| s.title == title)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    fn vm_general(m: &xen_core::ConnectionModel, r: &str) -> Vec<Section> {
        vm_sections(ctx(m), &m.vms[&Ref::new(r)])
    }

    #[test]
    fn vm_general_tab() {
        let m = fixture();
        let t = tab_strings();
        let s = vm_general(&m, "OpaqueRef:vm-web");
        let titles: Vec<&str> = s.iter().map(|s| s.title).collect();
        assert_eq!(titles, [t.sec_general, t.sec_boot, t.sec_cpus]);
        let g = &s[0];
        assert_eq!(g.value(t.name), Some("web-01"));
        assert_eq!(g.value(t.description), Some("Debian 12 web server"));
        assert_eq!(g.value(t.tags), Some("production, web"));
        assert_eq!(g.value(t.folder), Some("Production › Web"));
        assert_eq!(g.value(t.os), Some("Debian GNU/Linux 12 (bookworm)"));
        assert_eq!(g.value(t.virt_mode), Some(t.virt_hvm));
        assert_eq!(g.value(t.bios_copied), Some("No"));
        assert_eq!(
            g.value(t.virt_state),
            Some("Optimized (version 8.3 installed)")
        );
        // Started 2026-11-20T08:00Z, now 2026-11-25T12:00Z.
        assert_eq!(g.value(t.vm_uptime), Some("5 days 4 hours 0 minutes"));
        // A local disk: no home server row.
        assert_eq!(g.value(t.home_server), None);
        assert_eq!(
            g.value(t.uuid),
            Some("0b0b0b0b-0000-4000-8000-000000000001")
        );
        let boot = &s[1];
        assert_eq!(boot.value(t.auto_boot), Some("Yes"));
        assert_eq!(
            boot.value(t.boot_order),
            Some("Hard Disk\nDVD-Drive\nNetwork")
        );
        assert_eq!(boot.value(t.boot_mode), Some(t.boot_bios));
        let cpus = &s[2];
        assert_eq!(cpus.value(t.vcpus), Some("2"));
        assert_eq!(cpus.value(t.vcpus_max), Some("4"));
        assert_eq!(
            cpus.value(t.topology),
            Some("2 sockets with 2 cores per socket")
        );

        // Halted on shared storage: home server; UEFI without certificates;
        // a vTPM.
        let s = vm_general(&m, "OpaqueRef:vm-db");
        let g = &s[0];
        assert_eq!(g.value(t.home_server), Some("xcp-host-02"));
        assert_eq!(g.value(t.virt_state), None);
        assert_eq!(g.value(t.vm_uptime), None);
        assert_eq!(g.value(t.os), Some(t.unknown));
        assert_eq!(g.value(t.tags), Some(t.none));
        let boot = section(&s, t.sec_boot).unwrap();
        assert_eq!(
            boot.value(t.boot_order),
            Some("DVD-Drive\nHard Disk\nNetwork")
        );
        let mode = boot.rows.iter().find(|r| r.label == t.boot_mode).unwrap();
        assert!(mode.warn);
        assert_eq!(mode.value, t.boot_uefi_secure_missing);
        assert_eq!(
            section(&s, t.sec_device_security).unwrap().value(t.vtpm),
            Some(t.vtpm_one)
        );
        // PV: boot parameters.
        let s = vm_general(&m, "OpaqueRef:vm-build");
        let boot = section(&s, t.sec_boot).unwrap();
        assert_eq!(boot.value(t.os_boot_params), Some("console=hvc0"));
        assert_eq!(s[0].value(t.virt_mode), Some(t.virt_pv));
        // No disks: home server row with <None>.
        assert_eq!(s[0].value(t.home_server), Some(t.none));

        // Snapshot.
        let s = vm_general(&m, "OpaqueRef:vm-web-snap");
        assert_eq!(s[0].value(t.snapshot_of), Some("web-01"));
        assert_eq!(
            s[0].value(t.creation_time),
            Some("Nov 1, 2026 10:30:00 AM UTC")
        );
        assert_eq!(s[0].value(t.virt_state), None);
    }

    #[test]
    fn vm_virtualization_states() {
        let m = fixture();
        let t = tab_strings();
        let r = Ref::new("OpaqueRef:vm-web");
        let mut vm = (*m.vms[&r]).clone();
        let row = |m: &xen_core::ConnectionModel, vm: &VmRecord| virtualization_state(ctx(m), vm);
        // No guest metrics while running.
        let mut no_gm = vm.clone();
        no_gm.guest_metrics = Ref::null();
        assert_eq!(row(&m, &no_gm).unwrap().value, t.vs_unknown);
        // Windows: multi-line.
        vm.platform.insert("viridian".into(), "true".into());
        let mut windows_model = m.clone();
        let gm_ref = vm.guest_metrics.clone();
        let mut gm = (*windows_model.vm_guest_metrics[&gm_ref]).clone();
        gm.os_version.insert("distro".into(), "windows".into());
        windows_model
            .vm_guest_metrics
            .insert(gm_ref, std::sync::Arc::new(gm));
        assert_eq!(
            row(&windows_model, &vm).unwrap().value,
            format!(
                "{}\n{}\n{}",
                t.vs_io_optimized, t.vs_agent_not_installed, t.vs_wu_no
            )
        );
    }

    #[test]
    fn host_general_tab() {
        let m = fixture();
        let t = tab_strings();
        let r = Ref::new("OpaqueRef:host-0001");
        let s = host_sections(ctx(&m), &r, &m.hosts[&r]);
        let titles: Vec<&str> = s.iter().map(|s| s.title).collect();
        assert_eq!(
            titles,
            [
                t.sec_general,
                t.sec_certificates,
                t.sec_version,
                t.sec_management,
                t.sec_memory,
                t.sec_cpus
            ]
        );
        let g = &s[0];
        assert_eq!(g.value(t.coordinator), Some("Yes"));
        assert_eq!(g.value(t.enabled), Some("Yes"));
        assert_eq!(g.value(t.autoboot), Some("Yes"));
        assert_eq!(
            g.value(t.cert_verification),
            Some(t.cert_verification_enabled)
        );
        assert_eq!(
            g.value(t.iscsi_iqn),
            Some("iqn.2026-01.org.example:host-01")
        );
        assert_eq!(g.value(t.log_destination), Some(t.log_local));
        // boot_time 1795000000 = 2026-11-18T07:06:40Z.
        assert_eq!(g.value(t.host_uptime), Some("7 days 0 hours 53 minutes"));
        assert_eq!(g.value(t.agent_uptime), Some("6 days 23 hours 53 minutes"));
        assert_eq!(
            s[1].value(t.cert_host),
            Some(
                "Valid from Jan 1, 2026 12:00 AM UTC to Jan 1, 2036 12:00 AM UTC\nThumbprint: AA:BB:CC:01"
            )
        );
        assert_eq!(s[2].value(t.build_date), Some("Jan 15, 2026"));
        assert_eq!(s[2].value(t.version), Some("XCP-ng 8.3"));
        assert_eq!(s[2].value(t.dbv), Some("2025.0819"));
        assert_eq!(s[3].value(t.dns_hostname), Some("xcp-host-01"));
        assert_eq!(s[3].value(t.management_interface), Some("192.0.2.11"));
        assert_eq!(s[3].value("Storage"), Some("198.51.100.11"));
        assert_eq!(
            s[4].value(t.mem_server),
            Some("28.9 GB RAM available (32.0 GB total)")
        );
        assert_eq!(s[4].value(t.mem_vms), Some("web-01: using 2.0 GB"));
        assert_eq!(s[4].value(t.mem_xen), Some("2.1 GB"));
        assert_eq!(
            s[5].value("CPU 0 - 1"),
            Some(
                "Vendor: GenuineIntel\nModel: Intel(R) Xeon(R) CPU E5-2690 v3 @ 2.60GHz\nSpeed: 2596 MHz"
            )
        );

        // In maintenance mode; DHCP management interface.
        let r = Ref::new("OpaqueRef:host-0002");
        let s = host_sections(ctx(&m), &r, &m.hosts[&r]);
        let enabled = s[0].rows.iter().find(|r| r.label == t.enabled).unwrap();
        assert_eq!(enabled.value, t.host_maintenance);
        assert!(enabled.warn);
        assert_eq!(s[0].value(t.coordinator), Some("No"));
        assert_eq!(
            section(&s, t.sec_management)
                .unwrap()
                .value(t.management_interface),
            Some("DHCP")
        );
        assert_eq!(
            section(&s, t.sec_cpus).unwrap().value("CPU 0").map(|_| ()),
            Some(())
        );
    }

    #[test]
    fn pool_general_tab() {
        let m = fixture();
        let t = tab_strings();
        let (_, pool) = m.pool().unwrap();
        let s = pool_sections(ctx(&m), pool);
        let g = &s[0];
        assert_eq!(g.value(t.name), Some("Lab Pool"));
        assert_eq!(g.value(t.sockets), Some("2"));
        assert_eq!(g.value(t.version), Some("XCP-ng 8.3"));
        assert_eq!(
            g.value(t.cert_verification),
            Some(t.cert_verification_enabled)
        );
        let mgmt = section(&s, t.sec_management).unwrap();
        assert_eq!(
            mgmt.value("Management interface on 'xcp-host-02'"),
            Some("DHCP")
        );
        assert_eq!(mgmt.rows[0].label, "DNS hostname on 'xcp-host-01'");
    }

    #[test]
    fn sr_general_tab() {
        let m = fixture();
        let t = tab_strings();
        let general = |r: &str| sr_sections(ctx(&m), &m.srs[&Ref::new(r)]);
        let s = general("OpaqueRef:sr-nfs");
        assert_eq!(s[0].value(t.sr_type), Some("NFS"));
        assert_eq!(
            s[0].value(t.sr_size),
            Some("600 GB used of 2 TB total (158 GB allocated)")
        );
        let status = section(&s, t.sec_status).unwrap();
        assert_eq!(status.value(t.sr_state), Some(t.sr_ok));
        assert_eq!(status.value("xcp-host-02"), Some(t.pbd_connected));
        assert_eq!(
            section(&s, t.sec_multipathing)
                .unwrap()
                .value(t.multipath_capable),
            Some("No")
        );
        // Broken: unplugged on one host; ISO has no size row.
        let s = general("OpaqueRef:sr-iso");
        assert_eq!(s[0].value(t.sr_size), None);
        let status = section(&s, t.sec_status).unwrap();
        assert_eq!(status.value(t.sr_state), Some(t.sr_broken));
        assert_eq!(status.value("xcp-host-02"), Some(t.pbd_unplugged));
        // Local storage: only its host is listed.
        let s = general("OpaqueRef:sr-local-02");
        let status = section(&s, t.sec_status).unwrap();
        assert_eq!(status.rows.len(), 2);
        assert_eq!(s[0].value(t.sr_type), Some("Ext3"));
    }

    #[test]
    fn real_capture_general_tabs_build() {
        let m = capture();
        let c = ctx(&m);
        for vm in m.vms.values() {
            assert!(!vm_sections(c, vm).is_empty());
        }
        for (r, h) in &m.hosts {
            let s = host_sections(c, r, h);
            assert!(section(&s, tab_strings().sec_memory).is_some());
        }
        for sr in m.srs.values() {
            assert!(!sr_sections(c, sr).is_empty());
        }
        let (_, pool) = m.pool().unwrap();
        assert!(!pool_sections(c, pool).is_empty());
    }

    #[test]
    fn build_dates_parse() {
        assert_eq!(build_date("2026-01-15"), "Jan 15, 2026");
        assert_eq!(
            build_date("20250916T20:53:47Z"),
            "Sep 16, 2025 8:53:47 PM UTC"
        );
        assert_eq!(build_date("weird"), "weird");
    }
}

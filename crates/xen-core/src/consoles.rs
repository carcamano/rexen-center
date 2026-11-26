//! F14a: the external consoles — "Launch SSH Console" and Remote Desktop
//! through the platform's own client (plan decision: no embedded RDP).
//!
//! **SSH** (`VNCTabView`'s SSH console): offered for a VM that is not
//! Windows, and for a live server's console; shown unless the VM is
//! halted, usable while it runs and has an address. The address is the
//! first IPv4 one (else the first) of the VM's attached interfaces in
//! device order (installer networks skipped), as the guest agent reports
//! them (`VM.IPAddressForSSH`); a server's console uses its management
//! interfaces. The user is `root` (the original used the logged-in AD
//! subject's name when there was one).
//!
//! The program comes from Options > External Tools ([`SshConsole`]): the
//! platform's terminal running OpenSSH (this console's default, new),
//! PuTTY (`putty <address>`) or an OpenSSH binary (`ssh user@<address>`),
//! as the original's two choices. [`ssh_command`] builds the command
//! line; [`CommandLine::launch`] starts it without waiting.
//!
//! **Remote Desktop**: the guest agent's `feature-ts`, `feature-ts2` and
//! `data-ts` say whether Remote Desktop is usable or can be turned on
//! (`VM.CanUseRDP`, `CanEnableRDP`); turning it on asks the guest agent
//! (`guest-agent-operation` / `request-rdp-on`). The address is the first
//! candidate ([`rdp_candidates`], IPv4 first, interfaces whose NIC has a
//! link first, as `XSVNCScreen.PollPort`) that accepts a connection on
//! port 3389 ([`probe`]). The client is the platform's ([`rdp_command`]):
//! `mstsc /v:` on Windows, a `.rdp` file opened with `open` on macOS,
//! `xfreerdp /v:` elsewhere, or a program set in Options.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use xapi::{Host, Ref, VmPowerState, VmRecord};

use crate::model::ConnectionModel;
use crate::objects;
use crate::settings::{ExternalTools, SshConsole};

/// The Remote Desktop port (`XSVNCScreen.RDPPort`).
pub const RDP_PORT: u16 = 3389;

// ---- which guests ---------------------------------------------------------------------------

/// Distributions whose guest agent marks a VM as not Windows
/// (`VM.ReservedDistros`).
const LINUX_DISTROS: &[&str] = &[
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
    "neokylin",
    "gooroom",
    "rocky",
];

/// The guest metrics reference written in `last_booted_record`.
fn last_booted_guest_metrics(record: &str) -> Option<Ref<xapi::VmGuestMetrics>> {
    let at = record.find("'guest_metrics'")?;
    let rest = &record[at + "'guest_metrics'".len()..];
    let start = rest.find('\'')? + 1;
    let end = start + rest[start..].find('\'')?;
    let r = &rest[start..end];
    r.starts_with("OpaqueRef:").then(|| Ref::new(r))
}

/// `VM.IsWindows`: a guest agent reporting a Linux distribution (now or
/// at the last boot) says no; otherwise an HVM VM with `viridian`.
pub fn vm_is_windows(model: &ConnectionModel, vm: &VmRecord) -> bool {
    let not_windows = |gm: &xapi::VmGuestMetricsRecord| {
        gm.os_version
            .get("distro")
            .is_some_and(|d| LINUX_DISTROS.contains(&d.to_lowercase().as_str()))
            || gm
                .os_version
                .get("uname")
                .is_some_and(|u| u.to_lowercase().contains("netscaler"))
    };
    if model.get(&vm.guest_metrics).is_some_and(|g| not_windows(g)) {
        return false;
    }
    if let Some(r) = last_booted_guest_metrics(&vm.last_booted_record)
        && model.get(&r).is_some_and(|g| not_windows(g))
    {
        return false;
    }
    objects::vm_is_hvm(vm) && vm.platform.get("viridian").map(String::as_str) == Some("true")
}

/// `Helpers.FindIpAddresses`: the addresses the guest agent reports for
/// interface `device`, keys in order, each value split on new lines.
pub fn find_ip_addresses(
    networks: &std::collections::HashMap<String, String>,
    device: &str,
) -> Vec<String> {
    let prefix = format!("{device}/ip");
    let mut keys: Vec<&String> = networks.keys().filter(|k| k.starts_with(&prefix)).collect();
    keys.sort();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        for ip in networks[k].split('\n').flat_map(|s| s.split("%n")) {
            let ip = ip.trim();
            if !ip.is_empty() && !out.iter().any(|x| x == ip) {
                out.push(ip.to_owned());
            }
        }
    }
    out
}

fn first_ipv4_else_first(ips: Vec<String>) -> Option<String> {
    ips.iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .or(ips.first())
        .cloned()
}

/// `VM.IPAddressForSSH` of a guest.
pub fn vm_ssh_address(model: &ConnectionModel, vm: &VmRecord) -> Option<String> {
    let gm = model.get(&vm.guest_metrics)?;
    if vm.is_a_template {
        return None;
    }
    let mut vifs: Vec<&xapi::VifRecord> = vm
        .vifs
        .iter()
        .filter_map(|r| model.get(r).map(|v| &**v))
        .collect();
    vifs.sort_by(|a, b| a.device.cmp(&b.device));
    let mut ips = Vec::new();
    for vif in vifs {
        if !vif.currently_attached {
            continue;
        }
        let installer = model.get(&vif.network).is_some_and(|n| {
            n.other_config
                .get("is_guest_installer_network")
                .map(String::as_str)
                == Some("true")
        });
        if installer {
            continue;
        }
        ips.extend(find_ip_addresses(&gm.networks, &vif.device));
    }
    first_ipv4_else_first(ips)
}

/// The management addresses of a server (its console's SSH address):
/// attached interfaces of the server, management first.
pub fn host_ssh_address(model: &ConnectionModel, host: &Ref<Host>) -> Option<String> {
    let mut pifs: Vec<&xapi::PifRecord> = model
        .pifs
        .values()
        .filter(|p| &p.host == host && p.currently_attached)
        .map(|p| &**p)
        .collect();
    pifs.sort_by(|a, b| {
        b.management
            .cmp(&a.management)
            .then_with(|| objects::natural_cmp(&a.device, &b.device))
    });
    let ips = pifs
        .iter()
        .filter(|p| {
            p.management
                || !matches!(
                    p.ip_configuration_mode,
                    xapi::IpConfigurationMode::None | xapi::IpConfigurationMode::Unknown(_)
                )
        })
        .map(|p| p.ip.clone())
        .filter(|ip| !ip.is_empty())
        .collect();
    first_ipv4_else_first(ips)
}

/// What a console offers for SSH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshState {
    /// The button shows.
    pub visible: bool,
    /// It can be pressed: the address to connect to.
    pub address: Option<String>,
}

/// SSH for a VM's console (`UpdateOpenSSHConsoleButtonState`).
pub fn vm_ssh(model: &ConnectionModel, vm: &VmRecord) -> SshState {
    let supported = !vm_is_windows(model, vm);
    SshState {
        visible: supported && vm.power_state != VmPowerState::Halted,
        address: (supported && vm.power_state == VmPowerState::Running)
            .then(|| vm_ssh_address(model, vm))
            .flatten(),
    }
}

/// SSH for a server's console: the server must be live.
pub fn host_ssh(model: &ConnectionModel, host: &Ref<Host>) -> SshState {
    let live = model
        .get(host)
        .is_some_and(|h| objects::host_is_live(model, h));
    SshState {
        visible: live,
        address: live.then(|| host_ssh_address(model, host)).flatten(),
    }
}

// ---- Remote Desktop -------------------------------------------------------------------------

fn gm_flag(model: &ConnectionModel, vm: &VmRecord, key: &str) -> bool {
    model
        .get(&vm.guest_metrics)
        .and_then(|g| g.other.get(key))
        .and_then(|v| v.trim().parse::<i64>().ok())
        .is_some_and(|v| v != 0)
}

/// What the guest agent says about Remote Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RdpState {
    /// `HasRDP`: `feature-ts`.
    pub has_rdp: bool,
    /// `RDPEnabled`: `data-ts`.
    pub enabled: bool,
    /// `RDPControlEnabled`: `feature-ts2`.
    pub control: bool,
    /// `CanUseRDP`: all three and some network.
    pub usable: bool,
}

impl RdpState {
    /// `CanEnableRDP`: the agent can turn it on and it is off.
    pub fn can_enable(&self) -> bool {
        self.control && !self.enabled
    }
}

/// Remote Desktop of a VM.
pub fn rdp_state(model: &ConnectionModel, vm: &VmRecord) -> RdpState {
    let has_rdp = gm_flag(model, vm, "feature-ts");
    let enabled = gm_flag(model, vm, "data-ts");
    let control = gm_flag(model, vm, "feature-ts2");
    let networks = model
        .get(&vm.guest_metrics)
        .is_some_and(|g| !g.networks.is_empty());
    RdpState {
        has_rdp,
        enabled,
        control,
        usable: has_rdp && enabled && control && networks,
    }
}

/// The addresses to try for Remote Desktop (`XSVNCScreen.PollPort`):
/// IPv4 of interfaces whose NIC has a link, then their IPv6 (in
/// brackets), then those of interfaces without a NIC on the server.
/// Interfaces in device order.
pub fn rdp_candidates(model: &ConnectionModel, vm: &VmRecord) -> Vec<String> {
    let Some(gm) = model.get(&vm.guest_metrics) else {
        return Vec::new();
    };
    let (mut v4, mut v6, mut v4_nopif, mut v6_nopif) = (vec![], vec![], vec![], vec![]);
    let mut vifs: Vec<&xapi::VifRecord> = vm
        .vifs
        .iter()
        .filter_map(|r| model.get(r).map(|v| &**v))
        .collect();
    vifs.sort_by(|a, b| a.device.cmp(&b.device));
    for vif in vifs {
        let host = model.get(&vm.resident_on).map(|_| &vm.resident_on);
        let pif = model
            .pifs
            .values()
            .find(|p| p.network == vif.network && host.is_none_or(|h| &p.host == h));
        let link = pif.map(|p| model.get(&p.metrics).is_some_and(|m| m.carrier));
        let prefix = format!("{}/ip", vif.device);
        let mut keys: Vec<&String> = gm
            .networks
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .collect();
        keys.sort();
        for k in keys {
            let value = gm.networks[k].trim().to_owned();
            if value.is_empty() {
                continue;
            }
            if k.ends_with("ip") || k.contains("ipv4") {
                match link {
                    None => v4_nopif.push(value),
                    Some(true) => v4.push(value),
                    Some(false) => {}
                }
            } else if k.contains("ipv6") {
                let value = format!("[{value}]");
                match link {
                    None => v6_nopif.push(value),
                    Some(true) => v6.push(value),
                    Some(false) => {}
                }
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for ip in v4.into_iter().chain(v6).chain(v4_nopif).chain(v6_nopif) {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// The first of `candidates` that accepts a TCP connection on `port`
/// within `timeout` each, from this machine.
pub async fn probe(candidates: &[String], port: u16, timeout: Duration) -> Option<String> {
    for c in candidates {
        let Ok(ip) = c.trim_matches(['[', ']']).parse::<IpAddr>() else {
            continue;
        };
        let connect = tokio::net::TcpStream::connect((ip, port));
        if let Ok(Ok(_)) = tokio::time::timeout(timeout, connect).await {
            return Some(c.clone());
        }
    }
    None
}

// ---- command lines ----------------------------------------------------------------------------

/// The operating system the command line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    /// This build's platform.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// A program to start, with a file to write first (the `.rdp` file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub program: String,
    pub args: Vec<String>,
    pub file: Option<(PathBuf, String)>,
}

impl fmt::Display for CommandLine {
    /// The command as a shell would take it (for logs and the dry run).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quote = |s: &str| {
            if !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_[]".contains(c))
            {
                s.to_owned()
            } else {
                format!("'{}'", s.replace('\'', "'\\''"))
            }
        };
        write!(f, "{}", quote(&self.program))?;
        for a in &self.args {
            write!(f, " {}", quote(a))?;
        }
        Ok(())
    }
}

impl CommandLine {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            file: None,
        }
    }

    /// Writes the file, if any, and starts the program without waiting
    /// for it.
    pub fn launch(&self) -> std::io::Result<()> {
        if let Some((path, content)) = &self.file {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, content)?;
        }
        let mut child = std::process::Command::new(&self.program)
            .args(&self.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        // Reap it when it ends, without holding the caller.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Why the SSH console cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    /// "An external SSH console has not been configured."
    NotConfigured,
    /// "The file associated with your external SSH console doesn't exist."
    Missing(String),
}

/// `user@address`, bracketing an IPv6 address for a URL.
fn ssh_target(user: &str, address: &str, url: bool) -> String {
    let bare = address.trim_matches(['[', ']']);
    if url && bare.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("{user}@[{bare}]")
    } else {
        format!("{user}@{bare}")
    }
}

/// The SSH console's command line for `user` at `address`
/// (`buttonSSH_Click`): PuTTY gets the address alone, OpenSSH
/// `user@address`; the system choice opens the platform's terminal with
/// `ssh`. `exists` checks a configured program's path.
pub fn ssh_command(
    tools: &ExternalTools,
    platform: Platform,
    user: &str,
    address: &str,
    exists: impl Fn(&Path) -> bool,
) -> Result<CommandLine, SshError> {
    let configured = |path: &str| -> Result<String, SshError> {
        let path = path.trim();
        if path.is_empty() {
            return Err(SshError::NotConfigured);
        }
        if !exists(Path::new(path)) {
            return Err(SshError::Missing(path.to_owned()));
        }
        Ok(path.to_owned())
    };
    let bare = address.trim_matches(['[', ']']);
    Ok(match tools.ssh_console {
        SshConsole::Putty => CommandLine::new(&configured(&tools.putty_location)?, &[bare]),
        SshConsole::OpenSsh => CommandLine::new(
            &configured(&tools.openssh_location)?,
            &[&ssh_target(user, address, false)],
        ),
        SshConsole::System => match platform {
            Platform::MacOs => CommandLine::new(
                "open",
                &[&format!("ssh://{}", ssh_target(user, address, true))],
            ),
            Platform::Windows => CommandLine::new(
                "cmd.exe",
                &[
                    "/c",
                    "start",
                    "",
                    "ssh.exe",
                    &ssh_target(user, address, false),
                ],
            ),
            Platform::Linux => CommandLine::new(
                "x-terminal-emulator",
                &["-e", "ssh", &ssh_target(user, address, false)],
            ),
        },
    })
}

/// The Remote Desktop client's command line for `address` (`name`: the
/// VM's UUID, for the `.rdp` file's name under `temp`).
pub fn rdp_command(
    tools: &ExternalTools,
    platform: Platform,
    address: &str,
    name: &str,
    temp: &Path,
) -> CommandLine {
    let target = format!("/v:{address}");
    let custom = tools.rdp_client.trim();
    if !custom.is_empty() {
        return CommandLine::new(custom, &[&target]);
    }
    match platform {
        Platform::Windows => CommandLine::new("mstsc.exe", &[&target]),
        Platform::Linux => CommandLine::new("xfreerdp", &[&target]),
        Platform::MacOs => {
            let path = temp.join(format!("xenadmin-{name}.rdp"));
            let content =
                format!("full address:s:{address}:{RDP_PORT}\r\nprompt for credentials:i:1\r\n");
            CommandLine {
                program: "open".into(),
                args: vec![path.display().to_string()],
                file: Some((path, content)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(kind: SshConsole) -> ExternalTools {
        ExternalTools {
            ssh_console: kind,
            putty_location: "/opt/putty/putty".into(),
            openssh_location: "/usr/bin/ssh".into(),
            rdp_client: String::new(),
        }
    }

    #[test]
    fn ssh_command_lines() {
        let yes = |_: &Path| true;
        let c = ssh_command(
            &tools(SshConsole::System),
            Platform::MacOs,
            "root",
            "10.0.0.5",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "open ssh://root@10.0.0.5");
        let c = ssh_command(
            &tools(SshConsole::System),
            Platform::MacOs,
            "root",
            "fe80::1",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "open ssh://root@[fe80::1]");
        let c = ssh_command(
            &tools(SshConsole::System),
            Platform::Linux,
            "root",
            "10.0.0.5",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "x-terminal-emulator -e ssh root@10.0.0.5");
        let c = ssh_command(
            &tools(SshConsole::System),
            Platform::Windows,
            "root",
            "10.0.0.5",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "cmd.exe /c start '' ssh.exe root@10.0.0.5");
        let c = ssh_command(
            &tools(SshConsole::Putty),
            Platform::Windows,
            "root",
            "10.0.0.5",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "/opt/putty/putty 10.0.0.5");
        let c = ssh_command(
            &tools(SshConsole::OpenSsh),
            Platform::Linux,
            "root",
            "10.0.0.5",
            yes,
        )
        .unwrap();
        assert_eq!(c.to_string(), "/usr/bin/ssh root@10.0.0.5");
        let mut t = tools(SshConsole::Putty);
        t.putty_location.clear();
        assert_eq!(
            ssh_command(&t, Platform::Linux, "root", "10.0.0.5", yes),
            Err(SshError::NotConfigured)
        );
        assert_eq!(
            ssh_command(
                &tools(SshConsole::OpenSsh),
                Platform::Linux,
                "root",
                "1.2.3.4",
                |_| false
            ),
            Err(SshError::Missing("/usr/bin/ssh".into()))
        );
    }

    #[test]
    fn rdp_command_lines() {
        let t = tools(SshConsole::System);
        let temp = Path::new("/tmp/x");
        assert_eq!(
            rdp_command(&t, Platform::Windows, "10.0.0.7", "u", temp).to_string(),
            "mstsc.exe /v:10.0.0.7"
        );
        assert_eq!(
            rdp_command(&t, Platform::Linux, "[fe80::2]", "u", temp).to_string(),
            "xfreerdp /v:[fe80::2]"
        );
        let mac = rdp_command(&t, Platform::MacOs, "10.0.0.7", "u", temp);
        assert_eq!(mac.to_string(), "open /tmp/x/xenadmin-u.rdp");
        assert_eq!(
            mac.file.unwrap().1,
            "full address:s:10.0.0.7:3389\r\nprompt for credentials:i:1\r\n"
        );
        let mut t = t;
        t.rdp_client = "/usr/local/bin/wlfreerdp".into();
        assert_eq!(
            rdp_command(&t, Platform::MacOs, "10.0.0.7", "u", temp).to_string(),
            "/usr/local/bin/wlfreerdp /v:10.0.0.7"
        );
    }

    #[test]
    fn guest_addresses_in_key_order() {
        let mut n = std::collections::HashMap::new();
        n.insert("0/ipv6/0".to_owned(), "fe80::1".to_owned());
        n.insert("0/ip".to_owned(), "10.0.0.5".to_owned());
        n.insert("0/ipv4/0".to_owned(), "10.0.0.5\n10.0.0.6".to_owned());
        n.insert("1/ip".to_owned(), "192.168.1.9".to_owned());
        assert_eq!(
            find_ip_addresses(&n, "0"),
            vec!["10.0.0.5", "10.0.0.6", "fe80::1"]
        );
        assert_eq!(
            last_booted_guest_metrics("('guest_metrics' 'OpaqueRef:abc')").map(|r| r.to_string()),
            Some("OpaqueRef:abc".to_owned())
        );
    }

    /// The launcher really starts the program (a harmless local one) and
    /// writes the `.rdp` file first.
    #[cfg(unix)]
    #[test]
    fn the_launcher_starts_a_local_program() {
        let dir = std::env::temp_dir().join(format!("xenadmin-launch-{}", std::process::id()));
        let tools = ExternalTools {
            ssh_console: SshConsole::OpenSsh,
            openssh_location: "/usr/bin/true".into(),
            rdp_client: "/usr/bin/true".into(),
            ..ExternalTools::default()
        };
        let ssh = ssh_command(&tools, Platform::current(), "root", "127.0.0.1", |p| {
            p.exists()
        })
        .unwrap();
        assert_eq!(ssh.to_string(), "/usr/bin/true root@127.0.0.1");
        ssh.launch().unwrap();
        let rdp = rdp_command(&tools, Platform::current(), "127.0.0.1", "u", &dir);
        assert_eq!(rdp.to_string(), "/usr/bin/true /v:127.0.0.1");
        rdp.launch().unwrap();
        let mut with_file = rdp_command(
            &ExternalTools::default(),
            Platform::MacOs,
            "127.0.0.1",
            "u",
            &dir,
        );
        with_file.program = "/usr/bin/true".into();
        with_file.launch().unwrap();
        let path = dir.join("xenadmin-u.rdp");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("full address:s:127.0.0.1:3389")
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            CommandLine::new("/nonexistent/xenadmin-ssh", &[])
                .launch()
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_probe_takes_the_first_address_that_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let found = probe(
            &["not-an-ip".into(), "127.0.0.1".into()],
            port,
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(found.as_deref(), Some("127.0.0.1"));
        drop(listener);
    }
}

//! Performance alerts (`perfmon`): the XML a host, VM or SR keeps in
//! `other_config["perfmon"]`, the form of the original's "Alerts" page
//! (`PerfmonAlertEditPage`) and the body of the `ALARM` messages perfmon
//! sends.
//!
//! ```xml
//! <config>
//!   <variable>
//!     <name value="cpu_usage"/>
//!     <alarm_trigger_level value="0.9"/>
//!     <alarm_trigger_period value="60"/>
//!     <alarm_auto_inhibit_period value="3600"/>
//!   </variable>
//! </config>
//! ```
//!
//! Levels are in XAPI units (a fraction for CPU, B/s for network and disk,
//! KiB for free memory, MiB/s for SR throughput), periods in seconds. The
//! form shows them as the original does (percent, KB/s, MB, minutes).
//! Variables the form does not manage, and extra elements of a variable
//! (`alarm_priority`, `consolidation_fn`…), are kept when saving.

use quick_xml::Reader;
use quick_xml::events::Event;

/// `other_config` key holding the configuration.
pub const PERFMON_KEY: &str = "perfmon";

/// Variable names (`PerfmonDefinition.ALARM_TYPE_*`).
pub const CPU_USAGE: &str = "cpu_usage";
pub const NETWORK_USAGE: &str = "network_usage";
pub const DISK_USAGE: &str = "disk_usage";
pub const MEMORY_FREE: &str = "memory_free_kib";
pub const SR_IO_THROUGHPUT: &str = "sr_io_throughput_total_per_host";
pub const FS_USAGE: &str = "fs_usage";
pub const LOG_FS_USAGE: &str = "log_fs_usage";
pub const DOM0_MEMORY: &str = "mem_usage";
pub const SR_PHYSICAL_UTILISATION: &str = "physical_utilisation";

/// One `<variable>`.
#[derive(Debug, Clone, PartialEq)]
pub struct PerfmonVariable {
    pub name: String,
    /// `alarm_trigger_level`, XAPI units.
    pub level: Option<f64>,
    /// `alarm_trigger_period`, seconds.
    pub period: Option<f64>,
    /// `alarm_auto_inhibit_period`, seconds.
    pub inhibit: Option<f64>,
    /// Other elements of the variable, `(element, value)`, kept as found.
    pub extra: Vec<(String, String)>,
}

impl PerfmonVariable {
    pub fn new(name: &str, level: f64, period: f64, inhibit: f64) -> Self {
        Self {
            name: name.to_owned(),
            level: Some(level),
            period: Some(period),
            inhibit: Some(inhibit),
            extra: Vec::new(),
        }
    }

    /// The original ignores a variable without any value set.
    fn has_value(&self) -> bool {
        self.level.is_some() || self.period.is_some() || self.inhibit.is_some()
    }
}

/// A whole `perfmon` configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerfmonConfig {
    pub variables: Vec<PerfmonVariable>,
}

impl PerfmonConfig {
    /// Parses the XML; a configuration that cannot be read counts as empty
    /// (as the original's `GetPerfmonDefinitions`).
    pub fn parse(xml: &str) -> Self {
        parse_variables(xml)
            .map(|variables| Self { variables })
            .unwrap_or_default()
    }

    /// The configuration of an object's `other_config` (empty without one).
    pub fn of(other_config: &std::collections::HashMap<String, String>) -> Self {
        other_config
            .get(PERFMON_KEY)
            .map(|x| Self::parse(x))
            .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<&PerfmonVariable> {
        self.variables.iter().find(|v| v.name == name)
    }

    /// Writes the XML (`<config>…</config>`, no declaration).
    pub fn to_xml(&self) -> String {
        let mut out = String::from("<config>");
        for v in &self.variables {
            out.push_str("<variable>");
            element(&mut out, "name", &v.name);
            for (tag, value) in [
                ("alarm_trigger_level", v.level),
                ("alarm_trigger_period", v.period),
                ("alarm_auto_inhibit_period", v.inhibit),
            ] {
                if let Some(value) = value {
                    element(&mut out, tag, &number_text(value));
                }
            }
            for (tag, value) in &v.extra {
                element(&mut out, tag, value);
            }
            out.push_str("</variable>");
        }
        out.push_str("</config>");
        out
    }
}

fn element(out: &mut String, tag: &str, value: &str) {
    out.push('<');
    out.push_str(tag);
    out.push_str(" value=\"");
    out.push_str(&quick_xml::escape::escape(value));
    out.push_str("\" />");
}

/// A number as the original writes it (invariant culture, no exponent for
/// ordinary values, no trailing `.0`).
fn number_text(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v}");
        if s.contains('e') {
            format!("{v:.6}")
        } else {
            s
        }
    }
}

fn parse_variables(xml: &str) -> Option<Vec<PerfmonVariable>> {
    let mut reader = Reader::from_str(xml);
    let mut variables = Vec::new();
    let mut current: Option<PerfmonVariable> = None;
    let mut depth = 0usize;
    loop {
        let (event, empty) = match reader.read_event().ok()? {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1
                    && let Some(v) = current.take()
                    && v.has_value()
                {
                    variables.push(v);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let tag = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
        match depth {
            // Under the root: a variable starts.
            1 if tag == "variable" && !empty => {
                current = Some(PerfmonVariable {
                    name: String::new(),
                    level: None,
                    period: None,
                    inhibit: None,
                    extra: Vec::new(),
                });
            }
            2 => {
                if let Some(v) = current.as_mut() {
                    let value = event
                        .attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == b"value")
                        .and_then(|a| {
                            let raw = String::from_utf8_lossy(&a.value).into_owned();
                            quick_xml::escape::unescape(&raw)
                                .ok()
                                .map(|c| c.into_owned())
                        });
                    if let Some(value) = value {
                        let num = || crate::perfmon::parse_number(&value);
                        match tag.as_str() {
                            "name" => v.name = value.trim().to_owned(),
                            "alarm_trigger_level" => v.level = num(),
                            "alarm_trigger_period" => v.period = num(),
                            "alarm_auto_inhibit_period" => v.inhibit = num(),
                            _ => v.extra.push((tag.clone(), value)),
                        }
                    }
                }
            }
            _ => {}
        }
        if !empty {
            depth += 1;
        }
    }
    Some(variables)
}

fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

// ---- the Alerts page ------------------------------------------------------

/// What the edited object is (which groups the page shows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfmonObject {
    Vm,
    Host,
    Sr,
}

/// One group of the page: a check box, a threshold and a duration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlertSetting {
    pub enabled: bool,
    /// In the page's units: percent (CPU), KB/s (network, disk, SR), MB
    /// (free memory).
    pub level: f64,
    /// Minutes the level must hold before an alert.
    pub minutes: f64,
}

/// A group of the page and how its values convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertGroup {
    Cpu,
    Network,
    Disk,
    Memory,
    Sr,
}

impl AlertGroup {
    pub fn variable(self) -> &'static str {
        match self {
            Self::Cpu => CPU_USAGE,
            Self::Network => NETWORK_USAGE,
            Self::Disk => DISK_USAGE,
            Self::Memory => MEMORY_FREE,
            Self::Sr => SR_IO_THROUGHPUT,
        }
    }

    /// The groups the page shows for `object`, in its order.
    pub fn for_object(object: PerfmonObject) -> &'static [Self] {
        match object {
            PerfmonObject::Vm => &[Self::Cpu, Self::Network, Self::Disk],
            PerfmonObject::Host => &[Self::Cpu, Self::Network, Self::Memory],
            PerfmonObject::Sr => &[Self::Sr],
        }
    }

    /// XAPI level → page level.
    pub fn to_page(self, level: f64) -> f64 {
        match self {
            Self::Cpu => level * 100.0,
            Self::Network | Self::Disk | Self::Memory => level / 1024.0,
            Self::Sr => level * 1024.0,
        }
    }

    /// Page level → XAPI level.
    pub fn to_xapi(self, level: f64) -> f64 {
        match self {
            Self::Cpu => level / 100.0,
            Self::Network | Self::Disk | Self::Memory => level * 1024.0,
            Self::Sr => level / 1024.0,
        }
    }

    /// The page's default (the designer's initial values).
    pub fn default_level(self) -> f64 {
        match self {
            Self::Cpu => 50.0,
            Self::Network => 100.0,
            Self::Disk | Self::Memory | Self::Sr => 1000.0,
        }
    }

    /// Smallest and largest level the page accepts.
    pub fn level_range(self) -> (f64, f64) {
        match self {
            Self::Cpu => (5.0, 100.0),
            _ => (1.0, f64::from(i32::MAX)),
        }
    }
}

/// Minutes allowed for a duration and for the repeat interval.
pub const MINUTES_RANGE: (f64, f64) = (1.0, 60.0);
pub const INTERVAL_RANGE: (f64, f64) = (5.0, 86400.0);

/// The values of the Alerts page.
#[derive(Debug, Clone, PartialEq)]
pub struct PerfmonForm {
    pub object: PerfmonObject,
    pub groups: Vec<(AlertGroup, AlertSetting)>,
    /// "Alert repeat interval" in minutes (each variable's
    /// `alarm_auto_inhibit_period`); a multiple of 5.
    pub interval_minutes: f64,
}

impl PerfmonForm {
    /// The page filled from `config` (`Repopulate`).
    pub fn new(object: PerfmonObject, config: &PerfmonConfig) -> Self {
        let mut interval_minutes = 60.0;
        let groups = AlertGroup::for_object(object)
            .iter()
            .map(|&group| {
                let setting = match config.get(group.variable()) {
                    Some(v) => {
                        if let Some(inhibit) = v.inhibit {
                            interval_minutes = inhibit / 60.0;
                        }
                        AlertSetting {
                            enabled: true,
                            level: v
                                .level
                                .map(|l| group.to_page(l))
                                .unwrap_or(group.default_level()),
                            minutes: v.period.map(|p| p / 60.0).unwrap_or(1.0),
                        }
                    }
                    None => AlertSetting {
                        enabled: false,
                        level: group.default_level(),
                        minutes: 1.0,
                    },
                };
                (group, setting)
            })
            .collect();
        Self {
            object,
            groups,
            interval_minutes,
        }
    }

    pub fn setting(&self, group: AlertGroup) -> Option<&AlertSetting> {
        self.groups
            .iter()
            .find(|(g, _)| *g == group)
            .map(|(_, s)| s)
    }

    pub fn setting_mut(&mut self, group: AlertGroup) -> Option<&mut AlertSetting> {
        self.groups
            .iter_mut()
            .find(|(g, _)| *g == group)
            .map(|(_, s)| s)
    }

    /// Whether the values can be saved (`ValidToSave`): the repeat
    /// interval is a multiple of 5 minutes within its range.
    pub fn is_valid(&self) -> bool {
        let i = self.interval_minutes;
        i.fract() == 0.0
            && (i as i64) % 5 == 0
            && (INTERVAL_RANGE.0..=INTERVAL_RANGE.1).contains(&i)
    }

    /// The configuration to save: `original` with the page's variables
    /// replaced (enabled groups) or removed (disabled ones). An empty
    /// result means the key is removed (`PerfmonDefinitionAction`).
    pub fn apply(&self, original: &PerfmonConfig) -> PerfmonConfig {
        let managed: Vec<&str> = self.groups.iter().map(|(g, _)| g.variable()).collect();
        let mut variables: Vec<PerfmonVariable> = Vec::new();
        for (group, s) in self.groups.iter().filter(|(_, s)| s.enabled) {
            let mut v = PerfmonVariable::new(
                group.variable(),
                group.to_xapi(s.level),
                s.minutes * 60.0,
                self.interval_minutes * 60.0,
            );
            // Keep the variable's other elements.
            if let Some(old) = original.get(group.variable()) {
                v.extra = old.extra.clone();
            }
            variables.push(v);
        }
        variables.extend(
            original
                .variables
                .iter()
                .filter(|v| !managed.contains(&v.name.as_str()))
                .cloned(),
        );
        PerfmonConfig { variables }
    }
}

// ---- ALARM messages --------------------------------------------------------

/// What an `ALARM` message says (`AlarmMessageAlert.ParseAlarmMessage`):
///
/// ```text
/// value: 0.95
/// config:
/// <variable>
///   <name value="cpu_usage"/>
///   <alarm_trigger_level value="0.9"/>
///   <alarm_trigger_period value="60"/>
/// </variable>
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Alarm {
    /// The variable (`cpu_usage`, `network_usage`…; empty if unreadable).
    pub variable: String,
    pub value: f64,
    pub level: f64,
    /// Seconds.
    pub period: f64,
}

impl Alarm {
    pub fn parse(body: &str) -> Option<Self> {
        let mut lines = body.lines();
        let value = lines
            .next()?
            .trim()
            .strip_prefix("value:")
            .and_then(parse_number)
            .unwrap_or(0.0);
        let rest: String = lines.collect::<Vec<_>>().join("");
        let xml = rest.replace("config:", "");
        let config = PerfmonConfig::parse(&format!("<config>{}</config>", xml.trim()));
        let v = config.variables.first();
        Some(Self {
            variable: v.map(|v| v.name.clone()).unwrap_or_default(),
            value,
            level: v.and_then(|v| v.level).unwrap_or(0.0),
            period: v.and_then(|v| v.period).unwrap_or(0.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "<config><variable><name value=\"cpu_usage\" /><alarm_trigger_level value=\"0.9\" /><alarm_trigger_period value=\"60\" /><alarm_auto_inhibit_period value=\"3600\" /></variable><variable><name value=\"fs_usage\" /><alarm_trigger_level value=\"0.8\" /><alarm_priority value=\"2\" /></variable></config>";

    #[test]
    fn parses_and_writes_the_original_form() {
        let c = PerfmonConfig::parse(ORIGINAL);
        assert_eq!(c.variables.len(), 2);
        let cpu = c.get(CPU_USAGE).unwrap();
        assert_eq!(
            (cpu.level, cpu.period, cpu.inhibit),
            (Some(0.9), Some(60.0), Some(3600.0))
        );
        let fs = c.get(FS_USAGE).unwrap();
        assert_eq!(fs.extra, [("alarm_priority".to_owned(), "2".to_owned())]);
        assert_eq!(c.to_xml(), ORIGINAL);
        assert_eq!(PerfmonConfig::parse(&c.to_xml()), c);
        // Whitespace and an XML declaration are fine; garbage is empty.
        let spaced = "<?xml version=\"1.0\"?>\n<config>\n  <variable>\n    <name value=\"disk_usage\"/>\n    <alarm_trigger_level value=\"1048576\"/>\n  </variable>\n</config>";
        assert_eq!(
            PerfmonConfig::parse(spaced).get(DISK_USAGE).unwrap().level,
            Some(1048576.0)
        );
        assert!(
            PerfmonConfig::parse("<config><variable>")
                .variables
                .is_empty()
        );
        assert!(PerfmonConfig::parse("not xml <").variables.is_empty());
        // A variable without values is ignored, as in the original.
        assert!(
            PerfmonConfig::parse("<config><variable><name value=\"x\"/></variable></config>")
                .variables
                .is_empty()
        );
    }

    #[test]
    fn the_page_converts_units_like_the_original() {
        let c = PerfmonConfig::parse(ORIGINAL);
        let form = PerfmonForm::new(PerfmonObject::Vm, &c);
        let cpu = form.setting(AlertGroup::Cpu).unwrap();
        assert_eq!((cpu.enabled, cpu.level, cpu.minutes), (true, 90.0, 1.0));
        assert_eq!(form.interval_minutes, 60.0);
        let net = form.setting(AlertGroup::Network).unwrap();
        assert_eq!((net.enabled, net.level), (false, 100.0));
        assert!(form.setting(AlertGroup::Memory).is_none());
        assert!(form.is_valid());

        let mut form = form;
        form.setting_mut(AlertGroup::Network).unwrap().enabled = true;
        form.setting_mut(AlertGroup::Network).unwrap().level = 200.0;
        form.setting_mut(AlertGroup::Network).unwrap().minutes = 5.0;
        form.setting_mut(AlertGroup::Cpu).unwrap().enabled = false;
        form.interval_minutes = 10.0;
        let saved = form.apply(&c);
        assert!(saved.get(CPU_USAGE).is_none());
        let net = saved.get(NETWORK_USAGE).unwrap();
        assert_eq!(
            (net.level, net.period, net.inhibit),
            (Some(204800.0), Some(300.0), Some(600.0))
        );
        // Unmanaged variables are kept.
        assert_eq!(saved.get(FS_USAGE), c.get(FS_USAGE));

        // Host memory in MB ↔ KiB; SR in KB/s ↔ MiB/s.
        assert_eq!(AlertGroup::Memory.to_xapi(1000.0), 1_024_000.0);
        assert_eq!(AlertGroup::Sr.to_page(1.0), 1024.0);
        form.interval_minutes = 7.0;
        assert!(!form.is_valid());
    }

    #[test]
    fn nothing_enabled_means_no_configuration() {
        let form = PerfmonForm::new(PerfmonObject::Host, &PerfmonConfig::default());
        assert!(form.groups.iter().all(|(_, s)| !s.enabled));
        assert!(form.apply(&PerfmonConfig::default()).variables.is_empty());
    }

    #[test]
    fn alarm_bodies() {
        let a = Alarm::parse("value: 0.953\nconfig:\n<variable>\n\t<name value=\"cpu_usage\"/>\n\t<alarm_trigger_level value=\"0.9\"/>\n\t<alarm_trigger_period value=\"60\"/>\n</variable>\n").unwrap();
        assert_eq!(a.variable, CPU_USAGE);
        assert_eq!((a.value, a.level, a.period), (0.953, 0.9, 60.0));
        let b = Alarm::parse("something else").unwrap();
        assert_eq!(b.variable, "");
    }
}

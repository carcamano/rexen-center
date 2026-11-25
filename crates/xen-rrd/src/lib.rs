//! XAPI performance data (round-robin databases).
//!
//! XAPI keeps the performance counters of each host and VM in RRDs and
//! serves them over HTTP next to the JSON-RPC endpoint, authenticated with
//! `session_id=` in the query:
//!
//! * `GET /rrd_updates?start=…&cf=AVERAGE&interval=…&host=true` (or
//!   `&vm_uuid=…`): the rows of one archive newer than `start`
//!   ([`Updates`]);
//! * `GET /host_rrds` and `GET /vm_rrds?uuid=…`: a whole RRD, every archive
//!   ([`Rrd`]).
//!
//! Both answer in XML, or in JSON when `json=true` is in the query (checked
//! on XCP-ng 8.3). [`RrdClient`] asks for JSON and reads whichever format
//! comes back, so a server without JSON support still works through the
//! XML parser ([`parse_updates`], [`parse_rrd`]).
//!
//! [`Archives`] keeps the four archives the original console draws from
//! (5 s for 10 minutes, 1 min for 2 hours, 1 h for a week, 1 day for a
//! year), as its `ArchiveMaintainer` does.
//!
//! This crate does not depend on any UI toolkit or async runtime; the client
//! needs a tokio runtime to run on.

mod archive;
mod client;
mod json;
mod xml;

pub use archive::{Archives, Interval, Point};
pub use client::{Format, RrdClient, UpdatesQuery};

/// What went wrong reading performance data.
#[derive(Debug, thiserror::Error)]
pub enum RrdError {
    /// The request could not be made or answered (connection, TLS,
    /// timeout). The URL, which holds the session reference, is left out.
    #[error("transport error: {0}")]
    Transport(String),
    /// The server answered with an HTTP error.
    #[error("HTTP {0}")]
    Http(u16),
    /// The answer is neither the JSON nor the XML XAPI sends.
    #[error("cannot read the performance data: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, RrdError>;

/// Consolidation function of an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cf {
    Average,
    Min,
    Max,
    Last,
}

impl Cf {
    /// Parses `AVERAGE`, `MIN`, `MAX` or `LAST`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "AVERAGE" => Some(Self::Average),
            "MIN" => Some(Self::Min),
            "MAX" => Some(Self::Max),
            "LAST" => Some(Self::Last),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Average => "AVERAGE",
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::Last => "LAST",
        }
    }
}

/// The kind of object a data source belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Host,
    Vm,
    Sr,
    Other(String),
}

/// One column of `rrd_updates`: `AVERAGE:host:<uuid>:cpu0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId {
    pub cf: Cf,
    pub kind: ObjectKind,
    pub uuid: String,
    /// Data source name (`cpu0`, `memory_free_kib`, `vif_0_rx`…).
    pub name: String,
}

impl SourceId {
    /// Parses a legend entry `CF:kind:uuid:name` (the name may contain
    /// colons; a missing CF counts as `AVERAGE`, as the original's
    /// `DataSet.ParseId`).
    pub fn parse(entry: &str) -> Option<Self> {
        let parts: Vec<&str> = entry.trim().splitn(4, ':').collect();
        let (cf, rest) = match Cf::parse(parts.first()?) {
            Some(cf) => (cf, &parts[1..]),
            None => (Cf::Average, &parts[..]),
        };
        let (kind, uuid, name) = match rest {
            [kind, uuid, name] => (*kind, *uuid, (*name).to_owned()),
            // Without a CF the name may have been split at a colon.
            [kind, uuid, a, b] => (*kind, *uuid, format!("{a}:{b}")),
            _ => return None,
        };
        let kind = match kind {
            "host" => ObjectKind::Host,
            "vm" => ObjectKind::Vm,
            "sr" => ObjectKind::Sr,
            other => ObjectKind::Other(other.to_owned()),
        };
        Some(Self {
            cf,
            kind,
            uuid: uuid.to_owned(),
            name,
        })
    }
}

/// The answer to `rrd_updates`: rows of one archive, newest first as XAPI
/// sends them.
#[derive(Debug, Clone, PartialEq)]
pub struct Updates {
    pub start: i64,
    pub end: i64,
    /// Seconds between rows.
    pub step: i64,
    /// One entry per column.
    pub legend: Vec<SourceId>,
    pub rows: Vec<UpdateRow>,
}

/// One row of [`Updates`]: a time (Unix seconds) and one value per column
/// (NaN where the server had no value).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateRow {
    pub t: i64,
    pub values: Vec<f64>,
}

/// A whole round-robin database (`host_rrds`, `vm_rrds`).
#[derive(Debug, Clone, PartialEq)]
pub struct Rrd {
    /// Seconds per primary data point.
    pub step: i64,
    /// Time of the last update (Unix seconds, with a fraction).
    pub last_update: f64,
    /// Data source names, in column order.
    pub sources: Vec<String>,
    pub archives: Vec<Rra>,
}

/// One archive of an [`Rrd`].
#[derive(Debug, Clone, PartialEq)]
pub struct Rra {
    pub cf: Cf,
    /// Primary data points per row: 1 (5 s), 12 (1 min), 720 (1 h) or
    /// 17280 (1 day) on XAPI.
    pub pdp_per_row: i64,
    /// Rows, oldest first; one value per data source.
    pub rows: Vec<Vec<f64>>,
}

impl Rrd {
    /// Seconds between two rows of `rra`.
    pub fn row_step(&self, rra: &Rra) -> i64 {
        (self.step * rra.pdp_per_row).max(1)
    }

    /// The time of each row of `rra`: the last row is the last update
    /// rounded down to the archive's step, as rrdtool (and the original's
    /// `ArchiveMaintainer`) count.
    pub fn row_times(&self, rra: &Rra) -> Vec<i64> {
        let step = self.row_step(rra);
        let last = self.last_update.floor() as i64;
        let last_row = last - last.rem_euclid(step);
        let n = rra.rows.len() as i64;
        (0..n).map(|i| last_row - (n - 1 - i) * step).collect()
    }
}

/// Reads a number as XAPI writes it: plain decimals, `NaN`, `-nan`,
/// `Infinity`, `inf`…
pub(crate) fn number(text: &str) -> Option<f64> {
    let t = text.trim();
    if let Ok(v) = t.parse::<f64>() {
        return Some(v);
    }
    match t.to_ascii_lowercase().trim_start_matches(['+', '-']) {
        "nan" => Some(f64::NAN),
        "inf" | "infinity" => Some(if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }),
        _ => None,
    }
}

/// Parses an `rrd_updates` answer, JSON or XML (told apart by its first
/// character).
pub fn parse_updates(body: &[u8]) -> Result<Updates> {
    match first_char(body) {
        Some(b'{') => json::updates(body),
        Some(b'<') => xml::updates(body),
        _ => Err(RrdError::Decode("empty or unknown answer".to_owned())),
    }
}

/// Parses a `host_rrds` / `vm_rrds` answer, JSON or XML.
pub fn parse_rrd(body: &[u8]) -> Result<Rrd> {
    match first_char(body) {
        Some(b'{') => json::rrd(body),
        Some(b'<') => xml::rrd(body),
        _ => Err(RrdError::Decode("empty or unknown answer".to_owned())),
    }
}

/// The format of an answer, from its first character.
pub fn format_of(body: &[u8]) -> Option<Format> {
    match first_char(body) {
        Some(b'{') => Some(Format::Json),
        Some(b'<') => Some(Format::Xml),
        _ => None,
    }
}

fn first_char(body: &[u8]) -> Option<u8> {
    body.iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace() && *b != 0xEF && *b != 0xBB && *b != 0xBF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids() {
        let id = SourceId::parse("AVERAGE:host:0b0e-1:cpu0").unwrap();
        assert_eq!(
            (id.cf, id.kind, id.uuid.as_str(), id.name.as_str()),
            (Cf::Average, ObjectKind::Host, "0b0e-1", "cpu0")
        );
        let id = SourceId::parse("MAX:vm:abc:vbd_xvda_read").unwrap();
        assert_eq!((id.cf, id.kind), (Cf::Max, ObjectKind::Vm));
        // Without the CF (the original's three-part ids).
        let id = SourceId::parse("vm:abc:memory").unwrap();
        assert_eq!((id.cf, id.name.as_str()), (Cf::Average, "memory"));
        // Names with colons survive.
        let id = SourceId::parse("AVERAGE:host:abc:sr_x:y").unwrap();
        assert_eq!(id.name, "sr_x:y");
        assert!(SourceId::parse("AVERAGE:host").is_none());
    }

    #[test]
    fn numbers_as_xapi_writes_them() {
        assert_eq!(number("0.25"), Some(0.25));
        assert_eq!(number("1e3"), Some(1000.0));
        assert!(number("NaN").unwrap().is_nan());
        assert!(number("-nan").unwrap().is_nan());
        assert_eq!(number("Infinity"), Some(f64::INFINITY));
        assert_eq!(number("-inf"), Some(f64::NEG_INFINITY));
        assert_eq!(number("x"), None);
    }

    #[test]
    fn row_times_end_at_the_last_update() {
        let rrd = Rrd {
            step: 5,
            last_update: 1003.7,
            sources: vec!["a".into()],
            archives: vec![],
        };
        let rra = Rra {
            cf: Cf::Average,
            pdp_per_row: 12,
            rows: vec![vec![1.0], vec![2.0], vec![3.0]],
        };
        assert_eq!(rrd.row_times(&rra), vec![840, 900, 960]);
    }
}

//! The audit log: one JSON line per tool call (and per resource read) in
//! `mcp-audit.jsonl` in the application's log folder, appended and flushed
//! as each call ends.
//!
//! Each line has the time (UTC, RFC 3339), the tool, its arguments with
//! anything that looks like a secret replaced by `"[redacted]"`, the server
//! and target it resolved to, the outcome (`ok`, `refused` or `error`), a
//! detail (the refusal or error text, or the operation's status) and the
//! duration. Passwords, session references and the keyring never reach it:
//! the server does not hold them, and argument keys naming a secret are
//! redacted before anything is written.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

/// The file name, in the log folder.
pub const AUDIT_FILE: &str = "mcp-audit.jsonl";

/// What became of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    /// Refused by a rule (writes disabled, no confirmation, outside the
    /// allow-list, not offered now, not authorized) before any call.
    Refused,
    /// Bad arguments, an unknown object, or a failure of the server.
    Error,
}

/// One line of the audit log.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub time: String,
    pub tool: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub duration_ms: u128,
}

/// The audit log file (or nowhere, when there is no log folder).
#[derive(Debug)]
pub struct AuditLog {
    path: Option<PathBuf>,
    file: Mutex<Option<File>>,
}

impl AuditLog {
    /// Appends to `mcp-audit.jsonl` in `log_dir` (created if needed).
    pub fn in_dir(log_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(log_dir)?;
        let path = log_dir.join(AUDIT_FILE);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path: Some(path),
            file: Mutex::new(Some(file)),
        })
    }

    /// A log that only traces (no log folder).
    pub fn disabled() -> Self {
        Self {
            path: None,
            file: Mutex::new(None),
        }
    }

    /// The file written, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Writes one entry (and traces it).
    pub fn record(&self, entry: &AuditEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "audit entry not serializable");
                return;
            }
        };
        tracing::info!(tool = %entry.tool, outcome = ?entry.outcome, "mcp call");
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(f) = file.as_mut()
            && let Err(e) = writeln!(f, "{line}").and_then(|()| f.flush())
        {
            tracing::warn!(error = %e, "could not write the audit log");
        }
    }
}

/// Whether an argument key names a secret.
pub fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "session",
        "credential",
        "chap",
        "keyring",
        "auth",
    ]
    .iter()
    .any(|s| key.contains(s))
}

/// `value` with every object member whose key names a secret replaced by
/// `"[redacted]"`, at any depth.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if is_secret_key(k) {
                        Value::String("[redacted]".into())
                    } else {
                        redact(v)
                    };
                    (k.clone(), v)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// `time` as RFC 3339 UTC with milliseconds (`2026-11-26T10:00:00.000Z`).
pub fn rfc3339(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = since.as_secs() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since.subsec_millis()
    )
}

/// Unix seconds as RFC 3339 UTC (`2026-11-26T10:00:00Z`).
pub fn unix_rfc3339(seconds: i64) -> String {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    let rem = seconds.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_secret_keys_at_any_depth() {
        let args = json!({
            "vm": "web-01",
            "password": "hunter2",
            "nested": {"Session_Id": "OpaqueRef:abc", "list": [{"api_token": "t"}]},
            "confirm": true
        });
        let out = redact(&args);
        assert_eq!(out["vm"], "web-01");
        assert_eq!(out["password"], "[redacted]");
        assert_eq!(out["nested"]["Session_Id"], "[redacted]");
        assert_eq!(out["nested"]["list"][0]["api_token"], "[redacted]");
        assert_eq!(out["confirm"], true);
        assert!(!out.to_string().contains("hunter2"));
    }

    #[test]
    fn formats_utc_times() {
        assert_eq!(unix_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_rfc3339(1_795_721_817), "2026-11-26T19:36:57Z");
        assert_eq!(unix_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_795_721_817_250);
        assert_eq!(rfc3339(t), "2026-11-26T19:36:57.250Z");
    }
}

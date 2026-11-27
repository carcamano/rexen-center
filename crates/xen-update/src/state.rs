//! What the updater remembers between runs (`update.toml` in the
//! configuration folder): when it last checked, the version the user chose
//! to skip, and an update that had to be rolled back. And the rules that
//! use them: at most one automatic check a day, a skipped version is not
//! offered again by the automatic check.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::version::Version;

/// The file in the configuration folder.
pub const STATE_FILE: &str = "update.toml";

/// The automatic check runs at most this often.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The automatic check waits this long after the start, so that it never
/// competes with the window opening and the connections being restored.
pub const START_DELAY: Duration = Duration::from_secs(10);

/// The contents of `update.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// The last check that reached the release server (Unix seconds).
    pub last_check: Option<u64>,
    /// "Skip This Version": not offered again by the automatic check.
    pub skipped_version: Option<String>,
    /// An update that failed to start and was rolled back, to tell the
    /// user once.
    pub rolled_back: Option<RolledBack>,
}

/// An update rolled back after the new version failed to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolledBack {
    pub version: String,
    pub reason: String,
}

impl UpdateState {
    /// Reads the file; a missing or unreadable one gives the defaults (it
    /// only throttles and remembers a choice; never worth failing for).
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(file = %path.display(), error = %e, "ignoring a malformed update state");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the file atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Whether the automatic check should run now (`now` in Unix seconds):
    /// when it is on, and it never ran, or ran a day ago or more, or the
    /// clock went back since.
    pub fn check_due(&self, enabled: bool, now: u64) -> bool {
        if !enabled {
            return false;
        }
        match self.last_check {
            None => true,
            Some(last) if now < last => true,
            Some(last) => now - last >= CHECK_INTERVAL.as_secs(),
        }
    }

    /// Whether `latest` should be offered to `running`: a newer stable
    /// release, and, for the automatic check, not the skipped one. A
    /// manual check offers a skipped version all the same (the user asked).
    pub fn should_offer(&self, latest: &Version, running: &Version, manual: bool) -> bool {
        if !latest.is_update_for(running) {
            return false;
        }
        manual
            || self
                .skipped_version
                .as_deref()
                .and_then(Version::parse)
                .is_none_or(|skipped| &skipped != latest)
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where the state is kept: `update.toml` in `config_dir`.
pub fn state_file(config_dir: &Path) -> PathBuf {
    config_dir.join(STATE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn checks_at_most_once_a_day() {
        let never = UpdateState::default();
        assert!(never.check_due(true, 1_000));
        assert!(!never.check_due(false, 1_000), "turned off in Options");
        let state = UpdateState {
            last_check: Some(10 * DAY),
            ..UpdateState::default()
        };
        assert!(!state.check_due(true, 10 * DAY));
        assert!(!state.check_due(true, 11 * DAY - 1));
        assert!(state.check_due(true, 11 * DAY));
        assert!(state.check_due(true, 40 * DAY));
        assert!(!state.check_due(false, 40 * DAY));
        // The clock went back: check rather than wait for it.
        assert!(state.check_due(true, 9 * DAY));
    }

    #[test]
    fn skipped_versions_are_not_offered_automatically() {
        let running = v("0.1.1");
        let mut state = UpdateState::default();
        assert!(state.should_offer(&v("0.2.0"), &running, false));
        state.skipped_version = Some("0.2.0".into());
        assert!(!state.should_offer(&v("0.2.0"), &running, false));
        // The manual check offers it anyway.
        assert!(state.should_offer(&v("0.2.0"), &running, true));
        // A later release than the skipped one is offered again.
        assert!(state.should_offer(&v("0.2.1"), &running, false));
        // Never older, equal or pre-release, manual or not.
        for latest in ["0.1.1", "0.1.0", "0.3.0-rc.1"] {
            assert!(!state.should_offer(&v(latest), &running, true), "{latest}");
        }
        // A garbled skip does not hide anything.
        state.skipped_version = Some("garbage".into());
        assert!(state.should_offer(&v("0.2.0"), &running, false));
    }

    #[test]
    fn round_trips_and_tolerates_bad_files() {
        let dir = std::env::temp_dir().join(format!("xen-update-state-{}", std::process::id()));
        let path = state_file(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(UpdateState::load(&path), UpdateState::default());
        let state = UpdateState {
            last_check: Some(1_764_000_000),
            skipped_version: Some("0.2.0".into()),
            rolled_back: Some(RolledBack {
                version: "0.3.0".into(),
                reason: "it did not start".into(),
            }),
        };
        state.save(&path).unwrap();
        assert_eq!(UpdateState::load(&path), state);
        std::fs::write(&path, "last_check = [").unwrap();
        assert_eq!(UpdateState::load(&path), UpdateState::default());
        std::fs::write(&path, "skipped_version = \"0.2.0\"\n").unwrap();
        assert_eq!(UpdateState::load(&path).last_check, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

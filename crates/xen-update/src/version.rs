//! Release versions: the tags `v<version>` against the running version,
//! by semantic versioning. Only a stable release newer than the running
//! version is offered: never a pre-release, never a downgrade.

use std::cmp::Ordering;
use std::fmt;

/// A release version (`0.2.0`, from the tag `v0.2.0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version(semver::Version);

impl Version {
    /// Parses `1.2.3`, `v1.2.3` or `V1.2.3` (surrounding spaces allowed).
    /// Build metadata (`+…`) is kept but, as semantic versioning says,
    /// never decides an order.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text
            .strip_prefix('v')
            .or_else(|| text.strip_prefix('V'))
            .unwrap_or(text);
        semver::Version::parse(text).ok().map(Self)
    }

    /// A pre-release (`1.0.0-rc.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.0.pre.is_empty()
    }

    /// Whether this version is a stable release newer than `running`.
    pub fn is_update_for(&self, running: &Version) -> bool {
        !self.is_prerelease() && self.cmp(running) == Ordering::Greater
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // `semver::Version`'s own order also compares build metadata.
        self.0.cmp_precedence(&other.0)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text}"))
    }

    #[test]
    fn parses_tags_and_plain_versions() {
        assert_eq!(v("v0.2.0"), v("0.2.0"));
        assert_eq!(v(" V1.10.3 ").to_string(), "1.10.3");
        assert_eq!(v("1.0.0-rc.1").to_string(), "1.0.0-rc.1");
        for bad in ["", "v", "1", "1.2", "1.2.3.4", "latest", "vv1.2.3", "1.2.x"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn orders_numerically_not_textually() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.10") > v("0.1.2"));
        assert!(v("1.0.0") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        // Build metadata does not order.
        assert_eq!(v("1.0.0+abc").cmp(&v("1.0.0+def")), Ordering::Equal);
    }

    #[test]
    fn only_newer_stable_releases_are_updates() {
        let running = v("0.1.1");
        assert!(v("0.1.2").is_update_for(&running));
        assert!(v("0.2.0").is_update_for(&running));
        assert!(v("1.0.0").is_update_for(&running));
        // Same, older (never a downgrade), pre-release.
        assert!(!v("0.1.1").is_update_for(&running));
        assert!(!v("0.1.1+build.7").is_update_for(&running));
        assert!(!v("0.1.0").is_update_for(&running));
        assert!(!v("0.0.9").is_update_for(&running));
        assert!(!v("0.2.0-rc.1").is_update_for(&running));
        // A pre-release build is updated to its release.
        assert!(v("0.2.0").is_update_for(&v("0.2.0-rc.1")));
    }
}

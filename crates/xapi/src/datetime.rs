//! XenAPI date-time values.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A XenAPI `datetime`, kept as the string the server sent.
///
/// XAPI sends ISO 8601 basic or extended forms in UTC, such as
/// `20251020T15:31:42Z` or `2025-10-20T15:31:42Z`, sometimes with a fractional
/// part or a numeric offset; `19700101T00:00:00Z` means "never". Keeping the
/// raw string means decoding never fails; [`DateTime::unix_seconds`] parses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DateTime(pub String);

impl DateTime {
    /// The raw wire string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Seconds since the Unix epoch (UTC), if the value can be parsed.
    pub fn unix_seconds(&self) -> Option<i64> {
        parse_unix_seconds(&self.0)
    }

    /// Whether this is the epoch or empty, which XAPI uses for "never/unset".
    pub fn is_unset(&self) -> bool {
        self.unix_seconds().is_none_or(|s| s == 0)
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for DateTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DateTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(match Value::deserialize(d)? {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        }))
    }
}

fn parse_unix_seconds(s: &str) -> Option<i64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
    if digits.len() < 14 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| digits[r].parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (hour, minute, second) = (num(8..10)?, num(10..12)?, num(12..14)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    // Offset suffix (`Z`, `+hh:mm`, `-hhmm`) after the time part.
    let time_start = s.find('T')?;
    let tail = &s[time_start..];
    let offset = match tail.rfind(['+', '-']) {
        Some(i) => {
            let sign = if tail.as_bytes()[i] == b'-' { -1 } else { 1 };
            let d: String = tail[i + 1..]
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if d.len() < 2 {
                return None;
            }
            let h: i64 = d[..2].parse().ok()?;
            let m: i64 = d.get(2..4).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (h * 3600 + m * 60)
        }
        None => 0,
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xapi_forms() {
        let t = |s: &str| DateTime(s.into()).unix_seconds();
        assert_eq!(t("19700101T00:00:00Z"), Some(0));
        assert_eq!(t("20251020T15:31:42Z"), Some(1_760_974_302));
        assert_eq!(t("2025-10-20T15:31:42Z"), Some(1_760_974_302));
        assert_eq!(t("2025-10-20T15:31:42.123Z"), Some(1_760_974_302));
        assert_eq!(t("2025-10-20T17:31:42+02:00"), Some(1_760_974_302));
        assert_eq!(t("20251020T13:31:42-0200"), Some(1_760_974_302));
        assert_eq!(t("garbage"), None);
        assert!(DateTime("19700101T00:00:00Z".into()).is_unset());
        let d: DateTime = serde_json::from_str("null").unwrap();
        assert!(d.is_unset());
    }
}

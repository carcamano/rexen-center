//! Value formatting shared by the detail tabs.
//!
//! The rules follow the original console's `Util.MemorySizeStringSuitableUnits`
//! and `Util.DiskSizeString` (binary units, `kB`/`MB`/`GB`/`TB` labels), so the
//! numbers match what users of the original see.

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;
const TIB: f64 = GIB * 1024.0;

/// Memory size in the most suitable unit: `0`, `512 B`, `12 kB`, `768 MB`,
/// `4.0 GB` (one decimal up to 100 GB, none above). `point0` = false drops the
/// decimal for gigabytes too (`4 GB`).
pub fn memory_size(bytes: i64, point0: bool) -> String {
    let b = bytes as f64;
    if b >= GIB {
        let gb = b / GIB;
        if gb > 100.0 || !point0 {
            let dp = if gb > 100.0 { 0 } else { 1 };
            return format!("{} GB", trim_number(round_away(gb, dp)));
        }
        return format!("{:.1} GB", round_away(gb, 1));
    }
    if b >= MIB {
        return format!("{} MB", round_even(b / MIB, 0));
    }
    if b >= KIB {
        return format!("{} kB", round_even(b / KIB, 0));
    }
    if bytes == 0 {
        return "0".to_owned();
    }
    format!("{bytes} B")
}

/// Disk size with up to one decimal, trailing zeros dropped: `20 GB`,
/// `1.5 TB`, `512 B`. Negative values are shown by magnitude.
pub fn disk_size(bytes: i64) -> String {
    disk_size_dp(bytes, 1)
}

/// [`disk_size`] with `dp` decimal places at most.
pub fn disk_size_dp(bytes: i64, dp: u32) -> String {
    let b = bytes.unsigned_abs() as f64;
    let (value, unit) = if b >= TIB {
        (b / TIB, "TB")
    } else if b >= GIB {
        (b / GIB, "GB")
    } else if b >= MIB {
        (b / MIB, "MB")
    } else if b >= KIB {
        (b / KIB, "kB")
    } else {
        return format!("{} B", bytes.unsigned_abs());
    };
    format!("{} {unit}", trim_number(round_even(value, dp)))
}

/// Network speed of a NIC as XAPI reports it (megabits per second).
pub fn nic_speed(mbit: i64) -> String {
    format!("{mbit} Mbit/s")
}

/// Rounds half away from zero (`MidpointRounding.AwayFromZero`).
fn round_away(v: f64, dp: u32) -> f64 {
    let m = 10f64.powi(dp as i32);
    (v * m).round() / m
}

/// Rounds half to even (.NET's default `Math.Round`).
fn round_even(v: f64, dp: u32) -> f64 {
    let m = 10f64.powi(dp as i32);
    (v * m).round_ties_even() / m
}

/// Shortest decimal form: `2` rather than `2.0`, `1.5` stays.
fn trim_number(v: f64) -> String {
    let s = format!("{v}");
    if let Some(stripped) = s.strip_suffix(".0") {
        stripped.to_owned()
    } else {
        s
    }
}

/// Days, hours and minutes of a duration in whole seconds, as the original's
/// "uptime" rows count them (negative durations count as zero).
pub fn split_duration(seconds: i64) -> (i64, i64, i64) {
    let seconds = seconds.max(0);
    (
        seconds / 86_400,
        (seconds % 86_400) / 3600,
        (seconds % 3600) / 60,
    )
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Civil date (year, month 1-12, day) of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A UTC timestamp in the original's date format (`MMM d, yyyy h:mm:ss tt`,
/// or without seconds), marked UTC: `Nov 24, 2026 5:49:29 PM UTC`.
pub fn datetime_utc(unix_seconds: i64, with_seconds: bool) -> String {
    let (year, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    let secs = unix_seconds.rem_euclid(86_400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    let month = MONTHS[(month - 1) as usize];
    if with_seconds {
        format!("{month} {day}, {year} {h12}:{m:02}:{s:02} {ampm} UTC")
    } else {
        format!("{month} {day}, {year} {h12}:{m:02} {ampm} UTC")
    }
}

/// The UTC time of day, `HH:MM` (`HH:MM:SS` with seconds), for graph
/// axes.
pub fn clock_utc(unix_seconds: i64, with_seconds: bool) -> String {
    let secs = unix_seconds.rem_euclid(86_400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if with_seconds {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// The UTC day, `MMM d`, for graph axes.
pub fn day_utc(unix_seconds: i64) -> String {
    let (_, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    format!("{} {day}", MONTHS[(month - 1) as usize])
}

/// A date only (`MMM d, yyyy`).
pub fn date(year: i64, month: u32, day: u32) -> Option<String> {
    let name = MONTHS.get(month.checked_sub(1)? as usize)?;
    (1..=31)
        .contains(&day)
        .then(|| format!("{name} {day}, {year}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_sizes_follow_the_original_rules() {
        assert_eq!(memory_size(0, true), "0");
        assert_eq!(memory_size(512, true), "512 B");
        assert_eq!(memory_size(12 * 1024, true), "12 kB");
        assert_eq!(memory_size(768 * 1024 * 1024, true), "768 MB");
        assert_eq!(memory_size(4 << 30, true), "4.0 GB");
        assert_eq!(memory_size(4 << 30, false), "4 GB");
        assert_eq!(memory_size(1_610_612_736, true), "1.5 GB");
        assert_eq!(memory_size(17_179_869_184, true), "16.0 GB");
        assert_eq!(memory_size(200 << 30, true), "200 GB");
    }

    #[test]
    fn disk_sizes_drop_trailing_zeros() {
        assert_eq!(disk_size(0), "0 B");
        assert_eq!(disk_size(20 << 30), "20 GB");
        assert_eq!(disk_size(1_610_612_736), "1.5 GB");
        assert_eq!(disk_size(3 << 40), "3 TB");
        assert_eq!(disk_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(disk_size(-(2 << 30)), "2 GB");
        assert_eq!(disk_size_dp(1_700_000_000, 2), "1.58 GB");
    }

    #[test]
    fn durations_and_dates() {
        assert_eq!(split_duration(59), (0, 0, 0));
        assert_eq!(split_duration(3660), (0, 1, 1));
        assert_eq!(split_duration(86_400 * 3 + 4 * 3600 + 5 * 60), (3, 4, 5));
        assert_eq!(split_duration(-5), (0, 0, 0));
        assert_eq!(datetime_utc(0, true), "Jan 1, 1970 12:00:00 AM UTC");
        // 2026-11-25T17:49:29Z
        assert_eq!(
            datetime_utc(1_795_628_969, true),
            "Nov 25, 2026 5:49:29 PM UTC"
        );
        assert_eq!(
            datetime_utc(951_782_400, false),
            "Feb 29, 2000 12:00 AM UTC"
        );
        assert_eq!(date(2026, 1, 15).as_deref(), Some("Jan 15, 2026"));
        assert_eq!(clock_utc(1_795_628_969, false), "17:49");
        assert_eq!(clock_utc(1_795_628_969, true), "17:49:29");
        assert_eq!(day_utc(1_795_628_969), "Nov 25");
        assert_eq!(date(2026, 13, 1), None);
    }
}

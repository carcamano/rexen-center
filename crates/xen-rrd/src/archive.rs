//! The four archives a performance view draws from, as the original's
//! `ArchiveMaintainer` keeps them: 5-second rows for the last 10 minutes,
//! 1-minute rows for 2 hours, hourly rows for a week and daily rows for a
//! year.

use std::collections::{BTreeSet, HashMap};

use crate::{Cf, Rrd, SourceId, Updates};

/// A point: Unix seconds and a value (NaN for "unknown").
pub type Point = (i64, f64);

/// One archive's resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Interval {
    FiveSeconds,
    OneMinute,
    OneHour,
    OneDay,
}

impl Interval {
    /// Finest first.
    pub const ALL: [Self; 4] = [
        Self::FiveSeconds,
        Self::OneMinute,
        Self::OneHour,
        Self::OneDay,
    ];

    /// Seconds between rows (the `interval` of `rrd_updates`).
    pub fn seconds(self) -> i64 {
        match self {
            Self::FiveSeconds => 5,
            Self::OneMinute => 60,
            Self::OneHour => 3600,
            Self::OneDay => 86400,
        }
    }

    /// The time this archive covers: 10 minutes, 2 hours, 1 week, 1 year
    /// (366 days).
    pub fn span(self) -> i64 {
        self.seconds() * self.rows() as i64
    }

    /// Rows the server keeps in this archive.
    pub fn rows(self) -> usize {
        match self {
            Self::FiveSeconds => 120,
            Self::OneMinute => 120,
            Self::OneHour => 168,
            Self::OneDay => 366,
        }
    }

    /// Points kept here (the original keeps four more 5-second points).
    pub fn max_points(self) -> usize {
        match self {
            Self::FiveSeconds => 124,
            other => other.rows(),
        }
    }

    /// The archive with rows `seconds` apart.
    pub fn from_seconds(seconds: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.seconds() == seconds)
    }

    /// The archive to draw a range `width` seconds wide from
    /// (`DataPlotNav.GetCurrentWidthArchiveInterval`).
    pub fn for_width(width: i64) -> Self {
        Self::ALL
            .into_iter()
            .find(|i| width < i.span())
            .unwrap_or(Self::OneDay)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The next finer archive.
    pub fn finer(self) -> Option<Self> {
        self.index().checked_sub(1).map(|i| Self::ALL[i])
    }
}

/// The series of one object, by data source name, in each archive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Archives {
    sets: [HashMap<String, Vec<Point>>; 4],
}

impl Archives {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the AVERAGE archives of a whole RRD (`host_rrds`, `vm_rrds`).
    /// Archives of other resolutions are ignored.
    pub fn load_rrd(&mut self, rrd: &Rrd) {
        for rra in rrd.archives.iter().filter(|a| a.cf == Cf::Average) {
            let Some(interval) = Interval::from_seconds(rrd.row_step(rra)) else {
                continue;
            };
            let times = rrd.row_times(rra);
            for (col, name) in rrd.sources.iter().enumerate() {
                let points: Vec<Point> = times
                    .iter()
                    .zip(&rra.rows)
                    .map(|(t, row)| (*t, row.get(col).copied().unwrap_or(f64::NAN)))
                    .collect();
                self.insert(interval, name, &points);
            }
        }
    }

    /// Adds the rows of an `rrd_updates` answer to `interval`, for the
    /// columns `keep` accepts (stored under their data source name).
    pub fn merge_updates(
        &mut self,
        interval: Interval,
        updates: &Updates,
        keep: impl Fn(&SourceId) -> bool,
    ) {
        for (col, id) in updates.legend.iter().enumerate() {
            if !keep(id) {
                continue;
            }
            let points: Vec<Point> = updates
                .rows
                .iter()
                .map(|row| (row.t, row.values.get(col).copied().unwrap_or(f64::NAN)))
                .collect();
            self.insert(interval, &id.name, &points);
        }
    }

    /// Adds points to one series (a point at a time already there replaces
    /// it), keeping the newest [`Interval::max_points`].
    pub fn insert(&mut self, interval: Interval, name: &str, points: &[Point]) {
        let series = self.sets[interval.index()]
            .entry(name.to_owned())
            .or_default();
        for &(t, v) in points {
            match series.binary_search_by_key(&t, |p| p.0) {
                Ok(i) => series[i].1 = v,
                Err(i) => series.insert(i, (t, v)),
            }
        }
        let max = interval.max_points();
        if series.len() > max {
            series.drain(..series.len() - max);
        }
    }

    /// One series of one archive, oldest first.
    pub fn series(&self, interval: Interval, name: &str) -> &[Point] {
        self.sets[interval.index()]
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Every data source name with points in some archive.
    pub fn names(&self) -> BTreeSet<&str> {
        self.sets
            .iter()
            .flat_map(|s| s.iter())
            .filter(|(_, p)| !p.is_empty())
            .map(|(n, _)| n.as_str())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.sets.iter().all(|s| s.values().all(Vec::is_empty))
    }

    /// The newest point time of an archive (any series).
    pub fn last_time(&self, interval: Interval) -> Option<i64> {
        self.sets[interval.index()]
            .values()
            .filter_map(|s| s.last().map(|p| p.0))
            .max()
    }

    /// The points of `name` to draw between `from` and `to` (Unix
    /// seconds): the archive [`Interval::for_width`] picks, then the
    /// finer archives' points newer than its last one, averaged to its
    /// step, so the line reaches the present at the same resolution
    /// (`GetFinerPoints` in the original; each average is placed at its
    /// newest point). One point before `from` is kept so the line starts at
    /// the left edge.
    pub fn points(&self, name: &str, from: i64, to: i64) -> Vec<Point> {
        let wanted = Interval::for_width(to - from);
        let mut interval = wanted;
        let base = self.series(interval, name);
        let start = base.partition_point(|p| p.0 < from).saturating_sub(1);
        let mut out: Vec<Point> = base[start..]
            .iter()
            .copied()
            .take_while(|p| p.0 <= to)
            .collect();
        let mut newer: Vec<Point> = Vec::new();
        while let Some(finer) = interval.finer() {
            let last = newer.last().or(out.last()).map(|p| p.0).unwrap_or(i64::MIN);
            newer.extend(
                self.series(finer, name)
                    .iter()
                    .copied()
                    .filter(|p| p.0 > last && p.0 >= from && p.0 <= to),
            );
            interval = finer;
        }
        out.extend(average(&newer, wanted.seconds()));
        out
    }
}

/// Averages `points` (time order) over buckets `step` seconds long; each
/// average sits at the newest point of its bucket. Unknown values are left
/// out (a bucket of unknowns stays unknown).
fn average(points: &[Point], step: i64) -> Vec<Point> {
    // (bucket, newest time, sum, count)
    let mut out: Vec<Point> = Vec::new();
    let mut bucket: Option<(i64, i64, f64, usize)> = None;
    let flush = |b: Option<(i64, i64, f64, usize)>, out: &mut Vec<Point>| {
        if let Some((_, t, sum, n)) = b {
            out.push((t, if n == 0 { f64::NAN } else { sum / n as f64 }));
        }
    };
    for &(t, v) in points {
        let b = (t - 1).div_euclid(step.max(1));
        match bucket.as_mut() {
            Some(cur) if cur.0 == b => {
                cur.1 = t;
                if v.is_finite() {
                    cur.2 += v;
                    cur.3 += 1;
                }
            }
            _ => {
                flush(bucket.take(), &mut out);
                let known = v.is_finite();
                bucket = Some((b, t, if known { v } else { 0.0 }, usize::from(known)));
            }
        }
    }
    flush(bucket, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectKind, Rra, UpdateRow};

    #[test]
    fn widths_pick_archives_like_the_original() {
        assert_eq!(Interval::for_width(599), Interval::FiveSeconds);
        assert_eq!(Interval::for_width(600), Interval::OneMinute);
        assert_eq!(Interval::for_width(7199), Interval::OneMinute);
        assert_eq!(Interval::for_width(7 * 86400 - 1), Interval::OneHour);
        assert_eq!(Interval::for_width(7 * 86400), Interval::OneDay);
        assert_eq!(Interval::for_width(366 * 86400), Interval::OneDay);
        assert_eq!(Interval::OneDay.span(), 366 * 86400);
        assert_eq!(Interval::from_seconds(3600), Some(Interval::OneHour));
        assert_eq!(Interval::from_seconds(7), None);
    }

    #[test]
    fn inserting_replaces_sorts_and_trims() {
        let mut a = Archives::new();
        a.insert(Interval::OneMinute, "cpu0", &[(120, 2.0), (60, 1.0)]);
        a.insert(Interval::OneMinute, "cpu0", &[(120, 3.0), (180, 4.0)]);
        assert_eq!(
            a.series(Interval::OneMinute, "cpu0"),
            &[(60, 1.0), (120, 3.0), (180, 4.0)]
        );
        let many: Vec<Point> = (0..200).map(|i| (i * 60, i as f64)).collect();
        a.insert(Interval::OneMinute, "cpu0", &many);
        let s = a.series(Interval::OneMinute, "cpu0");
        assert_eq!(s.len(), 120);
        assert_eq!(s.last(), Some(&(199 * 60, 199.0)));
        assert!(a.series(Interval::OneHour, "cpu0").is_empty());
    }

    #[test]
    fn updates_are_merged_by_name_for_the_wanted_object() {
        let id = |kind, uuid: &str, name: &str| SourceId {
            cf: Cf::Average,
            kind,
            uuid: uuid.into(),
            name: name.into(),
        };
        let updates = Updates {
            start: 0,
            end: 10,
            step: 5,
            legend: vec![
                id(ObjectKind::Host, "h", "cpu0"),
                id(ObjectKind::Vm, "v", "cpu0"),
            ],
            rows: vec![
                UpdateRow {
                    t: 10,
                    values: vec![0.5, 0.9],
                },
                UpdateRow {
                    t: 5,
                    values: vec![0.4, f64::NAN],
                },
            ],
        };
        let mut a = Archives::new();
        a.merge_updates(Interval::FiveSeconds, &updates, |s| {
            s.kind == ObjectKind::Host
        });
        assert_eq!(
            a.series(Interval::FiveSeconds, "cpu0"),
            &[(5, 0.4), (10, 0.5)]
        );
        assert_eq!(a.names().into_iter().collect::<Vec<_>>(), ["cpu0"]);
        assert_eq!(a.last_time(Interval::FiveSeconds), Some(10));
    }

    #[test]
    fn full_rrds_fill_every_average_archive() {
        let rra = |cf, pdp, rows: Vec<Vec<f64>>| Rra {
            cf,
            pdp_per_row: pdp,
            rows,
        };
        let rrd = Rrd {
            step: 5,
            last_update: 86400.0 * 10.0 + 7.0,
            sources: vec!["a".into(), "b".into()],
            archives: vec![
                rra(Cf::Average, 1, vec![vec![1.0, 2.0], vec![3.0, 4.0]]),
                rra(Cf::Max, 1, vec![vec![9.0, 9.0]]),
                rra(Cf::Average, 12, vec![vec![5.0, 6.0]]),
                rra(Cf::Average, 720, vec![vec![7.0, 8.0]]),
                rra(Cf::Average, 17280, vec![vec![10.0, 11.0]]),
            ],
        };
        let mut a = Archives::new();
        a.load_rrd(&rrd);
        let end = 864_005;
        assert_eq!(
            a.series(Interval::FiveSeconds, "b"),
            &[(end - 5, 2.0), (end, 4.0)]
        );
        assert_eq!(a.series(Interval::OneMinute, "a"), &[(864_000, 5.0)]);
        assert_eq!(a.series(Interval::OneHour, "a"), &[(864_000, 7.0)]);
        assert_eq!(a.series(Interval::OneDay, "b"), &[(864_000, 11.0)]);
    }

    #[test]
    fn drawn_points_reach_the_present_through_finer_archives() {
        let mut a = Archives::new();
        a.insert(
            Interval::OneMinute,
            "x",
            &[(0, 1.0), (60, 2.0), (120, 3.0), (180, 4.0)],
        );
        a.insert(
            Interval::FiveSeconds,
            "x",
            &[(175, 9.0), (185, 5.0), (190, 6.0)],
        );
        // 2 hours wide: the minute archive, then the newer 5 s points
        // averaged to a minute.
        let p = a.points("x", 190 - 7000, 190);
        assert_eq!(
            p,
            vec![(0, 1.0), (60, 2.0), (120, 3.0), (180, 4.0), (190, 5.5)]
        );
        assert_eq!(
            average(&[(5, 1.0), (60, 3.0), (65, f64::NAN), (70, 4.0)], 60),
            vec![(60, 2.0), (70, 4.0)]
        );
        // From 100: one point before the edge is kept.
        assert_eq!(a.points("x", 100, 190)[0], (175, 9.0));
        let wide = a.points("x", 130, 130 + 1000);
        assert_eq!(wide[0], (120, 3.0));
        assert!(a.points("y", 0, 100).is_empty());
    }
}

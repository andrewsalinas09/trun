//! What checks can see about a run, as time series.
//!
//! [`RunData`] is built by ingesting the run's [`Event`]s in order, the same events
//! that are stored. The live agent feeds it as events happen, and `trun check test`
//! rebuilds it from a recorded run, so a replay sees exactly what the live check saw
//! (apart from process and host samples, which are not recorded yet).

use std::collections::{BTreeMap, HashMap, VecDeque};
use trun_proto::{Event, EventKind, Lifecycle, Millis};

/// How much history a series keeps. Checks look back minutes, not days.
pub const RETAIN_MS: Millis = 2 * 60 * 60 * 1000;
pub const MAX_POINTS: usize = 200_000;

/// Timestamped values, oldest first. Aggregations take a trailing window in seconds
/// ending at `now`; `None` means "no data in the window".
#[derive(Debug, Clone, Default)]
pub struct Series {
    pts: VecDeque<(Millis, f64)>,
}

impl Series {
    pub fn push(&mut self, t: Millis, v: f64) {
        self.pts.push_back((t, v));
        while self.pts.len() > MAX_POINTS
            || self.pts.front().is_some_and(|(t0, _)| t - t0 > RETAIN_MS)
        {
            self.pts.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.pts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }

    fn window(&self, now: Millis, secs: Option<f64>) -> impl Iterator<Item = &(Millis, f64)> {
        let from = secs
            .map(|s| now - (s * 1000.0) as Millis)
            .unwrap_or(Millis::MIN);
        self.pts
            .iter()
            .filter(move |(t, _)| *t >= from && *t <= now)
    }

    fn finite(&self, now: Millis, secs: Option<f64>) -> Vec<(Millis, f64)> {
        self.window(now, secs)
            .filter(|(_, v)| v.is_finite())
            .copied()
            .collect()
    }

    /// Latest value at or before `now` (may be NaN: that is the point of `is_nan`).
    pub fn last(&self, now: Millis) -> Option<f64> {
        self.pts
            .iter()
            .rev()
            .find(|(t, _)| *t <= now)
            .map(|(_, v)| *v)
    }

    pub fn avg(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        let f = self.finite(now, secs);
        (!f.is_empty()).then(|| f.iter().map(|(_, v)| v).sum::<f64>() / f.len() as f64)
    }

    pub fn min(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        self.finite(now, secs)
            .into_iter()
            .map(|(_, v)| v)
            .reduce(f64::min)
    }

    pub fn max(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        self.finite(now, secs)
            .into_iter()
            .map(|(_, v)| v)
            .reduce(f64::max)
    }

    pub fn count(&self, now: Millis, secs: Option<f64>) -> usize {
        self.window(now, secs).count()
    }

    pub fn count_non_finite(&self, now: Millis, secs: Option<f64>) -> usize {
        self.window(now, secs)
            .filter(|(_, v)| !v.is_finite())
            .count()
    }

    pub fn stddev(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        let f = self.finite(now, secs);
        if f.len() < 2 {
            return None;
        }
        let mean = f.iter().map(|(_, v)| v).sum::<f64>() / f.len() as f64;
        let var = f.iter().map(|(_, v)| (v - mean).powi(2)).sum::<f64>() / (f.len() - 1) as f64;
        Some(var.sqrt())
    }

    /// Least-squares slope, units per second. Needs two points spanning some time.
    pub fn slope(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        let f = self.finite(now, secs);
        if f.len() < 2 {
            return None;
        }
        let t0 = f[0].0;
        let xs: Vec<f64> = f.iter().map(|(t, _)| (t - t0) as f64 / 1000.0).collect();
        let n = f.len() as f64;
        let mx = xs.iter().sum::<f64>() / n;
        let my = f.iter().map(|(_, v)| v).sum::<f64>() / n;
        let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        if sxx == 0.0 {
            return None;
        }
        let sxy: f64 = xs
            .iter()
            .zip(&f)
            .map(|(x, (_, y))| (x - mx) * (y - my))
            .sum();
        Some(sxy / sxx)
    }

    /// Last minus first finite value in the window.
    pub fn delta(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        let f = self.finite(now, secs);
        Some(f.last()?.1 - f.first()?.1)
    }

    /// Change per second over the window (first to last point).
    pub fn rate(&self, now: Millis, secs: Option<f64>) -> Option<f64> {
        let f = self.finite(now, secs);
        let (a, b) = (f.first()?, f.last()?);
        (b.0 > a.0).then(|| (b.1 - a.1) / ((b.0 - a.0) as f64 / 1000.0))
    }

    /// Seconds since the latest point.
    pub fn age(&self, now: Millis) -> Option<f64> {
        self.pts
            .iter()
            .rev()
            .find(|(t, _)| *t <= now)
            .map(|(t, _)| (now - t) as f64 / 1000.0)
    }
}

#[derive(Debug, Clone)]
pub struct StepInfo {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub running: bool,
    pub current: Option<f64>,
    pub total: Option<f64>,
    pub unit: Option<String>,
    pub started_at: Millis,
}

/// Everything a check can read about one run.
#[derive(Debug, Clone, Default)]
pub struct RunData {
    pub id: String,
    pub name: String,
    pub project: String,
    pub lifecycle: Lifecycle,
    pub started_at: Option<Millis>,
    pub ended_at: Option<Millis>,
    pub last_output_at: Option<Millis>,
    /// Output, heartbeat, or any structured event.
    pub last_activity_at: Option<Millis>,
    pub expect_silence_ms: Option<Millis>,
    /// Output line timestamps (for lines-per-minute).
    pub output: Series,
    pub metrics: HashMap<String, Series>,
    /// `current` of each step over time, recorded when it changes.
    pub progress: HashMap<String, Series>,
    pub steps: Vec<StepInfo>,
    // Samples (live only; filled by the agent's sampler).
    pub proc_cpu: Series,
    pub proc_mem: Series,
    pub host_cpu: Series,
    pub host_mem: Series,
    pub disk_free: Series,
    pub gpu_util: Series,
    pub gpu_mem: Series,
    /// Thresholds from `[defaults.checks]` in config.toml, as seconds or numbers.
    pub config: BTreeMap<String, f64>,
}

impl RunData {
    pub fn new(id: &str, name: &str, project: &str) -> Self {
        RunData {
            id: id.into(),
            name: name.into(),
            project: project.into(),
            ..Default::default()
        }
    }

    fn touch(&mut self, t: Millis) {
        self.last_activity_at = Some(self.last_activity_at.map_or(t, |a| a.max(t)));
    }

    pub fn heartbeat(&mut self, t: Millis) {
        self.touch(t);
    }

    pub fn ingest(&mut self, ev: &Event) {
        let t = ev.ts;
        match &ev.kind {
            EventKind::Output { .. } => {
                self.last_output_at = Some(t);
                self.output.push(t, 1.0);
                self.touch(t);
            }
            EventKind::Lifecycle { state, .. } => {
                self.lifecycle = *state;
                if *state == Lifecycle::Running && self.started_at.is_none() {
                    self.started_at = Some(t);
                }
                if state.is_terminal() {
                    self.ended_at = Some(t);
                }
            }
            EventKind::StepBegin { id, name, parent } => {
                self.touch(t);
                match self.steps.iter_mut().find(|s| &s.id == id) {
                    Some(s) => {
                        s.running = true;
                        s.name = name.clone();
                    }
                    None => self.steps.push(StepInfo {
                        id: id.clone(),
                        name: name.clone(),
                        parent: parent.clone(),
                        running: true,
                        current: None,
                        total: None,
                        unit: None,
                        started_at: t,
                    }),
                }
            }
            EventKind::StepEnd { id, .. } => {
                self.touch(t);
                if let Some(s) = self.steps.iter_mut().find(|s| &s.id == id) {
                    s.running = false;
                }
            }
            EventKind::Progress {
                id,
                current,
                total,
                unit,
                ..
            } => {
                self.touch(t);
                let series = self.progress.entry(id.clone()).or_default();
                if series.last(t) != Some(*current) {
                    series.push(t, *current);
                }
                if let Some(s) = self.steps.iter_mut().find(|s| &s.id == id) {
                    s.current = Some(*current);
                    s.total = total.or(s.total);
                    s.unit = unit.clone().or(s.unit.take());
                }
            }
            EventKind::Metric { values, .. } => {
                self.touch(t);
                for (k, v) in values {
                    self.metrics.entry(k.clone()).or_default().push(t, v.0);
                }
            }
            EventKind::Log { .. } | EventKind::Note { .. } => self.touch(t),
            EventKind::Diagnosis(_) | EventKind::Alert { .. } | EventKind::CheckError { .. } => {}
        }
    }

    /// Seconds since the run last showed a sign of life (or since it started).
    pub fn silence(&self, now: Millis) -> Option<f64> {
        let last = self.last_activity_at.or(self.started_at)?;
        Some(((now - last).max(0)) as f64 / 1000.0)
    }

    pub fn elapsed(&self, now: Millis) -> f64 {
        self.started_at
            .map(|s| ((self.ended_at.unwrap_or(now) - s).max(0)) as f64 / 1000.0)
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(pts: &[(Millis, f64)]) -> Series {
        let mut s = Series::default();
        for (t, v) in pts {
            s.push(*t, *v);
        }
        s
    }

    #[test]
    fn aggregations_over_windows() {
        let x = s(&[(0, 1.0), (1000, 2.0), (2000, f64::NAN), (3000, 4.0)]);
        assert_eq!(x.avg(3000, None), Some(7.0 / 3.0));
        assert_eq!(x.avg(3000, Some(1.5)), Some(4.0)); // window [1500, 3000]: NaN, 4
        assert!(x.last(2500).unwrap().is_nan());
        assert_eq!(x.min(3000, None), Some(1.0));
        assert_eq!(x.max(3000, Some(10.0)), Some(4.0));
        assert_eq!(x.count_non_finite(3000, None), 1);
        assert_eq!(x.age(3500), Some(0.5));
        assert_eq!(Series::default().avg(0, None), None);
    }

    #[test]
    fn slope_rate_delta() {
        let x = s(&[(0, 10.0), (1000, 12.0), (2000, 14.0)]);
        assert!((x.slope(2000, None).unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(x.rate(2000, None), Some(2.0));
        assert_eq!(x.delta(2000, None), Some(4.0));
        assert_eq!(s(&[(0, 1.0)]).slope(0, None), None);
    }

    #[test]
    fn retention_drops_old_points() {
        let mut x = Series::default();
        x.push(0, 1.0);
        x.push(RETAIN_MS + 1, 2.0);
        assert_eq!(x.len(), 1);
    }
}

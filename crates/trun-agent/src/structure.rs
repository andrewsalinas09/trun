//! Applies [`Directive`]s to a run's derived structure (step tree, progress rates,
//! latest metrics) and decides which events to emit. Pure: no I/O, no clocks.
//!
//! Progress is the high-frequency case (a tqdm bar, one line per test), so it is
//! coalesced: state updates immediately, but a `Progress` event per step goes out
//! at most every [`PROGRESS_EMIT_MS`].

use std::collections::{BTreeMap, BTreeSet};
use trun_proto::{
    Directive, EventKind, Lifecycle, MetricLast, Millis, StepRunState, StepState, StepStatus,
};

pub const PROGRESS_EMIT_MS: Millis = 250;
/// Caps on what the run summary carries (events and stored metrics are not capped).
pub const MAX_STEPS: usize = 256;
pub const MAX_METRICS: usize = 200;
const IMPLICIT_STEP: &str = "progress";

#[derive(Debug, Default)]
pub struct Structure {
    pub steps: Vec<StepState>,
    pub metrics: BTreeMap<String, MetricLast>,
    pub expect_silence_ms: Option<Millis>,
    pub last_activity_at: Option<Millis>,
    /// Steps with progress not yet emitted, and when each step last emitted.
    dirty: BTreeSet<String>,
    last_emit: BTreeMap<String, Millis>,
    /// Previous (time, current) per step, for rates.
    last_point: BTreeMap<String, (Millis, f64)>,
}

impl Structure {
    fn find(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.id == id)
    }

    fn latest_running(&self) -> Option<usize> {
        self.steps
            .iter()
            .rposition(|s| s.state == StepRunState::Running)
    }

    /// Get or create a step. Returns its index, or `None` when over the cap.
    fn ensure(
        &mut self,
        id: &str,
        name: Option<&str>,
        parent: Option<&str>,
        now: Millis,
        out: &mut Vec<EventKind>,
    ) -> Option<usize> {
        if let Some(p) = parent
            && self.find(p).is_none()
        {
            self.ensure(p, None, None, now, out)?;
        }
        if let Some(i) = self.find(id) {
            let s = &mut self.steps[i];
            let mut changed = false;
            if let Some(n) = name
                && s.name != n
            {
                s.name = n.to_string();
                changed = true;
            }
            if s.state != StepRunState::Running {
                s.state = StepRunState::Running;
                s.ended_at = None;
                changed = true;
            }
            if changed {
                out.push(EventKind::StepBegin {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    parent: s.parent.clone(),
                });
            }
            return Some(i);
        }
        if self.steps.len() >= MAX_STEPS {
            return None;
        }
        let step = StepState {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            name: name.unwrap_or(id).to_string(),
            state: StepRunState::Running,
            current: None,
            total: None,
            unit: None,
            rate: None,
            eta_ms: None,
            started_at: now,
            ended_at: None,
            progressed_at: None,
        };
        out.push(EventKind::StepBegin {
            id: step.id.clone(),
            name: step.name.clone(),
            parent: step.parent.clone(),
        });
        self.steps.push(step);
        Some(self.steps.len() - 1)
    }

    /// End step `i` and its running descendants with `status`.
    fn end(&mut self, i: usize, status: StepStatus, now: Millis, out: &mut Vec<EventKind>) {
        let id = self.steps[i].id.clone();
        let children: Vec<usize> = self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.parent.as_deref() == Some(id.as_str()) && s.state == StepRunState::Running
            })
            .map(|(j, _)| j)
            .collect();
        for j in children {
            self.end(j, status, now, out);
        }
        let s = &mut self.steps[i];
        if s.state == StepRunState::Running {
            s.state = status.into();
            s.ended_at = Some(now);
            s.eta_ms = None;
            out.push(EventKind::StepEnd {
                id: s.id.clone(),
                status,
            });
        }
    }

    /// Apply one directive. Returns events to emit now (progress is deferred to
    /// [`Structure::flush_progress`]).
    pub fn apply(&mut self, d: Directive, now: Millis) -> Vec<EventKind> {
        let mut out = Vec::new();
        self.last_activity_at = Some(now);
        match d {
            Directive::StepBegin { id, name, parent } => {
                self.ensure(&id, name.as_deref(), parent.as_deref(), now, &mut out);
            }
            Directive::StepEnd { id, status } => {
                let i = match id {
                    Some(id) => self.find(&id),
                    None => self.latest_running(),
                };
                if let Some(i) = i {
                    self.flush_one(i, now, &mut out);
                    self.end(i, status, now, &mut out);
                }
            }
            Directive::Progress {
                id,
                current,
                total,
                unit,
            } => {
                let i = match id {
                    Some(id) => self.ensure(&id, None, None, now, &mut out),
                    None => self
                        .latest_running()
                        .or_else(|| self.ensure(IMPLICIT_STEP, None, None, now, &mut out)),
                };
                if let Some(i) = i {
                    self.progress(i, current, total, unit, now, &mut out);
                }
            }
            Directive::Metric { values, step } => {
                for (name, v) in &values {
                    if !self.metrics.contains_key(name) && self.metrics.len() >= MAX_METRICS {
                        continue;
                    }
                    let m = self.metrics.entry(name.clone()).or_insert(MetricLast {
                        value: *v,
                        step,
                        ts: now,
                        count: 0,
                        non_finite: 0,
                    });
                    m.value = *v;
                    m.step = step;
                    m.ts = now;
                    m.count += 1;
                    if !v.0.is_finite() {
                        m.non_finite += 1;
                    }
                }
                out.push(EventKind::Metric { values, step });
            }
            Directive::Log { level, text } => out.push(EventKind::Log { level, text }),
            Directive::Note { text } => out.push(EventKind::Note {
                text,
                author: "run".into(),
            }),
            Directive::Heartbeat => {}
            Directive::Expect { silence_ms } => self.expect_silence_ms = silence_ms,
        }
        out
    }

    fn progress(
        &mut self,
        i: usize,
        current: f64,
        total: Option<f64>,
        unit: Option<String>,
        now: Millis,
        out: &mut Vec<EventKind>,
    ) {
        let id = self.steps[i].id.clone();
        // A bar that restarts (new epoch reusing the same bar) or a finished step
        // that moves again is running again.
        let reopen =
            self.steps[i].state != StepRunState::Running && total.is_none_or(|t| current < t);
        if reopen {
            self.ensure(&id, None, None, now, out);
        }
        let prev = self.last_point.get(&id).copied();
        let s = &mut self.steps[i];
        if s.current != Some(current) {
            s.progressed_at = Some(now);
        }
        match prev {
            Some((t0, c0)) if current < c0 => {
                s.rate = None; // reset
                let _ = t0;
            }
            Some((t0, c0)) if now > t0 && current > c0 => {
                let inst = (current - c0) / ((now - t0) as f64 / 1000.0);
                s.rate = Some(match s.rate {
                    Some(r) => 0.3 * inst + 0.7 * r,
                    None => inst,
                });
            }
            _ => {}
        }
        if prev.is_none_or(|(_, c0)| c0 != current) {
            self.last_point.insert(id.clone(), (now, current));
        }
        s.current = Some(current);
        if total.is_some() {
            s.total = total;
        }
        if unit.is_some() {
            s.unit = unit;
        }
        s.eta_ms = match (s.total, s.rate) {
            (Some(t), Some(r)) if r > 0.0 && t > current => {
                Some(((t - current) / r * 1000.0) as Millis)
            }
            _ => None,
        };
        self.dirty.insert(id);
        if s.total.is_some_and(|t| t > 0.0 && current >= t) && s.state == StepRunState::Running {
            self.flush_one(i, now, out);
            self.end(i, StepStatus::Ok, now, out);
        }
    }

    fn flush_one(&mut self, i: usize, now: Millis, out: &mut Vec<EventKind>) {
        let s = &self.steps[i];
        if self.dirty.remove(&s.id) {
            self.last_emit.insert(s.id.clone(), now);
            out.push(progress_event(s));
        }
    }

    /// Emit coalesced progress for steps that haven't emitted recently.
    pub fn flush_progress(&mut self, now: Millis) -> Vec<EventKind> {
        let mut out = Vec::new();
        let due: Vec<String> = self
            .dirty
            .iter()
            .filter(|id| {
                self.last_emit
                    .get(*id)
                    .is_none_or(|t| now - t >= PROGRESS_EMIT_MS)
            })
            .cloned()
            .collect();
        for id in due {
            if let Some(i) = self.find(&id) {
                self.flush_one(i, now, &mut out);
            }
        }
        out
    }

    /// Close every running step when the run ends.
    pub fn finalize(&mut self, lifecycle: Lifecycle, now: Millis) -> Vec<EventKind> {
        let mut out = Vec::new();
        let dirty: Vec<String> = self.dirty.iter().cloned().collect();
        for id in dirty {
            if let Some(i) = self.find(&id) {
                self.flush_one(i, now, &mut out);
            }
        }
        let status = match lifecycle {
            Lifecycle::Succeeded => StepStatus::Ok,
            Lifecycle::Cancelled => StepStatus::Skipped,
            _ => StepStatus::Failed,
        };
        // Innermost (latest) first so parents don't pre-empt their children's status.
        for i in (0..self.steps.len()).rev() {
            if self.steps[i].state == StepRunState::Running {
                self.end(i, status, now, &mut out);
            }
        }
        out
    }
}

fn progress_event(s: &StepState) -> EventKind {
    EventKind::Progress {
        id: s.id.clone(),
        current: s.current.unwrap_or(0.0),
        total: s.total,
        unit: s.unit.clone(),
        rate: s.rate,
        eta_ms: s.eta_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trun_proto::Num;

    fn names(ev: &[EventKind]) -> Vec<String> {
        ev.iter()
            .map(|e| match e {
                EventKind::StepBegin { id, .. } => format!("begin:{id}"),
                EventKind::StepEnd { id, status } => format!("end:{id}:{status:?}"),
                EventKind::Progress { id, current, .. } => format!("p:{id}:{current}"),
                other => other.name().to_string(),
            })
            .collect()
    }

    fn prog(id: Option<&str>, c: f64, t: Option<f64>) -> Directive {
        Directive::Progress {
            id: id.map(Into::into),
            current: c,
            total: t,
            unit: None,
        }
    }

    #[test]
    fn nested_steps_end_children_with_parent() {
        let mut s = Structure::default();
        let mut ev = s.apply(
            Directive::StepBegin {
                id: "train".into(),
                name: Some("Train".into()),
                parent: None,
            },
            0,
        );
        ev.extend(s.apply(
            Directive::StepBegin {
                id: "e1".into(),
                name: None,
                parent: Some("train".into()),
            },
            1,
        ));
        ev.extend(s.apply(
            Directive::StepEnd {
                id: Some("train".into()),
                status: StepStatus::Failed,
            },
            2,
        ));
        assert_eq!(
            names(&ev),
            vec![
                "begin:train",
                "begin:e1",
                "end:e1:Failed",
                "end:train:Failed"
            ]
        );
        assert_eq!(s.steps[1].state, StepRunState::Failed);
    }

    #[test]
    fn implicit_parent_and_step() {
        let mut s = Structure::default();
        let ev = s.apply(
            Directive::StepBegin {
                id: "child".into(),
                name: None,
                parent: Some("root".into()),
            },
            0,
        );
        assert_eq!(names(&ev), vec!["begin:root", "begin:child"]);
        // Progress without id goes to the latest running step.
        s.apply(prog(None, 3.0, Some(10.0)), 10);
        assert_eq!(s.steps[1].current, Some(3.0));
        // With no running step, an implicit one is created.
        let mut s = Structure::default();
        let ev = s.apply(prog(None, 1.0, Some(4.0)), 0);
        assert_eq!(names(&ev), vec!["begin:progress"]);
    }

    #[test]
    fn progress_is_coalesced_and_rates_computed() {
        let mut s = Structure::default();
        s.apply(
            Directive::StepBegin {
                id: "t".into(),
                name: None,
                parent: None,
            },
            0,
        );
        let mut emitted = 0;
        for i in 1..=100 {
            let now = i * 10; // 100 updates over 1s
            s.apply(prog(Some("t"), i as f64, Some(1000.0)), now);
            emitted += s.flush_progress(now).len();
        }
        assert!(emitted <= 5, "coalesced to ~4/s, got {emitted}");
        let st = &s.steps[0];
        let rate = st.rate.unwrap();
        assert!((rate - 100.0).abs() < 1.0, "rate ≈ 100/s, got {rate}");
        let eta = st.eta_ms.unwrap();
        assert!((eta - 9000).abs() < 200, "eta ≈ 9s, got {eta}");
    }

    #[test]
    fn completion_auto_ends_and_restart_reopens() {
        let mut s = Structure::default();
        s.apply(prog(Some("bar"), 5.0, Some(10.0)), 0);
        let ev = s.apply(prog(Some("bar"), 10.0, Some(10.0)), 100);
        assert_eq!(names(&ev), vec!["p:bar:10", "end:bar:Ok"]);
        let ev = s.apply(prog(Some("bar"), 1.0, Some(10.0)), 200);
        assert_eq!(names(&ev), vec!["begin:bar"]);
        assert_eq!(s.steps[0].state, StepRunState::Running);
        assert_eq!(s.steps[0].rate, None, "rate resets when the bar restarts");
    }

    #[test]
    fn finalize_closes_open_steps_by_outcome() {
        let mut s = Structure::default();
        s.apply(
            Directive::StepBegin {
                id: "a".into(),
                name: None,
                parent: None,
            },
            0,
        );
        s.apply(
            Directive::StepBegin {
                id: "b".into(),
                name: None,
                parent: Some("a".into()),
            },
            0,
        );
        s.apply(prog(Some("b"), 2.0, None), 5);
        let ev = s.finalize(Lifecycle::Cancelled, 10);
        assert_eq!(names(&ev), vec!["p:b:2", "end:b:Skipped", "end:a:Skipped"]);
    }

    #[test]
    fn metrics_track_last_and_non_finite() {
        let mut s = Structure::default();
        let m = |v: f64| Directive::Metric {
            values: BTreeMap::from([("loss".to_string(), Num(v))]),
            step: Some(1),
        };
        s.apply(m(0.5), 0);
        s.apply(m(f64::NAN), 1);
        let last = &s.metrics["loss"];
        assert!(last.value.0.is_nan());
        assert_eq!((last.count, last.non_finite), (2, 1));
    }

    #[test]
    fn step_cap() {
        let mut s = Structure::default();
        for i in 0..(MAX_STEPS + 10) {
            s.apply(
                Directive::StepBegin {
                    id: format!("s{i}"),
                    name: None,
                    parent: None,
                },
                0,
            );
        }
        assert_eq!(s.steps.len(), MAX_STEPS);
    }
}

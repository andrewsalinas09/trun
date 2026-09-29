//! Alerts with hysteresis (docs/05-checks.md, "Alert lifecycle").
//!
//! Checks run repeatedly, so alerts are edge-triggered: the first firing opens an
//! alert (one event, one notification); while it keeps firing it stays open; it
//! clears after `clear_after` consecutive evaluations of its check that didn't fire.

use crate::engine::{Action, ActionKind};
use std::collections::BTreeMap;
use trun_proto::{Alert, AlertLevel, AlertState, Health, Millis};

#[derive(Debug, Clone, PartialEq)]
pub struct AlertChange {
    pub alert: Alert,
    pub state: AlertState,
}

#[derive(Debug, Default)]
pub struct AlertTracker {
    open: BTreeMap<String, (Alert, u32)>, // alert, consecutive misses
}

/// Identity of an alert: explicit key, or the message with numbers normalized so
/// "no output for 5m03s" and "no output for 5m13s" are the same alert.
pub fn alert_key(check: &str, action: &Action) -> String {
    let body = match &action.key {
        Some(k) => k.clone(),
        None => {
            let mut s = String::new();
            let mut in_num = false;
            for c in action.message.chars() {
                if c.is_ascii_digit() || (in_num && c == '.') {
                    if !in_num {
                        s.push('#');
                    }
                    in_num = true;
                } else {
                    in_num = false;
                    s.push(c);
                }
            }
            s
        }
    };
    format!("{check}:{body}")
}

fn level(kind: ActionKind) -> Option<AlertLevel> {
    Some(match kind {
        ActionKind::Info => AlertLevel::Info,
        ActionKind::Warn => AlertLevel::Warn,
        ActionKind::Stalled => AlertLevel::Stalled,
        ActionKind::Fail | ActionKind::Kill => AlertLevel::Fail,
        ActionKind::Notify => return None,
    })
}

impl AlertTracker {
    /// Record one evaluation of `check`: its actions open/update alerts; its open
    /// alerts that didn't fire move toward clearing.
    pub fn evaluate(
        &mut self,
        check: &str,
        clear_after: u32,
        actions: &[Action],
        now: Millis,
    ) -> Vec<AlertChange> {
        let mut changes = Vec::new();
        // Highest level per key wins within one evaluation.
        let mut fired: BTreeMap<String, (AlertLevel, &str)> = BTreeMap::new();
        for a in actions {
            let Some(l) = level(a.kind) else { continue };
            let key = alert_key(check, a);
            let e = fired.entry(key).or_insert((l, a.message.as_str()));
            if l > e.0 {
                *e = (l, a.message.as_str());
            }
        }
        for (key, (lvl, msg)) in &fired {
            match self.open.get_mut(key) {
                Some((alert, misses)) => {
                    *misses = 0;
                    alert.last_at = now;
                    alert.count += 1;
                    if alert.level != *lvl || alert.message != *msg {
                        let escalated = *lvl != alert.level;
                        alert.level = *lvl;
                        alert.message = msg.to_string();
                        // Level changes are reported; message-only changes (a counter
                        // ticking up) just refresh the stored text.
                        if escalated {
                            changes.push(AlertChange {
                                alert: alert.clone(),
                                state: AlertState::Updated,
                            });
                        }
                    }
                }
                None => {
                    let alert = Alert {
                        key: key.clone(),
                        check: check.to_string(),
                        level: *lvl,
                        message: msg.to_string(),
                        opened_at: now,
                        last_at: now,
                        count: 1,
                    };
                    changes.push(AlertChange {
                        alert: alert.clone(),
                        state: AlertState::Opened,
                    });
                    self.open.insert(key.clone(), (alert, 0));
                }
            }
        }
        let stale: Vec<String> = self
            .open
            .iter_mut()
            .filter(|(k, (a, _))| a.check == check && !fired.contains_key(*k))
            .filter_map(|(k, (_, misses))| {
                *misses += 1;
                (*misses >= clear_after).then(|| k.clone())
            })
            .collect();
        for k in stale {
            if let Some((a, _)) = self.open.remove(&k) {
                changes.push(AlertChange {
                    alert: a,
                    state: AlertState::Cleared,
                });
            }
        }
        changes
    }

    /// Drop every alert of a check (e.g. the check file was deleted).
    pub fn clear_check(&mut self, check: &str) -> Vec<AlertChange> {
        let keys: Vec<String> = self
            .open
            .iter()
            .filter(|(_, (a, _))| a.check == check)
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter()
            .filter_map(|k| self.open.remove(&k))
            .map(|(a, _)| AlertChange {
                alert: a,
                state: AlertState::Cleared,
            })
            .collect()
    }

    pub fn open_alerts(&self) -> Vec<Alert> {
        let mut v: Vec<Alert> = self.open.values().map(|(a, _)| a.clone()).collect();
        v.sort_by(|a, b| b.level.cmp(&a.level).then(a.opened_at.cmp(&b.opened_at)));
        v
    }

    /// Health from the worst open alert.
    pub fn health(&self) -> Health {
        self.open
            .values()
            .map(|(a, _)| a.level)
            .max()
            .map(AlertLevel::health)
            .unwrap_or(Health::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(kind: ActionKind, msg: &str) -> Action {
        Action {
            kind,
            message: msg.into(),
            key: None,
        }
    }

    #[test]
    fn open_hold_clear_with_hysteresis() {
        let mut t = AlertTracker::default();
        let c = t.evaluate(
            "defaults",
            3,
            &[act(ActionKind::Stalled, "no output for 5m03s")],
            0,
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].state, AlertState::Opened);
        assert_eq!(t.health(), Health::Stalled);
        // Same alert, different number: no new event.
        assert!(
            t.evaluate(
                "defaults",
                3,
                &[act(ActionKind::Stalled, "no output for 5m13s")],
                10
            )
            .is_empty()
        );
        assert_eq!(t.open_alerts()[0].message, "no output for 5m13s");
        // Two quiet evaluations: still open. Third: cleared.
        assert!(t.evaluate("defaults", 3, &[], 20).is_empty());
        assert!(t.evaluate("defaults", 3, &[], 30).is_empty());
        let c = t.evaluate("defaults", 3, &[], 40);
        assert_eq!(c[0].state, AlertState::Cleared);
        assert_eq!(t.health(), Health::Ok);
    }

    #[test]
    fn escalation_and_isolation_between_checks() {
        let mut t = AlertTracker::default();
        t.evaluate(
            "a",
            1,
            &[Action {
                kind: ActionKind::Warn,
                message: "x".into(),
                key: Some("k".into()),
            }],
            0,
        );
        let c = t.evaluate(
            "a",
            1,
            &[Action {
                kind: ActionKind::Fail,
                message: "x!".into(),
                key: Some("k".into()),
            }],
            1,
        );
        assert_eq!(c[0].state, AlertState::Updated);
        assert_eq!(t.health(), Health::Failing);
        // Another check's evaluation doesn't clear check a's alert.
        assert!(t.evaluate("b", 1, &[], 2).is_empty());
        assert_eq!(t.open_alerts().len(), 1);
        // Notify never creates an alert.
        assert!(
            t.evaluate("b", 1, &[act(ActionKind::Notify, "hi")], 3)
                .is_empty()
        );
    }
}

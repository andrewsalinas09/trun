//! Structure a run reports about itself: steps, progress, metrics, logs, notes
//! (docs/04-progress-protocol.md).
//!
//! [`Directive`] is what a program *says* (via `::` lines, the side channel, or a
//! built-in parser). The agent turns directives into sequenced [`crate::EventKind`]s
//! and derived state ([`StepState`], [`MetricLast`]) on the run summary.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

use crate::Millis;

/// An f64 that survives JSON even when it is NaN or ±inf (JSON has no literal for
/// them). Finite values are plain numbers; the rest are the strings `"NaN"`,
/// `"inf"`, `"-inf"`. Detecting NaN is the point of metrics, so it must round-trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Num(pub f64);

impl Serialize for Num {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        if v.is_finite() {
            s.serialize_f64(v)
        } else if v.is_nan() {
            s.serialize_str("NaN")
        } else if v > 0.0 {
            s.serialize_str("inf")
        } else {
            s.serialize_str("-inf")
        }
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(f64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(v) => Ok(Num(v)),
            Raw::S(s) => parse_num(&s)
                .map(Num)
                .ok_or_else(|| serde::de::Error::custom(format!("not a number: {s:?}"))),
        }
    }
}

/// Parse a metric value, accepting `nan`, `inf`, `-inf`, `+inf` (any case).
pub fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "nan" | "-nan" => Some(f64::NAN),
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        _ => t.parse().ok(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Ok,
    Failed,
    Skipped,
}

impl StepStatus {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "ok" | "success" | "succeeded" | "pass" | "passed" | "done" => StepStatus::Ok,
            "failed" | "fail" | "error" => StepStatus::Failed,
            "skipped" | "skip" => StepStatus::Skipped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Something a program reports about its own structure. Also the JSON schema of
/// the `TRUN_EVENTS` side channel (one object per line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Directive {
    StepBegin {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        parent: Option<String>,
    },
    StepEnd {
        /// `None`: the most recently begun open step.
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        status: StepStatus,
    },
    Progress {
        /// `None`: the most recently begun open step (or an implicit one).
        #[serde(default)]
        id: Option<String>,
        current: f64,
        #[serde(default)]
        total: Option<f64>,
        #[serde(default)]
        unit: Option<String>,
    },
    Metric {
        values: BTreeMap<String, Num>,
        #[serde(default)]
        step: Option<i64>,
    },
    Log {
        level: LogLevel,
        text: String,
    },
    Note {
        text: String,
    },
    /// "I'm alive" without producing output.
    Heartbeat,
    /// Tell the silence check this phase is legitimately quiet (`None` = default).
    Expect {
        #[serde(default)]
        silence_ms: Option<i64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRunState {
    Running,
    Ok,
    Failed,
    Skipped,
}

impl From<StepStatus> for StepRunState {
    fn from(s: StepStatus) -> Self {
        match s {
            StepStatus::Ok => StepRunState::Ok,
            StepStatus::Failed => StepRunState::Failed,
            StepStatus::Skipped => StepRunState::Skipped,
        }
    }
}

/// Derived state of one step, kept on the run summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepState {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub name: String,
    pub state: StepRunState,
    #[serde(default)]
    pub current: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    /// Smoothed progress rate, units per second.
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub eta_ms: Option<Millis>,
    pub started_at: Millis,
    #[serde(default)]
    pub ended_at: Option<Millis>,
    /// Last time `current` changed.
    #[serde(default)]
    pub progressed_at: Option<Millis>,
}

/// Latest value of a metric, kept on the run summary for digests and lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricLast {
    pub value: Num,
    #[serde(default)]
    pub step: Option<i64>,
    pub ts: Millis,
    pub count: u64,
    /// Number of NaN/inf values seen.
    #[serde(default)]
    pub non_finite: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_round_trips_non_finite() {
        for v in [1.5, f64::INFINITY, f64::NEG_INFINITY] {
            let j = serde_json::to_string(&Num(v)).unwrap();
            assert_eq!(serde_json::from_str::<Num>(&j).unwrap(), Num(v));
        }
        let j = serde_json::to_string(&Num(f64::NAN)).unwrap();
        assert_eq!(j, "\"NaN\"");
        assert!(serde_json::from_str::<Num>(&j).unwrap().0.is_nan());
    }

    #[test]
    fn directive_json_schema() {
        let d: Directive =
            serde_json::from_str(r#"{"kind":"metric","values":{"loss":0.3,"g":"nan"},"step":12}"#)
                .unwrap();
        match d {
            Directive::Metric { values, step } => {
                assert_eq!(step, Some(12));
                assert!(values["g"].0.is_nan());
            }
            other => panic!("{other:?}"),
        }
        let d: Directive =
            serde_json::from_str(r#"{"kind":"progress","id":"train","current":3,"total":10}"#)
                .unwrap();
        assert!(matches!(d, Directive::Progress { total: Some(t), .. } if t == 10.0));
        let d: Directive = serde_json::from_str(r#"{"kind":"heartbeat"}"#).unwrap();
        assert_eq!(d, Directive::Heartbeat);
    }
}

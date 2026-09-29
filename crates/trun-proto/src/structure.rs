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
    // An explicit visitor rather than `#[serde(untagged)]`: some dependencies (e.g.
    // starlark) enable serde_json's `arbitrary_precision`, which delivers numbers to
    // untagged enums as a private map and breaks them. The map form is handled here.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Num;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a number or one of \"NaN\", \"inf\", \"-inf\"")
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Num, E> {
                parse_num(s)
                    .map(Num)
                    .ok_or_else(|| E::custom(format!("not a number: {s:?}")))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Num, A::Error> {
                // serde_json arbitrary_precision: {"$serde_json::private::Number": "1.5"}
                let (_k, v): (String, String) = m
                    .next_entry()?
                    .ok_or_else(|| serde::de::Error::custom("empty map for number"))?;
                self.visit_str(&v)
            }
        }
        d.deserialize_any(V)
    }
}

/// Float field deserializers that survive serde_json's `arbitrary_precision`.
///
/// starlark (used by trun-checks) enables that feature, and Cargo unifies it across
/// the build. Under it, floats inside *buffered* serde paths (internally tagged enums,
/// `#[serde(flatten)]`, untagged) arrive as a private map and a plain `f64` field fails
/// with "invalid type: map, expected f64". Every float field that can travel through
/// such a path uses these (D25).
pub mod flex {
    use super::Num;
    use serde::{Deserialize, Deserializer};

    pub fn f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Num::deserialize(d).map(|n| n.0)
    }

    pub fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Option::<Num>::deserialize(d).map(|o| o.map(|n| n.0))
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
        #[serde(deserialize_with = "crate::structure::flex::f64")]
        current: f64,
        #[serde(deserialize_with = "crate::structure::flex::opt_f64", default)]
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
    #[serde(deserialize_with = "crate::structure::flex::opt_f64", default)]
    pub current: Option<f64>,
    #[serde(deserialize_with = "crate::structure::flex::opt_f64", default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    /// Smoothed progress rate, units per second.
    #[serde(deserialize_with = "crate::structure::flex::opt_f64", default)]
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

/// Severity of a check alert. Ordered: the worst open alert sets the run's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Info,
    Warn,
    Stalled,
    Fail,
}

impl AlertLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertLevel::Info => "info",
            AlertLevel::Warn => "warn",
            AlertLevel::Stalled => "stalled",
            AlertLevel::Fail => "fail",
        }
    }

    pub fn health(self) -> crate::Health {
        match self {
            AlertLevel::Info => crate::Health::Ok,
            AlertLevel::Warn => crate::Health::Warn,
            AlertLevel::Stalled => crate::Health::Stalled,
            AlertLevel::Fail => crate::Health::Failing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    Opened,
    /// Still firing, but its level or message changed.
    Updated,
    Cleared,
}

/// An open alert, as kept on the run summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    /// Stable identity: check name plus the message with digits normalized (or an
    /// explicit `key=`), so an alert with a changing number stays one alert.
    pub key: String,
    /// Check file name (stem), e.g. `defaults`, `training`.
    pub check: String,
    pub level: AlertLevel,
    pub message: String,
    pub opened_at: Millis,
    pub last_at: Millis,
    /// Evaluations that fired it.
    pub count: u64,
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
    fn num_inside_structs_and_integers() {
        // Regression: must work when serde_json's arbitrary_precision is enabled by
        // another crate in the build (starlark does this).
        let m: MetricLast =
            serde_json::from_str(r#"{"value":3.0,"step":null,"ts":1,"count":2,"non_finite":0}"#)
                .unwrap();
        assert_eq!(m.value, Num(3.0));
        let m: MetricLast = serde_json::from_str(r#"{"value":7,"ts":1,"count":1}"#).unwrap();
        assert_eq!(m.value, Num(7.0));
        let v: Vec<Num> = serde_json::from_str(r#"[1, -2, 0.5, "inf"]"#).unwrap();
        assert_eq!(v, vec![Num(1.0), Num(-2.0), Num(0.5), Num(f64::INFINITY)]);
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

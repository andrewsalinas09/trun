//! Regression (D25): starlark enables serde_json's `arbitrary_precision`, which
//! breaks floats inside buffered serde paths (tagged enums, flatten). This test
//! lives in trun-checks so it always builds with the feature on.
use trun_proto::{Directive, Event, EventKind, FleetEvent, RunSummary, StepRunState, StepState};

#[test]
fn events_with_floats_decode() {
    let e: Event = serde_json::from_str(r#"{"run_id":"R","seq":5,"ts":10,"kind":"progress","id":"t","current":3.5,"total":10.0,"rate":1.25}"#).unwrap();
    match e.kind {
        EventKind::Progress {
            current,
            total,
            rate,
            ..
        } => assert_eq!((current, total, rate), (3.5, Some(10.0), Some(1.25))),
        other => panic!("{other:?}"),
    }
    let e: Event = serde_json::from_str(r#"{"run_id":"R","seq":7,"ts":12,"kind":"metric","values":{"loss":0.5,"g":"NaN"},"step":3}"#).unwrap();
    assert!(matches!(e.kind, EventKind::Metric { .. }));
}

#[test]
fn directives_with_floats_decode() {
    let d: Directive =
        serde_json::from_str(r#"{"kind":"progress","id":"t","current":0.42,"total":1.0}"#).unwrap();
    assert_eq!(
        d,
        Directive::Progress {
            id: Some("t".into()),
            current: 0.42,
            total: Some(1.0),
            unit: None
        }
    );
}

#[test]
fn fleet_event_with_steps_round_trips() {
    let run = RunSummary {
        id: "R".into(),
        steps: vec![StepState {
            id: "s".into(),
            parent: None,
            name: "S".into(),
            state: StepRunState::Running,
            current: Some(2.5),
            total: Some(7.0),
            unit: None,
            rate: Some(0.75),
            eta_ms: Some(1000),
            started_at: 0,
            ended_at: None,
            progressed_at: None,
        }],
        ..Default::default()
    };
    let json = serde_json::to_string(&FleetEvent::Run(run.clone())).unwrap();
    let FleetEvent::Run(back) = serde_json::from_str::<FleetEvent>(&json).unwrap();
    assert_eq!(back.steps[0].current, Some(2.5));
    assert_eq!(back.steps[0].rate, Some(0.75));
}

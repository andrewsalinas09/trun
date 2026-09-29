use super::*;
use trun_proto::{Diagnosis, Health};

fn tempdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "trun-store-test-{}-{}",
        std::process::id(),
        ulid_like()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ulid_like() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn run(id: &str, project: &str, name: &str, lc: Lifecycle) -> RunSummary {
    RunSummary {
        id: id.into(),
        project: project.into(),
        name: name.into(),
        host: "local".into(),
        cmd: vec!["echo".into()],
        cwd: ".".into(),
        lifecycle: lc,
        health: Health::Ok,
        created_at: now_ms(),
        ..Default::default()
    }
}

fn out(run_id: &str, seq: u64, text: &str) -> Event {
    Event {
        run_id: run_id.into(),
        seq,
        ts: now_ms(),
        kind: EventKind::Output {
            stream: Stream::Stdout,
            text: text.into(),
            cr: false,
        },
    }
}

#[test]
fn round_trip_runs_logs_events() {
    let store = Store::open(tempdir()).unwrap();
    let mut r = run("01ABC", "detector", "train", Lifecycle::Running);
    store.upsert_run(&r).unwrap();
    store
        .append_events(
            "detector",
            vec![
                out("01ABC", 1, "hello"),
                out("01ABC", 2, "error: boom"),
                Event {
                    run_id: "01ABC".into(),
                    seq: 3,
                    ts: now_ms(),
                    kind: EventKind::Diagnosis(Diagnosis {
                        cause: "unknown".into(),
                        summary: "x".into(),
                        early: false,
                        evidence: vec![],
                    }),
                },
            ],
        )
        .unwrap();
    r.lifecycle = Lifecycle::Failed;
    store.upsert_run(&r).unwrap();
    store.flush();

    let got = store.get_run("01ABC").unwrap().unwrap();
    assert_eq!(got.lifecycle, Lifecycle::Failed);

    let logs = store
        .logs("detector", "01ABC", &LogQuery::default())
        .unwrap();
    assert_eq!(logs.len(), 2);
    let grep = LogQuery {
        grep: Some(Regex::new("boom").unwrap()),
        ..Default::default()
    };
    assert_eq!(store.logs("detector", "01ABC", &grep).unwrap().len(), 1);
    let tail = LogQuery {
        tail: Some(1),
        ..Default::default()
    };
    assert_eq!(store.logs("detector", "01ABC", &tail).unwrap()[0].seq, 2);

    let events = store.events("detector", "01ABC", 0, None, None).unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(events[2].kind, EventKind::Diagnosis(_)));
    assert_eq!(
        store
            .events("detector", "01ABC", 1, Some(3), None)
            .unwrap()
            .len(),
        1
    );

    let projects = store.projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "detector");
    assert_eq!(projects[0].run_count, 1);
}

#[test]
fn resolve_by_id_prefix_and_name() {
    let store = Store::open(tempdir()).unwrap();
    store
        .upsert_run(&run("01AAA111", "p", "train", Lifecycle::Succeeded))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    store
        .upsert_run(&run("01BBB222", "p", "train", Lifecycle::Running))
        .unwrap();
    store.flush();
    assert_eq!(
        store.resolve_run("01AAA111").unwrap().unwrap().id,
        "01AAA111"
    );
    assert_eq!(store.resolve_run("01bbb").unwrap().unwrap().id, "01BBB222");
    // Ambiguous prefix falls through to name lookup, which fails.
    assert!(store.resolve_run("01").unwrap().is_none());
    assert_eq!(store.resolve_run("train").unwrap().unwrap().id, "01BBB222");
}

#[test]
fn orphans_become_lost() {
    let dir = tempdir();
    {
        let store = Store::open(&dir).unwrap();
        store
            .upsert_run(&run("01RUN", "p", "x", Lifecycle::Running))
            .unwrap();
        store
            .upsert_run(&run("01DONE", "p", "y", Lifecycle::Succeeded))
            .unwrap();
        store.flush();
    }
    let store = Store::open(&dir).unwrap();
    let lost = store.mark_orphans_lost().unwrap();
    assert_eq!(lost.len(), 1);
    assert_eq!(
        store.get_run("01RUN").unwrap().unwrap().lifecycle,
        Lifecycle::Lost
    );
    assert_eq!(
        store.get_run("01DONE").unwrap().unwrap().lifecycle,
        Lifecycle::Succeeded
    );
}

#[test]
fn metric_series_keep_nan_and_downsample() {
    use std::collections::BTreeMap;
    use trun_proto::Num;
    let store = Store::open(tempdir()).unwrap();
    store
        .upsert_run(&run("01MET", "p", "m", Lifecycle::Running))
        .unwrap();
    let mut events = Vec::new();
    for i in 0..1000u64 {
        let v = if i == 500 {
            f64::NAN
        } else if i == 700 {
            1e9
        } else {
            i as f64
        };
        events.push(Event {
            run_id: "01MET".into(),
            seq: i + 1,
            ts: i as i64,
            kind: EventKind::Metric {
                values: BTreeMap::from([("loss".to_string(), Num(v))]),
                step: Some(i as i64),
            },
        });
    }
    store.append_events("p", events).unwrap();
    store.flush();

    let full = store
        .metric_series("p", "01MET", &["loss".into()], 10_000)
        .unwrap();
    assert_eq!(full[0].points.len(), 1000);
    assert!(!full[0].downsampled);
    assert!(full[0].points[500].value.0.is_nan());

    let small = store
        .metric_series("p", "01MET", &["loss".into()], 100)
        .unwrap();
    let s = &small[0];
    assert!(s.downsampled);
    assert_eq!(s.total, 1000);
    assert!(s.points.len() <= 110, "{}", s.points.len());
    assert!(
        s.points.iter().any(|p| p.value.0.is_nan()),
        "NaN must survive downsampling"
    );
    assert!(
        s.points.iter().any(|p| p.value.0 == 1e9),
        "spikes must survive downsampling"
    );
    assert!(
        s.points.windows(2).all(|w| w[0].ts <= w[1].ts),
        "order preserved"
    );

    // Metric events also replay through the event log.
    assert_eq!(
        store.events("p", "01MET", 0, None, Some(5)).unwrap().len(),
        5
    );
}

#[test]
fn file_stems_are_safe_and_distinct() {
    assert_eq!(file_stem("detector"), "detector");
    assert_eq!(file_stem("a/b"), "a%2Fb");
    assert_ne!(file_stem("a/b"), file_stem("a_b"));
    assert_eq!(file_stem(".."), "_..");
    assert_eq!(file_stem(""), "_");
}
